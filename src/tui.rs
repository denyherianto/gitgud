use std::{
    collections::BTreeMap,
    io::{self, Stdout},
    sync::mpsc::{self, Receiver, TryRecvError},
    time::Duration,
};

use anyhow::{Result, anyhow, bail};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use tui_textarea::{Input, Key, TextArea};

use crate::{
    ai::{
        AiClient, AskSuggestion, CommitSuggestions, PromptInput, SplitCommitPlan,
        build_heuristic_commit_suggestions, fetch_model_options, normalize_base_api_url,
        validate_commit_message_with_preset,
    },
    config::{CommitStyle, GenerationMode, Provider, ResolvedConventionalPreset, TokenStatus},
    git::{GitRepo, PushPlan, RepoStatus, UnsafeDiffWarning},
    rescue::{RescueContext, RescueIncident, RescuePlan},
    risk::{self, RiskLevel},
};

type Backend = CrosstermBackend<Stdout>;
type AppTerminal = Terminal<Backend>;

pub enum HomeAction {
    Ship,
    Commit,
    Push,
    Rescue,
    Quit,
}

pub enum CommitAction {
    Confirmed(String),
    SplitRequested(Vec<SplitCommitPlan>),
    Cancelled,
}

#[derive(Debug, Clone)]
pub enum CommitGenerator {
    Ai(AiClient),
    HeuristicOnly,
}

pub enum AskAction {
    RunRecommended,
    RunAlternative,
    Cancel,
}

pub enum RescuePlanAction {
    Recommended,
    Alternative(usize),
    Cancel,
}

#[derive(Debug, Clone)]
pub struct ConfigSetupInput {
    pub provider: Provider,
    pub base_api_url: String,
    pub base_model: String,
    pub commit_style: CommitStyle,
    pub generation_mode: GenerationMode,
    pub existing_api_token: Option<String>,
    pub token_status: TokenStatus,
    pub token_present: bool,
}

#[derive(Debug, Clone)]
pub struct ConfigSetupAction {
    pub provider: Provider,
    pub base_api_url: String,
    pub base_model: String,
    pub commit_style: CommitStyle,
    pub generation_mode: GenerationMode,
    pub api_token: Option<String>,
}

pub fn run_home(status: &RepoStatus) -> Result<HomeAction> {
    with_terminal(|terminal| {
        loop {
            terminal.draw(|frame| draw_home(frame, status))?;

            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('s') => return Ok(HomeAction::Ship),
                    KeyCode::Char('c') => return Ok(HomeAction::Commit),
                    KeyCode::Char('p') => return Ok(HomeAction::Push),
                    KeyCode::Char('r') => return Ok(HomeAction::Rescue),
                    KeyCode::Esc | KeyCode::Char('q') => return Ok(HomeAction::Quit),
                    _ => {}
                }
            }
        }
    })
}

pub async fn run_commit(
    repo: &GitRepo,
    generator: CommitGenerator,
    commit_style: CommitStyle,
    generation_mode: GenerationMode,
    conventional_preset: ResolvedConventionalPreset,
    memory: Option<crate::memory::RepoMemory>,
    git_memory_context: Vec<String>,
) -> Result<CommitAction> {
    let branch = repo.current_branch().or_else(|error| {
        show_message("Cannot Commit", &error.to_string())?;
        Err(error)
    })?;
    let staged = repo.staged_changes()?;

    if staged.staged_files.is_empty() {
        let message = "No staged changes found. Stage files before generating commit messages.";
        show_message("Cannot Commit", message)?;
        bail!(message);
    }

    let input = PromptInput {
        branch,
        staged_files: staged.staged_files.clone(),
        diff_stat: staged.diff_stat,
        diff: staged.diff,
        commit_style,
        conventional_preset: conventional_preset.clone(),
        repo_memory: memory,
        git_memory_context,
    };

    let mut state = CommitView::new(
        input.staged_files.clone(),
        commit_style,
        generation_mode,
        conventional_preset,
    );
    state.start_generation(generator, input);
    with_terminal(|terminal| commit_loop(terminal, &mut state))
}

pub fn run_config_setup(input: ConfigSetupInput) -> Result<Option<ConfigSetupAction>> {
    let mut state = ConfigSetupView::new(input);
    with_terminal(|terminal| config_setup_loop(terminal, &mut state))
}

pub fn confirm_force_push(plan: &PushPlan, error_message: &str) -> Result<bool> {
    with_terminal(|terminal| force_push_loop(terminal, plan, error_message))
}

pub fn confirm_stage_all_changes() -> Result<bool> {
    with_terminal(stage_all_changes_loop)
}

pub fn confirm_split_commits(plans: &[SplitCommitPlan]) -> Result<bool> {
    with_terminal(|terminal| split_commit_confirm_loop(terminal, plans))
}

pub fn confirm_unsafe_diff_warnings(
    action_label: &str,
    warnings: &[UnsafeDiffWarning],
) -> Result<bool> {
    with_terminal(|terminal| unsafe_diff_warning_loop(terminal, action_label, warnings))
}

pub fn show_message(title: &str, message: &str) -> Result<()> {
    with_terminal(|terminal| {
        loop {
            terminal.draw(|frame| draw_message(frame, title, message))?;

            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
                    _ => {}
                }
            }
        }
    })
}

pub fn confirm_message(title: &str, message: &str) -> Result<bool> {
    with_terminal(|terminal| {
        loop {
            terminal.draw(|frame| draw_message(frame, title, message))?;

            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Enter => return Ok(true),
                    KeyCode::Esc | KeyCode::Char('q') => return Ok(false),
                    _ => {}
                }
            }
        }
    })
}

pub fn prompt_input(title: &str, prompt: &str, initial: Option<&str>) -> Result<Option<String>> {
    let mut textarea = TextArea::default();
    textarea.set_block(Block::default().borders(Borders::ALL).title(title));
    textarea.set_cursor_line_style(Style::default());
    textarea.insert_str(initial.unwrap_or(""));

    with_terminal(|terminal| {
        loop {
            terminal.draw(|frame| draw_prompt_input(frame, &textarea, title, prompt))?;

            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Enter if key.modifiers.is_empty() => {
                        let value = textarea.lines().join("\n").trim().to_string();
                        return Ok(Some(value));
                    }
                    KeyCode::Esc => return Ok(None),
                    _ => {
                        textarea.input(key_event_to_textarea_input(key));
                    }
                }
            }
        }
    })
}

pub fn run_ask(suggestion: &AskSuggestion, risk_levels: &[RiskLevel]) -> Result<AskAction> {
    with_terminal(|terminal| ask_loop(terminal, suggestion, risk_levels))
}

pub fn run_rescue_diagnosis(
    context: &RescueContext,
    suggested: RescueIncident,
) -> Result<Option<RescueIncident>> {
    with_terminal(|terminal| rescue_diagnosis_loop(terminal, context, suggested))
}

pub fn run_rescue_plan(plan: &RescuePlan) -> Result<RescuePlanAction> {
    with_terminal(|terminal| rescue_plan_loop(terminal, plan))
}

pub fn confirm_dangerous_command(command: &str, description: &str) -> Result<bool> {
    with_terminal(|terminal| dangerous_command_loop(terminal, command, description))
}

fn ask_loop(
    terminal: &mut AppTerminal,
    suggestion: &AskSuggestion,
    risk_levels: &[RiskLevel],
) -> Result<AskAction> {
    let has_alt = suggestion.alternative.is_some();
    loop {
        terminal.draw(|frame| draw_ask(frame, suggestion, risk_levels))?;

        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Enter => return Ok(AskAction::RunRecommended),
                KeyCode::Char('2') if has_alt => return Ok(AskAction::RunAlternative),
                KeyCode::Esc | KeyCode::Char('q') => return Ok(AskAction::Cancel),
                _ => {}
            }
        }
    }
}

fn dangerous_command_loop(
    terminal: &mut AppTerminal,
    command: &str,
    description: &str,
) -> Result<bool> {
    loop {
        terminal.draw(|frame| draw_dangerous_command_confirm(frame, command, description))?;

        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Enter => return Ok(true),
                KeyCode::Esc | KeyCode::Char('q') => return Ok(false),
                _ => {}
            }
        }
    }
}

fn rescue_diagnosis_loop(
    terminal: &mut AppTerminal,
    context: &RescueContext,
    suggested: RescueIncident,
) -> Result<Option<RescueIncident>> {
    let incidents = RescueIncident::all();
    let mut selected = incidents
        .iter()
        .position(|incident| *incident == suggested)
        .unwrap_or(0);

    loop {
        terminal.draw(|frame| draw_rescue_diagnosis(frame, context, selected))?;

        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Up => {
                    selected = selected.saturating_sub(1);
                }
                KeyCode::Down => {
                    selected = (selected + 1).min(incidents.len().saturating_sub(1));
                }
                KeyCode::Enter => return Ok(Some(incidents[selected])),
                KeyCode::Esc | KeyCode::Char('q') => return Ok(None),
                _ => {}
            }
        }
    }
}

fn rescue_plan_loop(terminal: &mut AppTerminal, plan: &RescuePlan) -> Result<RescuePlanAction> {
    let option_count = 1 + plan.alternatives.len();
    let mut selected = 0usize;

    loop {
        terminal.draw(|frame| draw_rescue_plan(frame, plan, selected))?;

        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Up => {
                    selected = selected.saturating_sub(1);
                }
                KeyCode::Down => {
                    selected = (selected + 1).min(option_count.saturating_sub(1));
                }
                KeyCode::Enter => {
                    if selected == 0 {
                        return Ok(RescuePlanAction::Recommended);
                    }
                    return Ok(RescuePlanAction::Alternative(selected - 1));
                }
                KeyCode::Esc | KeyCode::Char('q') => return Ok(RescuePlanAction::Cancel),
                _ => {}
            }
        }
    }
}

fn draw_ask(frame: &mut ratatui::Frame<'_>, suggestion: &AskSuggestion, risk_levels: &[RiskLevel]) {
    let area = frame.area();

    // Build recommended lines
    let mut rec_lines: Vec<Line<'static>> = Vec::new();
    for (i, cmd) in suggestion.recommended.iter().enumerate() {
        let risk = risk_levels.get(i).copied().unwrap_or(RiskLevel::Safe);
        let (badge_text, badge_color) = risk_badge(risk);
        rec_lines.push(Line::from(vec![
            Span::styled(
                badge_text,
                Style::default()
                    .fg(badge_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(" {}", cmd.command)),
        ]));
        rec_lines.push(Line::from(vec![
            Span::raw("       "),
            Span::styled(
                cmd.description.clone(),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }
    let rec_height = rec_lines.len() as u16 + 2;

    // Build alternative lines
    let mut alt_lines: Vec<Line<'static>> = Vec::new();
    if let Some(alt) = &suggestion.alternative {
        for cmd in alt {
            let risk = risk::classify_risk(&cmd.command);
            let (badge_text, badge_color) = risk_badge(risk);
            alt_lines.push(Line::from(vec![
                Span::styled(
                    badge_text,
                    Style::default()
                        .fg(badge_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!(" {}", cmd.command)),
            ]));
            alt_lines.push(Line::from(vec![
                Span::raw("       "),
                Span::styled(
                    cmd.description.clone(),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
    }

    let has_alt = !alt_lines.is_empty();
    let alt_height = if has_alt {
        alt_lines.len() as u16 + 2
    } else {
        0
    };

    let key_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let mut help_spans: Vec<Span<'static>> = vec![
        Span::styled("Enter", key_style),
        Span::raw(" run recommended  "),
    ];
    if has_alt {
        help_spans.push(Span::styled("2", key_style));
        help_spans.push(Span::raw(" run alternative  "));
    }
    help_spans.push(Span::styled("Esc", key_style));
    help_spans.push(Span::raw("/"));
    help_spans.push(Span::styled("q", key_style));
    help_spans.push(Span::raw(" cancel"));

    let explain_lines: Vec<Line<'static>> = vec![
        Line::from(vec![
            Span::styled(
                "Explanation: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(suggestion.explanation.clone()),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "Teaching note: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(suggestion.teaching_note.clone()),
        ]),
    ];

    let constraints: Vec<Constraint> = if has_alt {
        vec![
            Constraint::Length(rec_height),
            Constraint::Length(alt_height),
            Constraint::Min(6),
            Constraint::Length(3),
        ]
    } else {
        vec![
            Constraint::Length(rec_height),
            Constraint::Min(6),
            Constraint::Length(3),
        ]
    };

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let rec_block = Paragraph::new(rec_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Recommended Commands"),
        )
        .wrap(Wrap { trim: false });

    let explain_block = Paragraph::new(explain_lines)
        .block(Block::default().borders(Borders::ALL).title("Details"))
        .wrap(Wrap { trim: false });

    let help = Paragraph::new(Line::from(help_spans))
        .block(Block::default().borders(Borders::ALL).title("Keys"))
        .alignment(Alignment::Center);

    frame.render_widget(rec_block, layout[0]);

    if has_alt {
        let alt_block = Paragraph::new(alt_lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Alternative  [2]"),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(alt_block, layout[1]);
        frame.render_widget(explain_block, layout[2]);
        frame.render_widget(help, layout[3]);
    } else {
        frame.render_widget(explain_block, layout[1]);
        frame.render_widget(help, layout[2]);
    }
}

fn draw_rescue_diagnosis(frame: &mut ratatui::Frame<'_>, context: &RescueContext, selected: usize) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Min(10),
            Constraint::Length(3),
        ])
        .split(frame.area());
    let content = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(layout[1]);

    let summary = Paragraph::new(vec![
        Line::from(format!(
            "Branch: {}",
            context.branch.as_deref().unwrap_or("DETACHED")
        )),
        Line::from(format!("HEAD: {}", context.head_sha)),
        Line::from(format!(
            "Upstream: {}",
            context.upstream.as_deref().unwrap_or("(none)")
        )),
        Line::from(format!(
            "Stash candidates: live {} / dropped {}",
            context.stash_entries.len(),
            context.lost_stash_candidates.len()
        )),
        Line::from(format!(
            "Rebase in progress: {}",
            if context.has_rebase_in_progress {
                "yes"
            } else {
                "no"
            }
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Rescue Diagnosis"),
    )
    .wrap(Wrap { trim: false });

    let mut items = Vec::new();
    for entry in &context.incident_matches {
        items.push(ListItem::new(format!(
            "{}  [{}]",
            entry.incident.title(),
            entry.score
        )));
    }

    let incidents = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Incident Picker"),
        )
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan))
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select(Some(selected));

    let detail = context
        .incident_matches
        .get(selected)
        .map(|entry| {
            let mut lines = vec![Line::from(entry.summary.clone()), Line::from("")];
            for evidence in &entry.evidence {
                lines.push(Line::from(format!("- {evidence}")));
            }
            lines
        })
        .unwrap_or_else(|| vec![Line::from("(no details)")]);
    let detail = Paragraph::new(detail)
        .block(Block::default().borders(Borders::ALL).title("Evidence"))
        .wrap(Wrap { trim: false });

    let help = Paragraph::new("Up/Down choose incident  Enter open recovery plan  Esc cancel")
        .block(Block::default().borders(Borders::ALL).title("Keys"))
        .alignment(Alignment::Center);

    frame.render_widget(summary, layout[0]);
    frame.render_stateful_widget(incidents, content[0], &mut state);
    frame.render_widget(detail, content[1]);
    frame.render_widget(help, layout[2]);
}

fn draw_rescue_plan(frame: &mut ratatui::Frame<'_>, plan: &RescuePlan, selected: usize) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Min(12),
            Constraint::Length(3),
        ])
        .split(frame.area());
    let content = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(layout[1]);

    let mut header_lines = vec![Line::from(plan.summary.clone())];
    for finding in &plan.findings {
        header_lines.push(Line::from(format!("- {finding}")));
    }
    if let Some(note) = &plan.note {
        header_lines.push(Line::from(""));
        header_lines.push(Line::from(vec![Span::styled(
            note.clone(),
            Style::default().fg(Color::Yellow),
        )]));
    }

    let header = Paragraph::new(header_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(plan.title.clone()),
        )
        .wrap(Wrap { trim: false });

    let mut options = vec![ListItem::new(format!(
        "Recommended: {}",
        plan.recommended.title
    ))];
    for alternative in &plan.alternatives {
        options.push(ListItem::new(format!("Alternative: {}", alternative.title)));
    }
    let option_list = List::new(options)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Recovery Paths"),
        )
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan))
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select(Some(selected));

    let option = if selected == 0 {
        &plan.recommended
    } else {
        plan.alternatives
            .get(selected - 1)
            .unwrap_or(&plan.recommended)
    };
    let mut preview_lines = vec![
        Line::from(vec![
            Span::styled("Summary: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(option.summary.clone()),
        ]),
        Line::from(vec![
            Span::styled("Impact: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(option.impact.clone()),
        ]),
        Line::from(vec![
            Span::styled("Rollback: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(option.rollback_hint.clone()),
        ]),
        Line::from(""),
    ];
    if option.steps.is_empty() {
        preview_lines.push(Line::from("(no executable steps)"));
    } else {
        for step in &option.steps {
            preview_lines.push(Line::from(format!("- {}", step.description)));
            preview_lines.push(Line::from(format!("  {}", step.command_preview)));
        }
    }
    if option.requires_fetch {
        preview_lines.push(Line::from(""));
        preview_lines.push(Line::from(vec![Span::styled(
            "This path may fetch remote refs before it restores anything.",
            Style::default().fg(Color::Yellow),
        )]));
    }

    let preview = Paragraph::new(preview_lines)
        .block(Block::default().borders(Borders::ALL).title("Preview"))
        .wrap(Wrap { trim: false });
    let help = Paragraph::new("Up/Down choose path  Enter run selected path  Esc cancel")
        .block(Block::default().borders(Borders::ALL).title("Keys"))
        .alignment(Alignment::Center);

    frame.render_widget(header, layout[0]);
    frame.render_stateful_widget(option_list, content[0], &mut state);
    frame.render_widget(preview, content[1]);
    frame.render_widget(help, layout[2]);
}

fn draw_prompt_input(
    frame: &mut ratatui::Frame<'_>,
    textarea: &TextArea<'_>,
    title: &str,
    prompt: &str,
) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Length(5),
            Constraint::Length(3),
        ])
        .split(centered_rect(80, 40, frame.area()));
    frame.render_widget(Clear, centered_rect(80, 40, frame.area()));

    let prompt_block = Paragraph::new(prompt)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false });
    let help = Paragraph::new("Type a SHA or ref  Enter submit  Esc cancel")
        .block(Block::default().borders(Borders::ALL).title("Keys"))
        .alignment(Alignment::Center);

    frame.render_widget(prompt_block, layout[0]);
    frame.render_widget(textarea, layout[1]);
    frame.render_widget(help, layout[2]);

    let (x, y) = textarea.cursor();
    frame.set_cursor_position((layout[1].x + x as u16 + 1, layout[1].y + y as u16 + 1));
}

fn draw_dangerous_command_confirm(
    frame: &mut ratatui::Frame<'_>,
    command: &str,
    description: &str,
) {
    let message = format!(
        "This command is potentially destructive and cannot be undone:\n\n  {}\n  {}\n\nPress Enter to execute, or Esc to cancel.",
        command, description
    );
    draw_message(frame, "Dangerous Command — Confirm", &message);
}

fn risk_badge(risk: RiskLevel) -> (&'static str, Color) {
    match risk {
        RiskLevel::Safe => ("[SAFE]", Color::Green),
        RiskLevel::Medium => ("[MED ]", Color::Yellow),
        RiskLevel::Dangerous => ("[RISK]", Color::Red),
    }
}

fn commit_loop(terminal: &mut AppTerminal, state: &mut CommitView) -> Result<CommitAction> {
    loop {
        state.poll_generation();
        terminal.draw(|frame| draw_commit(frame, state))?;

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }

        if let Event::Key(key) = event::read()? {
            match state.mode {
                CommitMode::Browsing => {
                    if handle_commit_browsing_key(state, key)? {
                        if let Some(message) = state.take_confirmed_message() {
                            return Ok(CommitAction::Confirmed(message));
                        }
                        if let Some(plans) = state.take_split_request() {
                            return Ok(CommitAction::SplitRequested(plans));
                        }
                        if state.cancelled {
                            return Ok(CommitAction::Cancelled);
                        }
                    }
                }
                CommitMode::Editing => handle_commit_editing_key(state, key),
            }
        }
    }
}

fn config_setup_loop(
    terminal: &mut AppTerminal,
    state: &mut ConfigSetupView,
) -> Result<Option<ConfigSetupAction>> {
    loop {
        state.poll_model_options();
        terminal.draw(|frame| draw_config_setup(frame, state))?;

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }

        if let Event::Key(key) = event::read()? {
            match state.mode {
                ConfigMode::Browsing => match handle_config_browsing_key(state, key)? {
                    ConfigSetupLoopAction::Continue => {}
                    ConfigSetupLoopAction::Submit(action) => return Ok(Some(action)),
                    ConfigSetupLoopAction::Cancel => return Ok(None),
                },
                ConfigMode::Editing => handle_config_editing_key(state, key)?,
            }
        }
    }
}

fn force_push_loop(
    terminal: &mut AppTerminal,
    plan: &PushPlan,
    error_message: &str,
) -> Result<bool> {
    loop {
        terminal.draw(|frame| draw_force_push_confirm(frame, plan, error_message))?;

        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Enter => return Ok(true),
                KeyCode::Esc => return Ok(false),
                _ => {}
            }
        }
    }
}

fn stage_all_changes_loop(terminal: &mut AppTerminal) -> Result<bool> {
    loop {
        terminal.draw(draw_stage_all_changes_confirm)?;

        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Enter => return Ok(true),
                KeyCode::Esc => return Ok(false),
                _ => {}
            }
        }
    }
}

fn split_commit_confirm_loop(
    terminal: &mut AppTerminal,
    plans: &[SplitCommitPlan],
) -> Result<bool> {
    loop {
        terminal.draw(|frame| draw_split_commit_confirm(frame, plans))?;

        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Enter => return Ok(true),
                KeyCode::Esc => return Ok(false),
                _ => {}
            }
        }
    }
}

fn unsafe_diff_warning_loop(
    terminal: &mut AppTerminal,
    action_label: &str,
    warnings: &[UnsafeDiffWarning],
) -> Result<bool> {
    loop {
        terminal.draw(|frame| draw_unsafe_diff_warning(frame, action_label, warnings))?;

        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Enter => return Ok(true),
                KeyCode::Esc => return Ok(false),
                _ => {}
            }
        }
    }
}

fn handle_commit_browsing_key(state: &mut CommitView, key: KeyEvent) -> Result<bool> {
    match key.code {
        KeyCode::Up => {
            state.move_selection(-1);
            Ok(false)
        }
        KeyCode::Down => {
            state.move_selection(1);
            Ok(false)
        }
        KeyCode::Left => {
            state.scroll_staged(-1);
            Ok(false)
        }
        KeyCode::Right => {
            state.scroll_staged(1);
            Ok(false)
        }
        KeyCode::PageUp => {
            state.scroll_staged(-8);
            Ok(false)
        }
        KeyCode::PageDown => {
            state.scroll_staged(8);
            Ok(false)
        }
        KeyCode::Home => {
            state.scroll_staged_to_start();
            Ok(false)
        }
        KeyCode::End => {
            state.scroll_staged_to_end();
            Ok(false)
        }
        KeyCode::Char('r') => {
            state.restart_generation()?;
            Ok(false)
        }
        KeyCode::Char('e') => {
            if matches!(state.generation, GenerationState::Ready) && !state.drafts.is_empty() {
                state.mode = CommitMode::Editing;
                state.textarea.set_cursor_line_style(Style::default());
            }
            Ok(false)
        }
        KeyCode::Char('s') => {
            if matches!(state.generation, GenerationState::Ready) && !state.split_plans.is_empty() {
                state.split_requested = Some(state.split_plans.clone());
                return Ok(true);
            }
            Ok(false)
        }
        KeyCode::Enter => {
            if !matches!(state.generation, GenerationState::Ready) || state.drafts.is_empty() {
                return Ok(false);
            }
            let message = validate_commit_message_with_preset(
                &state.current_message(),
                state.commit_style,
                &state.conventional_preset,
            )?;
            state.confirmed_message = Some(message);
            Ok(true)
        }
        KeyCode::Esc => {
            state.cancelled = true;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn handle_commit_editing_key(state: &mut CommitView, key: KeyEvent) {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
        state.sync_current_draft();
        state.mode = CommitMode::Browsing;
        return;
    }

    if key.code == KeyCode::Esc {
        state.sync_current_draft();
        state.mode = CommitMode::Browsing;
        return;
    }

    state.textarea.input(key_event_to_textarea_input(key));
}

fn handle_config_browsing_key(
    state: &mut ConfigSetupView,
    key: KeyEvent,
) -> Result<ConfigSetupLoopAction> {
    match key.code {
        KeyCode::Up => {
            if state.selected_field() == ConfigField::BaseModel && state.model_picker_active {
                state.move_model_highlight(-1);
            } else {
                state.move_selection(-1);
            }
        }
        KeyCode::Down => {
            if state.selected_field() == ConfigField::BaseModel && state.model_picker_active {
                state.move_model_highlight(1);
            } else {
                state.move_selection(1);
            }
        }
        KeyCode::Left => state.cycle_selected(false),
        KeyCode::Right => state.cycle_selected(true),
        KeyCode::Char('e') => state.begin_editing_selected(),
        KeyCode::Char('s') => match state.save() {
            Ok(Some(action)) => return Ok(ConfigSetupLoopAction::Submit(action)),
            Ok(None) => return Ok(ConfigSetupLoopAction::Continue),
            Err(error) => state.error = Some(error.to_string()),
        },
        KeyCode::Enter => match state.selected_field() {
            ConfigField::Provider | ConfigField::CommitStyle | ConfigField::GenerationMode => {
                state.cycle_selected(true)
            }
            ConfigField::BaseApiUrl | ConfigField::ApiToken => state.begin_editing_selected(),
            ConfigField::BaseModel => state.open_or_apply_model_picker(),
            ConfigField::Save => match state.save() {
                Ok(Some(action)) => return Ok(ConfigSetupLoopAction::Submit(action)),
                Ok(None) => return Ok(ConfigSetupLoopAction::Continue),
                Err(error) => state.error = Some(error.to_string()),
            },
            ConfigField::Cancel => return Ok(ConfigSetupLoopAction::Cancel),
        },
        KeyCode::Esc => {
            if state.selected_field() == ConfigField::BaseModel && state.model_picker_active {
                state.close_model_picker();
            } else {
                return Ok(ConfigSetupLoopAction::Cancel);
            }
        }
        _ => {}
    }

    Ok(ConfigSetupLoopAction::Continue)
}

fn handle_config_editing_key(state: &mut ConfigSetupView, key: KeyEvent) -> Result<()> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
        state.finish_editing(true)?;
        return Ok(());
    }

    match key.code {
        KeyCode::Enter => state.finish_editing(true)?,
        KeyCode::Esc => state.finish_editing(false)?,
        _ => {
            state.textarea.input(key_event_to_textarea_input(key));
        }
    }

    Ok(())
}

fn draw_home(frame: &mut ratatui::Frame<'_>, status: &RepoStatus) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(9), Constraint::Min(3)])
        .split(frame.area());

    let branch = status
        .branch
        .clone()
        .unwrap_or_else(|| "DETACHED".to_string());
    let remote_status = if status.has_upstream {
        status
            .tracking
            .clone()
            .unwrap_or_else(|| "upstream configured".to_string())
    } else if status.remotes.is_empty() {
        "no remotes".to_string()
    } else {
        format!("{} remotes, no upstream", status.remotes.len())
    };

    let summary = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("Branch: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(branch),
        ]),
        Line::from(vec![
            Span::styled("Staged: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(status.staged_count.to_string()),
        ]),
        Line::from(vec![
            Span::styled("Unstaged: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(status.unstaged_count.to_string()),
        ]),
        Line::from(vec![
            Span::styled("Remote: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(remote_status),
        ]),
    ])
    .block(Block::default().borders(Borders::ALL).title("gitgud"))
    .alignment(Alignment::Left);

    let shortcuts = Paragraph::new(vec![
        Line::from("s  ship branch"),
        Line::from("c  commit staged changes"),
        Line::from("p  push current branch"),
        Line::from("r  rescue Git mistakes"),
        Line::from("q  quit"),
    ])
    .block(Block::default().borders(Borders::ALL).title("Actions"))
    .wrap(Wrap { trim: false });

    frame.render_widget(summary, layout[0]);
    frame.render_widget(shortcuts, layout[1]);
}

fn draw_commit(frame: &mut ratatui::Frame<'_>, state: &CommitView) {
    let status_lines = build_commit_status_lines(state);
    let status_height = status_lines.len() as u16 + 2;
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(status_height),
            Constraint::Min(10),
            Constraint::Length(3),
        ])
        .split(frame.area());
    let content = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(34), Constraint::Percentage(66)])
        .split(layout[1]);
    let sidebar = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Fill(3), Constraint::Fill(2)])
        .split(content[0]);
    let tree_lines = staged_files_tree_lines(&state.staged_files);

    let status_block = Paragraph::new(status_lines)
        .block(Block::default().borders(Borders::ALL).title("Commit"))
        .wrap(Wrap { trim: false });
    let staged_files = Paragraph::new(tree_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Staged Tree ({})", state.staged_files.len())),
        )
        .scroll((state.staged_scroll as u16, 0))
        .wrap(Wrap { trim: false });

    let options = if state.options.is_empty() {
        vec![ListItem::new("(waiting for options)")]
    } else {
        state
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| {
                let subject = option.lines().next().unwrap_or("(empty)");
                ListItem::new(format!("{}. {}", index + 1, subject))
            })
            .collect::<Vec<_>>()
    };
    let options_list = List::new(options)
        .block(Block::default().borders(Borders::ALL).title("Options"))
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan))
        .highlight_symbol("> ");

    let help = Paragraph::new(build_commit_help_line(state))
        .block(Block::default().borders(Borders::ALL).title("Keys"))
        .alignment(Alignment::Center);
    let mut list_state = state.list_state.clone();

    frame.render_widget(status_block, layout[0]);
    frame.render_widget(staged_files, sidebar[0]);
    frame.render_stateful_widget(options_list, sidebar[1], &mut list_state);
    frame.render_widget(&state.textarea, content[1]);
    frame.render_widget(help, layout[2]);

    if matches!(state.mode, CommitMode::Editing) {
        let (x, y) = state.textarea.cursor();
        frame.set_cursor_position((x as u16, y as u16));
    }
}

fn build_commit_help_line(state: &CommitView) -> Line<'static> {
    let key_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);

    match state.mode {
        CommitMode::Browsing => Line::from(vec![
            Span::styled("Up/Down", key_style),
            Span::raw(" select  "),
            Span::styled("e", key_style),
            Span::raw(" edit  "),
            Span::styled("r", key_style),
            Span::raw(" regenerate  "),
            Span::styled("s", key_style),
            Span::raw(" split  "),
            Span::styled("Enter", key_style),
            Span::raw(" commit  "),
            Span::styled("Esc", key_style),
            Span::raw(" cancel"),
        ]),
        CommitMode::Editing => Line::from(vec![
            Span::raw("Edit draft inline. "),
            Span::styled("Ctrl-S", key_style),
            Span::raw(" or "),
            Span::styled("Esc", key_style),
            Span::raw(" returns to browse mode."),
        ]),
    }
}

fn build_commit_status_lines(state: &CommitView) -> Vec<Line<'static>> {
    let status_text = match &state.generation {
        GenerationState::Loading => "Generating 1-3 commit message options from the staged diff...",
        GenerationState::Ready if state.split_plans.is_empty() => {
            "Choose an option, edit if needed, then press Enter to commit."
        }
        GenerationState::Ready => "Staged changes look mixed. Press s to let gitgud split them.",
        GenerationState::Error(error) => error.as_str(),
    };

    let style_line = if matches!(state.commit_style, CommitStyle::Conventional) {
        format!(
            "Style: {} ({})",
            state.commit_style, state.conventional_preset.name
        )
    } else {
        format!("Style: {}", state.commit_style)
    };
    let generation_line = format!("Generation: {}", state.generation_mode);

    let mut lines = vec![
        Line::from(style_line),
        Line::from(generation_line),
        Line::from(format!("Staged count: {}", state.staged_files.len())),
        Line::from("Left/Right scroll staged tree  PgUp/PgDn jump  Home/End edges"),
        Line::from(status_text.to_string()),
    ];

    if let Some(note) = &state.generation_note {
        lines.push(Line::from(vec![Span::styled(
            note.clone(),
            Style::default().fg(Color::Yellow),
        )]));
    }

    if matches!(state.commit_style, CommitStyle::Conventional) {
        lines.insert(
            2,
            Line::from(format!(
                "Allowed types: {}",
                state.conventional_preset.types.join(", ")
            )),
        );
    }

    if matches!(state.generation, GenerationState::Ready) && !state.split_plans.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "Split plan:",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )]));
        for plan in &state.split_plans {
            lines.push(Line::from(format!("  - {}", plan.message)));
        }
    }

    lines
}

fn staged_files_tree_lines(staged_files: &[String]) -> Vec<Line<'static>> {
    if staged_files.is_empty() {
        return vec![Line::from("(none)")];
    }

    let mut root = FileTreeNode::default();
    for path in staged_files {
        root.insert(path);
    }

    let mut lines = Vec::new();
    root.render(String::new(), &mut lines);
    lines
}

fn draw_config_setup(frame: &mut ratatui::Frame<'_>, state: &ConfigSetupView) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(10),
            Constraint::Length(4),
        ])
        .split(frame.area());
    let content = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(layout[1]);

    let header = Paragraph::new(vec![
        Line::from("Configure provider, endpoint, model, token, and commit generation."),
        Line::from(state.status_line()),
    ])
    .block(Block::default().borders(Borders::ALL).title("Setup"))
    .wrap(Wrap { trim: false });

    let items = ConfigField::all()
        .iter()
        .map(|field| ListItem::new(state.field_label(*field)))
        .collect::<Vec<_>>();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Fields"))
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan))
        .highlight_symbol("> ");

    let detail_scroll = state.detail_scroll_offset(content[1].height);
    let detail: Paragraph = if matches!(state.mode, ConfigMode::Editing) {
        Paragraph::new(state.current_editor_help())
            .block(Block::default().borders(Borders::ALL).title("Edit Help"))
            .wrap(Wrap { trim: false })
    } else {
        Paragraph::new(state.detail_text())
            .block(Block::default().borders(Borders::ALL).title("Details"))
            .scroll((detail_scroll, 0))
            .wrap(Wrap { trim: false })
    };

    let help_text = match state.mode {
        ConfigMode::Browsing => {
            "Up/Down move or scroll picker  Left/Right cycle choices  Enter edit/select/load  e manual edit  s save  Esc cancel/close picker"
        }
        ConfigMode::Editing => "Type a value. Enter or Ctrl-S saves the field. Esc discards edits.",
    };
    let help = Paragraph::new(help_text)
        .block(Block::default().borders(Borders::ALL).title("Keys"))
        .wrap(Wrap { trim: false });
    let mut list_state = state.list_state.clone();

    frame.render_widget(header, layout[0]);
    frame.render_stateful_widget(list, content[0], &mut list_state);

    if matches!(state.mode, ConfigMode::Editing) {
        frame.render_widget(&state.textarea, content[1]);
        let (x, y) = state.textarea.cursor();
        frame.set_cursor_position((x as u16, y as u16));
    } else {
        frame.render_widget(detail, content[1]);
    }
    frame.render_widget(help, layout[2]);
}

fn draw_force_push_confirm(frame: &mut ratatui::Frame<'_>, plan: &PushPlan, error_message: &str) {
    let message = match plan {
        PushPlan::Upstream { branch } => format!(
            "Normal push for '{branch}' was rejected.\n\n{error_message}\n\nPress Enter to retry with --force-with-lease, or Esc to cancel."
        ),
        PushPlan::SetUpstream { remote, branch } => format!(
            "Normal push for '{branch}' to '{remote}' was rejected.\n\n{error_message}\n\nPress Enter to retry with --force-with-lease, or Esc to cancel."
        ),
    };

    draw_message(frame, "Force Push Confirmation", &message);
}

fn draw_stage_all_changes_confirm(frame: &mut ratatui::Frame<'_>) {
    draw_message(
        frame,
        "Stage All Changes",
        "No staged changes found.\n\nPress Enter to stage all tracked and untracked changes with git add --all, or Esc to cancel.",
    );
}

fn draw_split_commit_confirm(frame: &mut ratatui::Frame<'_>, plans: &[SplitCommitPlan]) {
    let mut lines = vec![
        "gitgud will clear the current index, restage each group, and create one commit per plan."
            .to_string(),
        String::new(),
    ];
    for plan in plans {
        lines.push(format!("- {}", plan.message));
        lines.push(format!("  files: {}", plan.files.join(", ")));
    }
    lines.push(String::new());
    lines.push("Press Enter to create these split commits, or Esc to cancel.".to_string());

    draw_message(frame, "Split Commit Confirmation", &lines.join("\n"));
}

fn draw_unsafe_diff_warning(
    frame: &mut ratatui::Frame<'_>,
    action_label: &str,
    warnings: &[UnsafeDiffWarning],
) {
    let mut lines = vec![
        format!("Suspicious diff patterns were detected before {action_label}."),
        String::new(),
    ];
    for warning in warnings {
        lines.push(format!("- {}", warning.message));
    }
    lines.push(String::new());
    lines.push(format!(
        "Press Enter to continue with {action_label}, or Esc to cancel."
    ));

    draw_message(frame, "Unsafe Diff Warning", &lines.join("\n"));
}

fn draw_message(frame: &mut ratatui::Frame<'_>, title: &str, message: &str) {
    let popup = centered_rect(72, 40, frame.area());
    frame.render_widget(Clear, popup);

    let paragraph = Paragraph::new(message)
        .block(Block::default().borders(Borders::ALL).title(title))
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, popup);
}

fn centered_rect(
    percent_x: u16,
    percent_y: u16,
    area: ratatui::layout::Rect,
) -> ratatui::layout::Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn with_terminal<T>(f: impl FnOnce(&mut AppTerminal) -> Result<T>) -> Result<T> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let result = f(&mut terminal);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn key_event_to_textarea_input(key: KeyEvent) -> Input {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    match key.code {
        KeyCode::Char(ch) => Input {
            key: Key::Char(ch),
            ctrl,
            alt,
            shift,
        },
        KeyCode::Backspace => Input {
            key: Key::Backspace,
            ctrl,
            alt,
            shift,
        },
        KeyCode::Delete => Input {
            key: Key::Delete,
            ctrl,
            alt,
            shift,
        },
        KeyCode::Left => Input {
            key: Key::Left,
            ctrl,
            alt,
            shift,
        },
        KeyCode::Right => Input {
            key: Key::Right,
            ctrl,
            alt,
            shift,
        },
        KeyCode::Up => Input {
            key: Key::Up,
            ctrl,
            alt,
            shift,
        },
        KeyCode::Down => Input {
            key: Key::Down,
            ctrl,
            alt,
            shift,
        },
        KeyCode::Home => Input {
            key: Key::Home,
            ctrl,
            alt,
            shift,
        },
        KeyCode::End => Input {
            key: Key::End,
            ctrl,
            alt,
            shift,
        },
        KeyCode::Tab => Input {
            key: Key::Tab,
            ctrl,
            alt,
            shift,
        },
        KeyCode::Enter => Input {
            key: Key::Enter,
            ctrl,
            alt,
            shift,
        },
        _ => Input {
            key: Key::Null,
            ctrl,
            alt,
            shift,
        },
    }
}

struct CommitView {
    textarea: TextArea<'static>,
    generation: GenerationState,
    generation_note: Option<String>,
    mode: CommitMode,
    staged_files: Vec<String>,
    receiver: Option<Receiver<Result<CommitSuggestions>>>,
    generator: Option<(CommitGenerator, PromptInput)>,
    options: Vec<String>,
    drafts: Vec<String>,
    split_plans: Vec<SplitCommitPlan>,
    list_state: ListState,
    staged_scroll: usize,
    confirmed_message: Option<String>,
    split_requested: Option<Vec<SplitCommitPlan>>,
    cancelled: bool,
    commit_style: CommitStyle,
    generation_mode: GenerationMode,
    conventional_preset: ResolvedConventionalPreset,
}

impl CommitView {
    fn new(
        staged_files: Vec<String>,
        commit_style: CommitStyle,
        generation_mode: GenerationMode,
        conventional_preset: ResolvedConventionalPreset,
    ) -> Self {
        Self {
            textarea: make_textarea("Draft", "Generating commit message options..."),
            generation: GenerationState::Loading,
            generation_note: None,
            mode: CommitMode::Browsing,
            staged_files,
            receiver: None,
            generator: None,
            options: Vec::new(),
            drafts: Vec::new(),
            split_plans: Vec::new(),
            list_state: ListState::default().with_selected(Some(0)),
            staged_scroll: 0,
            confirmed_message: None,
            split_requested: None,
            cancelled: false,
            commit_style,
            generation_mode,
            conventional_preset,
        }
    }

    fn start_generation(&mut self, generator: CommitGenerator, input: PromptInput) {
        self.generator = Some((generator.clone(), input.clone()));
        self.spawn_generation(generator, input);
    }

    fn restart_generation(&mut self) -> Result<()> {
        let (generator, input) = self
            .generator
            .clone()
            .ok_or_else(|| anyhow!("commit generation is not configured"))?;
        self.spawn_generation(generator, input);
        Ok(())
    }

    fn spawn_generation(&mut self, generator: CommitGenerator, input: PromptInput) {
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        self.generation = GenerationState::Loading;
        self.generation_note = None;
        self.mode = CommitMode::Browsing;
        self.options.clear();
        self.drafts.clear();
        self.split_plans.clear();
        self.list_state.select(Some(0));
        self.staged_scroll = 0;
        self.split_requested = None;
        self.textarea = make_textarea("Draft", "Generating commit message options...");

        tokio::spawn(async move {
            let result = match generator {
                CommitGenerator::Ai(ai) => ai.generate_commit_suggestions(&input).await,
                CommitGenerator::HeuristicOnly => {
                    let mut suggestions = build_heuristic_commit_suggestions(&input);
                    suggestions.note = Some("Using heuristic commit options only.".to_string());
                    Ok(suggestions)
                }
            };
            let _ = tx.send(result);
        });
    }

    fn poll_generation(&mut self) {
        let Some(receiver) = &self.receiver else {
            return;
        };

        match receiver.try_recv() {
            Ok(result) => {
                self.receiver = None;
                match result {
                    Ok(suggestions) => {
                        self.options = suggestions.options.clone();
                        self.drafts = suggestions.options;
                        self.split_plans = suggestions.split;
                        self.generation_note = suggestions.note;
                        self.list_state.select(Some(0));
                        self.load_selected_draft();
                        self.generation = GenerationState::Ready;
                    }
                    Err(error) => {
                        self.generation = GenerationState::Error(error.to_string());
                        self.generation_note = None;
                        self.textarea = make_textarea("Draft", "No commit draft available");
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.receiver = None;
                self.generation_note = None;
                self.generation = GenerationState::Error("generation task disconnected".into());
            }
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.drafts.is_empty() || !matches!(self.generation, GenerationState::Ready) {
            return;
        }

        let current = self.list_state.selected().unwrap_or(0) as isize;
        let last = self.drafts.len().saturating_sub(1) as isize;
        let next = (current + delta).clamp(0, last) as usize;
        self.list_state.select(Some(next));
        self.load_selected_draft();
    }

    fn load_selected_draft(&mut self) {
        let Some(index) = self.list_state.selected() else {
            return;
        };
        let draft = self.drafts.get(index).cloned().unwrap_or_default();
        self.textarea = make_textarea_with_content("Draft", &draft);
    }

    fn sync_current_draft(&mut self) {
        let current_message = self.current_message();
        if let Some(index) = self.list_state.selected() {
            if let Some(draft) = self.drafts.get_mut(index) {
                *draft = current_message;
            }
        }
    }

    fn current_message(&self) -> String {
        self.textarea.lines().join("\n")
    }

    fn take_confirmed_message(&mut self) -> Option<String> {
        self.confirmed_message.take()
    }

    fn take_split_request(&mut self) -> Option<Vec<SplitCommitPlan>> {
        self.split_requested.take()
    }

    fn scroll_staged(&mut self, delta: isize) {
        let total_lines = staged_files_tree_lines(&self.staged_files).len();
        let max_scroll = total_lines.saturating_sub(1) as isize;
        let next = (self.staged_scroll as isize + delta).clamp(0, max_scroll) as usize;
        self.staged_scroll = next;
    }

    fn scroll_staged_to_start(&mut self) {
        self.staged_scroll = 0;
    }

    fn scroll_staged_to_end(&mut self) {
        self.staged_scroll = staged_files_tree_lines(&self.staged_files)
            .len()
            .saturating_sub(1);
    }
}

#[derive(Debug)]
enum GenerationState {
    Loading,
    Ready,
    Error(String),
}

#[derive(Debug, Clone, Copy)]
enum CommitMode {
    Browsing,
    Editing,
}

struct ConfigSetupView {
    provider: Provider,
    base_api_url: String,
    base_model: String,
    commit_style: CommitStyle,
    generation_mode: GenerationMode,
    existing_api_token: Option<String>,
    pending_api_token: String,
    token_status: TokenStatus,
    token_present: bool,
    model_options: ModelOptionsState,
    model_options_receiver: Option<Receiver<Result<Vec<String>>>>,
    model_picker_active: bool,
    model_highlighted: usize,
    mode: ConfigMode,
    selected: usize,
    list_state: ListState,
    textarea: TextArea<'static>,
    error: Option<String>,
}

const MODEL_DETAIL_HEADER_LINES: usize = 4;

impl ConfigSetupView {
    fn new(input: ConfigSetupInput) -> Self {
        let model_options = if can_load_model_options(
            &input.base_api_url,
            input.existing_api_token.as_deref(),
            None,
        ) {
            ModelOptionsState::Idle
        } else {
            ModelOptionsState::Unavailable
        };

        Self {
            provider: input.provider,
            base_api_url: input.base_api_url,
            base_model: input.base_model,
            commit_style: input.commit_style,
            generation_mode: input.generation_mode,
            existing_api_token: input.existing_api_token,
            pending_api_token: String::new(),
            token_status: input.token_status,
            token_present: input.token_present,
            model_options,
            model_options_receiver: None,
            model_picker_active: false,
            model_highlighted: 0,
            mode: ConfigMode::Browsing,
            selected: 0,
            list_state: ListState::default().with_selected(Some(0)),
            textarea: make_textarea("Value", ""),
            error: None,
        }
    }

    fn selected_field(&self) -> ConfigField {
        ConfigField::all()[self.selected]
    }

    fn move_selection(&mut self, delta: isize) {
        let last = ConfigField::all().len().saturating_sub(1) as isize;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
        self.list_state.select(Some(self.selected));
    }

    fn cycle_selected(&mut self, forward: bool) {
        match self.selected_field() {
            ConfigField::Provider => {
                self.provider = cycle_provider(self.provider, forward);
                self.apply_provider_defaults();
            }
            ConfigField::CommitStyle => {
                self.commit_style = cycle_commit_style(self.commit_style, forward);
            }
            ConfigField::GenerationMode => {
                self.generation_mode = cycle_generation_mode(self.generation_mode, forward);
            }
            _ => {}
        }
    }

    fn begin_editing_selected(&mut self) {
        let (title, value) = match self.selected_field() {
            ConfigField::BaseApiUrl => ("Base API URL", self.base_api_url.clone()),
            ConfigField::BaseModel => ("Model", self.base_model.clone()),
            ConfigField::ApiToken => ("API Token", self.pending_api_token.clone()),
            _ => return,
        };

        self.model_picker_active = false;
        self.textarea = make_textarea_with_content(title, &value);
        self.mode = ConfigMode::Editing;
        self.error = None;
    }

    fn finish_editing(&mut self, save: bool) -> Result<()> {
        if save {
            let value = self.textarea.lines().join("\n").trim().to_string();
            match self.selected_field() {
                ConfigField::BaseApiUrl => {
                    self.base_api_url = value;
                    self.reset_model_options();
                }
                ConfigField::BaseModel => self.base_model = value,
                ConfigField::ApiToken => {
                    self.pending_api_token = value;
                    self.reset_model_options();
                }
                _ => bail!("cannot edit this field"),
            }
        }

        self.mode = ConfigMode::Browsing;
        self.error = None;
        Ok(())
    }

    fn save(&mut self) -> Result<Option<ConfigSetupAction>> {
        let base_api_url = normalize_base_api_url(&self.base_api_url)
            .map_err(|error| anyhow!("BASE_API_URL: {error}"))?;
        let base_model = self.base_model.trim();
        if base_model.is_empty() {
            bail!("BASE_MODEL cannot be empty");
        }

        if !matches!(self.generation_mode, GenerationMode::HeuristicOnly)
            && matches!(self.token_status, TokenStatus::Missing)
            && !self.token_present
            && self.pending_api_token.trim().is_empty()
        {
            bail!("API_TOKEN is required when no token is currently configured");
        }

        let action = ConfigSetupAction {
            provider: self.provider,
            base_api_url,
            base_model: base_model.to_string(),
            commit_style: self.commit_style,
            generation_mode: self.generation_mode,
            api_token: if self.pending_api_token.trim().is_empty() {
                None
            } else {
                Some(self.pending_api_token.trim().to_string())
            },
        };

        Ok(Some(action))
    }

    fn field_label(&self, field: ConfigField) -> String {
        match field {
            ConfigField::Provider => format!("Provider: {}", self.provider),
            ConfigField::BaseApiUrl => format!("Base API URL: {}", self.base_api_url),
            ConfigField::BaseModel => {
                format!("Model: {}{}", self.base_model, self.base_model_suffix())
            }
            ConfigField::ApiToken => format!("API Token: {}", self.token_label()),
            ConfigField::CommitStyle => format!("Commit Style: {}", self.commit_style),
            ConfigField::GenerationMode => {
                format!("Generation Mode: {}", self.generation_mode)
            }
            ConfigField::Save => "Save".to_string(),
            ConfigField::Cancel => "Cancel".to_string(),
        }
    }

    fn token_label(&self) -> String {
        if !self.pending_api_token.trim().is_empty() {
            format!(
                "new value staged ({} chars)",
                self.pending_api_token.trim().chars().count()
            )
        } else {
            match self.token_status {
                TokenStatus::EnvironmentOverride => "provided by environment".to_string(),
                TokenStatus::Keychain => "stored in keychain".to_string(),
                TokenStatus::Missing => "(missing)".to_string(),
            }
        }
    }

    fn detail_text(&self) -> String {
        if let Some(error) = &self.error {
            return format!("Validation error:\n{error}");
        }

        match self.selected_field() {
            ConfigField::Provider => {
                "Choose `gemini` or `openai-compatible`. Switching provider also refreshes the default endpoint and model.".to_string()
            }
            ConfigField::BaseApiUrl => {
                "Set the OpenAI-compatible base URL used for chat completions.".to_string()
            }
            ConfigField::BaseModel => self.base_model_detail_text(),
            ConfigField::ApiToken => {
                "Enter a new API token to store it in the system keychain. Leave it blank to keep the current token.".to_string()
            }
            ConfigField::CommitStyle => {
                "Choose `standard` for plain Git subjects or `conventional` for Conventional Commits. Custom Conventional Commit presets can be selected with `gg config set conventional-preset <name>` after defining them in the config file.".to_string()
            }
            ConfigField::GenerationMode => {
                "Choose `auto` to use AI with timeout fallback, `ai-only` to require the provider, or `heuristic-only` to skip AI and use local suggestions.".to_string()
            }
            ConfigField::Save => {
                "Save the current settings. Base API URL and Model are written to the config file; API Token is stored in the keychain.".to_string()
            }
            ConfigField::Cancel => "Exit without writing changes.".to_string(),
        }
    }

    fn current_editor_help(&self) -> String {
        match self.selected_field() {
            ConfigField::BaseApiUrl => "Edit the full Base API URL value.",
            ConfigField::BaseModel => {
                "Edit the Model value manually. In browsing mode, press Enter to open the provider model picker and Enter again to apply the highlighted model."
            }
            ConfigField::ApiToken => "Enter a replacement API Token for the keychain.",
            _ => "Edit the selected field.",
        }
        .to_string()
    }

    fn status_line(&self) -> String {
        match &self.error {
            Some(error) => format!("Error: {error}"),
            None if matches!(self.model_options, ModelOptionsState::Loading) => {
                "Loading model options from the configured provider.".to_string()
            }
            None if self.model_picker_active => {
                "Model picker active. Up/Down moves and scrolls the list, Enter applies the model, Esc closes the picker.".to_string()
            }
            None => {
                if matches!(self.generation_mode, GenerationMode::HeuristicOnly) {
                    "Heuristic-only mode skips the AI provider for commit suggestions.".to_string()
                } else {
                    match self.token_status {
                        TokenStatus::EnvironmentOverride => {
                            "Environment token override is active; a saved token will be ignored until it is removed.".to_string()
                        }
                        TokenStatus::Keychain => {
                            "A token is already stored in the system keychain.".to_string()
                        }
                        TokenStatus::Missing => "No API token is currently configured.".to_string(),
                    }
                }
            }
        }
    }

    fn apply_provider_defaults(&mut self) {
        self.base_api_url = self.provider.default_base_api_url().to_string();
        self.base_model = self.provider.default_base_model().to_string();
        self.reset_model_options();
    }

    fn effective_api_token(&self) -> Option<String> {
        let pending = self.pending_api_token.trim();
        if !pending.is_empty() {
            return Some(pending.to_string());
        }

        self.existing_api_token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(str::to_string)
    }

    fn reset_model_options(&mut self) {
        self.model_options_receiver = None;
        self.model_picker_active = false;
        self.model_highlighted = 0;
        self.model_options = if can_load_model_options(
            &self.base_api_url,
            self.existing_api_token.as_deref(),
            Some(self.pending_api_token.as_str()),
        ) {
            ModelOptionsState::Idle
        } else {
            ModelOptionsState::Unavailable
        };
    }

    fn poll_model_options(&mut self) {
        let Some(receiver) = &self.model_options_receiver else {
            return;
        };

        match receiver.try_recv() {
            Ok(result) => {
                self.model_options_receiver = None;
                match result {
                    Ok(models) => {
                        self.model_highlighted = models
                            .iter()
                            .position(|model| model == &self.base_model)
                            .unwrap_or(0);
                        self.model_options = ModelOptionsState::Loaded(models);
                    }
                    Err(error) => {
                        self.model_picker_active = false;
                        self.model_options = ModelOptionsState::Error(error.to_string());
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.model_options_receiver = None;
                self.model_picker_active = false;
                self.model_options =
                    ModelOptionsState::Error("model loading task disconnected".to_string());
            }
        }
    }

    fn open_or_apply_model_picker(&mut self) {
        self.error = None;

        if !can_load_model_options(
            &self.base_api_url,
            self.existing_api_token.as_deref(),
            Some(self.pending_api_token.as_str()),
        ) {
            self.model_options = ModelOptionsState::Unavailable;
            self.error = Some(
                "Fill BASE_API_URL and API_TOKEN before loading provider model options."
                    .to_string(),
            );
            return;
        }

        match &self.model_options {
            ModelOptionsState::Loaded(models) => {
                if models.is_empty() {
                    self.model_options =
                        ModelOptionsState::Error("AI provider returned no models".to_string());
                    return;
                }
                if self.model_picker_active {
                    self.apply_highlighted_model();
                } else {
                    self.model_picker_active = true;
                    self.model_highlighted = models
                        .iter()
                        .position(|model| model == &self.base_model)
                        .unwrap_or(0);
                }
            }
            ModelOptionsState::Loading => {}
            ModelOptionsState::Idle
            | ModelOptionsState::Unavailable
            | ModelOptionsState::Error(_) => {
                self.model_picker_active = true;
                self.load_model_options();
            }
        }
    }

    fn load_model_options(&mut self) {
        let Some(api_token) = self.effective_api_token() else {
            self.model_options = ModelOptionsState::Unavailable;
            self.error = Some(
                "Fill BASE_API_URL and API_TOKEN before loading provider model options."
                    .to_string(),
            );
            return;
        };

        let base_api_url = self.base_api_url.clone();
        let (tx, rx) = mpsc::channel();
        self.model_options = ModelOptionsState::Loading;
        self.model_options_receiver = Some(rx);
        self.error = None;

        tokio::spawn(async move {
            let result = fetch_model_options(&base_api_url, &api_token).await;
            let _ = tx.send(result);
        });
    }

    fn move_model_highlight(&mut self, delta: isize) {
        let ModelOptionsState::Loaded(models) = &self.model_options else {
            return;
        };
        if models.is_empty() {
            return;
        }

        self.model_picker_active = true;
        let len = models.len() as isize;
        let current = self.model_highlighted.min(models.len().saturating_sub(1)) as isize;
        let next = (current + delta).clamp(0, len - 1) as usize;
        self.model_highlighted = next;
    }

    fn apply_highlighted_model(&mut self) {
        let ModelOptionsState::Loaded(models) = &self.model_options else {
            return;
        };
        if let Some(model) = models.get(self.model_highlighted) {
            self.base_model = model.clone();
        }
        self.model_picker_active = false;
    }

    fn close_model_picker(&mut self) {
        self.model_picker_active = false;
        if let ModelOptionsState::Loaded(models) = &self.model_options {
            self.model_highlighted = models
                .iter()
                .position(|model| model == &self.base_model)
                .unwrap_or(0);
        }
    }

    fn detail_scroll_offset(&self, detail_height: u16) -> u16 {
        let ModelOptionsState::Loaded(models) = &self.model_options else {
            return 0;
        };

        let viewport_lines = detail_height.saturating_sub(2) as usize;
        if models.is_empty() || viewport_lines == 0 {
            return 0;
        }

        let total_lines = MODEL_DETAIL_HEADER_LINES + models.len();
        if total_lines <= viewport_lines {
            return 0;
        }

        let focus_index = if self.model_picker_active {
            self.model_highlighted.min(models.len().saturating_sub(1))
        } else {
            models
                .iter()
                .position(|model| model == &self.base_model)
                .unwrap_or(0)
        };
        let focus_line = MODEL_DETAIL_HEADER_LINES + focus_index;
        let max_scroll = total_lines.saturating_sub(viewport_lines);
        let desired_scroll = focus_line.saturating_sub(viewport_lines.saturating_sub(1));

        desired_scroll.min(max_scroll) as u16
    }

    fn base_model_suffix(&self) -> &'static str {
        match self.model_options {
            ModelOptionsState::Unavailable => " (fill URL + API token to load options)",
            ModelOptionsState::Idle => " (press Enter to open options)",
            ModelOptionsState::Loading => " (loading options...)",
            ModelOptionsState::Loaded(_) if self.model_picker_active => " (picker active)",
            ModelOptionsState::Loaded(_) => " (press Enter to open picker)",
            ModelOptionsState::Error(_) => " (press Enter to retry)",
        }
    }

    fn base_model_detail_text(&self) -> String {
        match &self.model_options {
            ModelOptionsState::Unavailable => {
                "Fill `BASE_API_URL` and `API_TOKEN` first, then press Enter on this row to load provider model options. Press `e` to enter a custom model manually.".to_string()
            }
            ModelOptionsState::Idle => {
                "Press Enter on this row to load provider model options, then use Up/Down on the right-panel picker and Enter to apply one. Press `e` to enter a custom model manually.".to_string()
            }
            ModelOptionsState::Loading => {
                "Loading model options from the configured provider. Wait for the request to finish, then use Up/Down on the right-panel picker and Enter to apply one.".to_string()
            }
            ModelOptionsState::Loaded(models) => {
                let mut lines = vec![
                    format!("Loaded {} model option(s).", models.len()),
                    if self.model_picker_active {
                        "Newest models are listed first. Up/Down moves the highlight and scrolls the list. Enter applies it. Esc closes the picker. Press `e` for manual model input.".to_string()
                    } else {
                        "Newest models are listed first. Press Enter to open the picker, then use Up/Down to choose a model. Press `e` for manual model input.".to_string()
                    },
                    String::new(),
                    "Available models:".to_string(),
                ];
                lines.extend(models.iter().enumerate().map(|(index, model)| {
                    let marker = if self.model_picker_active && index == self.model_highlighted {
                        ">"
                    } else {
                        " "
                    };
                    let suffix = if model == &self.base_model {
                        " (current)"
                    } else {
                        ""
                    };
                    format!("{marker} {model}{suffix}")
                }));
                lines.join("\n")
            }
            ModelOptionsState::Error(error) => format!(
                "Failed to load model options: {error}. Press Enter to retry, or press `e` to enter a custom model manually."
            ),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ConfigMode {
    Browsing,
    Editing,
}

enum ConfigSetupLoopAction {
    Continue,
    Submit(ConfigSetupAction),
    Cancel,
}

#[derive(Debug, Clone)]
enum ModelOptionsState {
    Unavailable,
    Idle,
    Loading,
    Loaded(Vec<String>),
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigField {
    Provider,
    BaseApiUrl,
    ApiToken,
    BaseModel,
    CommitStyle,
    GenerationMode,
    Save,
    Cancel,
}

impl ConfigField {
    fn all() -> &'static [ConfigField] {
        &[
            ConfigField::Provider,
            ConfigField::BaseApiUrl,
            ConfigField::ApiToken,
            ConfigField::BaseModel,
            ConfigField::CommitStyle,
            ConfigField::GenerationMode,
            ConfigField::Save,
            ConfigField::Cancel,
        ]
    }
}

fn cycle_provider(current: Provider, forward: bool) -> Provider {
    match (current, forward) {
        (Provider::Gemini, true) | (Provider::OpenAiCompatible, false) => {
            Provider::OpenAiCompatible
        }
        (Provider::OpenAiCompatible, true) | (Provider::Gemini, false) => Provider::Gemini,
    }
}

fn cycle_commit_style(current: CommitStyle, forward: bool) -> CommitStyle {
    match (current, forward) {
        (CommitStyle::Standard, true) | (CommitStyle::Conventional, false) => {
            CommitStyle::Conventional
        }
        (CommitStyle::Conventional, true) | (CommitStyle::Standard, false) => CommitStyle::Standard,
    }
}

fn cycle_generation_mode(current: GenerationMode, forward: bool) -> GenerationMode {
    match (current, forward) {
        (GenerationMode::Auto, true) | (GenerationMode::HeuristicOnly, false) => {
            GenerationMode::AiOnly
        }
        (GenerationMode::AiOnly, true) | (GenerationMode::Auto, false) => {
            GenerationMode::HeuristicOnly
        }
        (GenerationMode::HeuristicOnly, true) | (GenerationMode::AiOnly, false) => {
            GenerationMode::Auto
        }
    }
}

fn can_load_model_options(
    base_api_url: &str,
    existing_api_token: Option<&str>,
    pending_api_token: Option<&str>,
) -> bool {
    !base_api_url.trim().is_empty()
        && pending_api_token
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .or(existing_api_token
                .map(str::trim)
                .filter(|token| !token.is_empty()))
            .is_some()
}

fn make_textarea(title: &'static str, placeholder: &'static str) -> TextArea<'static> {
    let mut textarea = TextArea::default();
    textarea.set_block(Block::default().borders(Borders::ALL).title(title));
    textarea.set_line_number_style(Style::default().fg(Color::DarkGray));
    textarea.set_cursor_line_style(Style::default());
    textarea.set_placeholder_text(placeholder);
    textarea
}

fn make_textarea_with_content(title: &'static str, content: &str) -> TextArea<'static> {
    let mut textarea = if content.is_empty() {
        TextArea::default()
    } else {
        TextArea::from(content.lines().map(str::to_string).collect::<Vec<_>>())
    };
    textarea.set_block(Block::default().borders(Borders::ALL).title(title));
    textarea.set_line_number_style(Style::default().fg(Color::DarkGray));
    textarea.set_cursor_line_style(Style::default());
    textarea
}

#[derive(Debug, Default)]
struct FileTreeNode {
    children: BTreeMap<String, FileTreeNode>,
}

impl FileTreeNode {
    fn insert(&mut self, path: &str) {
        let mut node = self;
        for segment in path.split('/').filter(|segment| !segment.is_empty()) {
            node = node.children.entry(segment.to_string()).or_default();
        }
    }

    fn render(&self, prefix: String, lines: &mut Vec<Line<'static>>) {
        let len = self.children.len();
        for (index, (name, child)) in self.children.iter().enumerate() {
            let is_last = index + 1 == len;
            let connector = if is_last { "`-- " } else { "|-- " };
            let is_file = child.children.is_empty();
            let icon = if is_file { "f " } else { "d " };
            let name_style = if is_file {
                Style::default().fg(Color::White)
            } else {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            };
            lines.push(Line::from(vec![
                Span::styled(prefix.clone(), Style::default().fg(Color::DarkGray)),
                Span::styled(connector, Style::default().fg(Color::DarkGray)),
                Span::styled(icon, Style::default().fg(Color::Yellow)),
                Span::styled(name.clone(), name_style),
            ]));

            let child_prefix = format!("{prefix}{}", if is_last { "    " } else { "|   " });
            child.render(child_prefix, lines);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CommitMode, CommitView, ConfigField, ConfigMode, ConfigSetupInput, ConfigSetupLoopAction,
        ConfigSetupView, GenerationState, ModelOptionsState, build_commit_status_lines,
        can_load_model_options, handle_config_browsing_key, staged_files_tree_lines,
    };
    use crate::ai::SplitCommitPlan;
    use crate::config::{
        CommitStyle, GenerationMode, Provider, ResolvedConventionalPreset, TokenStatus,
    };
    use crossterm::event::{KeyCode, KeyEvent};

    fn plain_text(lines: Vec<ratatui::text::Line<'static>>) -> Vec<String> {
        lines
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.to_string())
                    .collect::<String>()
            })
            .collect()
    }

    fn ready_commit_view() -> CommitView {
        let mut state = CommitView::new(
            vec!["src/tui.rs".to_string()],
            CommitStyle::Standard,
            GenerationMode::Auto,
            ResolvedConventionalPreset::built_in_default(),
        );
        state.generation = GenerationState::Ready;
        state.mode = CommitMode::Browsing;
        state
    }

    fn setup_input() -> ConfigSetupInput {
        ConfigSetupInput {
            provider: Provider::Gemini,
            base_api_url: "https://example.com/v1".to_string(),
            base_model: "gemini-2.5-flash".to_string(),
            commit_style: CommitStyle::Standard,
            generation_mode: GenerationMode::Auto,
            existing_api_token: Some("token".to_string()),
            token_status: TokenStatus::Keychain,
            token_present: true,
        }
    }

    fn field_index(field: ConfigField) -> usize {
        ConfigField::all()
            .iter()
            .position(|candidate| *candidate == field)
            .expect("field should exist in config field list")
    }

    #[test]
    fn requires_url_and_token_for_model_options() {
        assert!(can_load_model_options(
            "https://example.com/v1",
            Some("token"),
            None
        ));
        assert!(!can_load_model_options("", Some("token"), None));
        assert!(!can_load_model_options(
            "https://example.com/v1",
            None,
            Some("   ")
        ));
    }

    #[test]
    fn model_options_start_idle_when_setup_can_load_them() {
        let state = ConfigSetupView::new(setup_input());
        assert!(matches!(state.model_options, ModelOptionsState::Idle));
    }

    #[test]
    fn editing_url_resets_model_options_until_prerequisites_return() {
        let mut state = ConfigSetupView::new(setup_input());
        state.selected = 1;
        state.textarea = super::make_textarea_with_content("BASE_API_URL", "");
        state.mode = ConfigMode::Editing;
        state.finish_editing(true).unwrap();

        assert!(matches!(
            state.model_options,
            ModelOptionsState::Unavailable
        ));
    }

    #[test]
    fn base_model_details_show_loaded_model_list() {
        let mut state = ConfigSetupView::new(setup_input());
        state.model_options = ModelOptionsState::Loaded(vec![
            "gemini-2.5-flash".to_string(),
            "gpt-4.1-mini".to_string(),
        ]);

        let detail = state.base_model_detail_text();

        assert!(detail.contains("Available models:"));
        assert!(detail.contains("  gemini-2.5-flash (current)"));
        assert!(detail.contains("  gpt-4.1-mini"));
    }

    #[test]
    fn enter_opens_model_picker() {
        let mut state = ConfigSetupView::new(setup_input());
        state.selected = field_index(ConfigField::BaseModel);
        state.model_options = ModelOptionsState::Loaded(vec![
            "gemini-2.5-flash".to_string(),
            "gpt-4.1-mini".to_string(),
        ]);

        let action =
            handle_config_browsing_key(&mut state, KeyEvent::from(KeyCode::Enter)).unwrap();

        assert!(matches!(action, ConfigSetupLoopAction::Continue));
        assert!(state.model_picker_active);
        assert_eq!(state.base_model, "gemini-2.5-flash");
    }

    #[test]
    fn down_moves_model_picker_highlight() {
        let mut state = ConfigSetupView::new(setup_input());
        state.selected = field_index(ConfigField::BaseModel);
        state.model_options = ModelOptionsState::Loaded(vec![
            "gemini-2.5-flash".to_string(),
            "gpt-4.1-mini".to_string(),
        ]);
        state.model_picker_active = true;

        let action = handle_config_browsing_key(&mut state, KeyEvent::from(KeyCode::Down)).unwrap();

        assert!(matches!(action, ConfigSetupLoopAction::Continue));
        assert_eq!(state.model_highlighted, 1);
        assert_eq!(state.base_model, "gemini-2.5-flash");
    }

    #[test]
    fn model_picker_scrolls_to_keep_highlight_visible() {
        let mut state = ConfigSetupView::new(setup_input());
        state.model_options = ModelOptionsState::Loaded(
            (0..8)
                .map(|index| format!("model-{index}"))
                .collect::<Vec<_>>(),
        );
        state.model_picker_active = true;
        state.model_highlighted = 6;

        assert_eq!(state.detail_scroll_offset(8), 5);
    }

    #[test]
    fn enter_applies_highlighted_model() {
        let mut state = ConfigSetupView::new(setup_input());
        state.selected = field_index(ConfigField::BaseModel);
        state.model_options = ModelOptionsState::Loaded(vec![
            "gemini-2.5-flash".to_string(),
            "gpt-4.1-mini".to_string(),
        ]);
        state.model_picker_active = true;
        state.model_highlighted = 1;

        let action =
            handle_config_browsing_key(&mut state, KeyEvent::from(KeyCode::Enter)).unwrap();

        assert!(matches!(action, ConfigSetupLoopAction::Continue));
        assert_eq!(state.base_model, "gpt-4.1-mini");
        assert!(!state.model_picker_active);
    }

    #[test]
    fn esc_closes_model_picker_before_canceling_setup() {
        let mut state = ConfigSetupView::new(setup_input());
        state.selected = field_index(ConfigField::BaseModel);
        state.model_options = ModelOptionsState::Loaded(vec![
            "gemini-2.5-flash".to_string(),
            "gpt-4.1-mini".to_string(),
        ]);
        state.model_picker_active = true;
        state.model_highlighted = 1;

        let action = handle_config_browsing_key(&mut state, KeyEvent::from(KeyCode::Esc)).unwrap();

        assert!(matches!(action, ConfigSetupLoopAction::Continue));
        assert!(!state.model_picker_active);
        assert_eq!(state.model_highlighted, 0);
    }

    #[test]
    fn esc_cancels_config_setup() {
        let mut state = ConfigSetupView::new(setup_input());

        let action = handle_config_browsing_key(&mut state, KeyEvent::from(KeyCode::Esc)).unwrap();

        assert!(matches!(action, ConfigSetupLoopAction::Cancel));
    }

    #[test]
    fn cancel_row_cancels_config_setup() {
        let mut state = ConfigSetupView::new(setup_input());
        state.selected = super::ConfigField::all().len() - 1;

        let action =
            handle_config_browsing_key(&mut state, KeyEvent::from(KeyCode::Enter)).unwrap();

        assert!(matches!(action, ConfigSetupLoopAction::Cancel));
    }

    #[test]
    fn renders_staged_files_as_tree() {
        let lines = plain_text(staged_files_tree_lines(&[
            "src/tui.rs".to_string(),
            "src/app.rs".to_string(),
            "tests/git_flow.rs".to_string(),
        ]));

        assert_eq!(
            lines,
            vec![
                "|-- d src".to_string(),
                "|   |-- f app.rs".to_string(),
                "|   `-- f tui.rs".to_string(),
                "`-- d tests".to_string(),
                "    `-- f git_flow.rs".to_string(),
            ]
        );
    }

    #[test]
    fn keeps_all_staged_files_in_tree() {
        let lines = plain_text(staged_files_tree_lines(&[
            "a.rs".to_string(),
            "b.rs".to_string(),
            "c.rs".to_string(),
            "d.rs".to_string(),
        ]));

        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], "|-- f a.rs");
        assert_eq!(lines[3], "`-- f d.rs");
    }

    #[test]
    fn shows_split_suggestions_in_commit_status() {
        let mut state = ready_commit_view();
        state.split_plans = vec![
            SplitCommitPlan {
                message: "feat(billing): add billing summary card".to_string(),
                files: vec!["src/billing.rs".to_string()],
            },
            SplitCommitPlan {
                message: "fix(subscription): handle null subscription status".to_string(),
                files: vec!["src/subscription.rs".to_string()],
            },
        ];

        let lines = plain_text(build_commit_status_lines(&state));

        assert!(
            lines.contains(
                &"Staged changes look mixed. Press s to let gitgud split them.".to_string()
            )
        );
        assert!(lines.contains(&"Split plan:".to_string()));
        assert!(lines.contains(&"  - feat(billing): add billing summary card".to_string()));
        assert!(
            lines.contains(&"  - fix(subscription): handle null subscription status".to_string())
        );
    }

    #[test]
    fn omits_split_section_when_not_needed() {
        let state = ready_commit_view();

        let lines = plain_text(build_commit_status_lines(&state));

        assert!(lines.contains(
            &"Choose an option, edit if needed, then press Enter to commit.".to_string()
        ));
        assert!(!lines.contains(&"Split plan:".to_string()));
    }
}
