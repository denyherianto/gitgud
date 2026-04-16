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
        AiClient, CommitSuggestions, PromptInput, normalize_base_api_url,
        validate_commit_message_with_preset,
    },
    config::{CommitStyle, Provider, ResolvedConventionalPreset, TokenStatus},
    git::{GitRepo, PushPlan, RepoStatus},
};

type Backend = CrosstermBackend<Stdout>;
type AppTerminal = Terminal<Backend>;

pub enum HomeAction {
    Commit,
    Push,
    Quit,
}

pub enum CommitAction {
    Confirmed(String),
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct ConfigSetupInput {
    pub provider: Provider,
    pub base_api_url: String,
    pub base_model: String,
    pub commit_style: CommitStyle,
    pub token_status: TokenStatus,
    pub token_present: bool,
}

#[derive(Debug, Clone)]
pub struct ConfigSetupAction {
    pub provider: Provider,
    pub base_api_url: String,
    pub base_model: String,
    pub commit_style: CommitStyle,
    pub api_token: Option<String>,
}

pub fn run_home(status: &RepoStatus) -> Result<HomeAction> {
    with_terminal(|terminal| {
        loop {
            terminal.draw(|frame| draw_home(frame, status))?;

            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('c') => return Ok(HomeAction::Commit),
                    KeyCode::Char('p') => return Ok(HomeAction::Push),
                    KeyCode::Esc | KeyCode::Char('q') => return Ok(HomeAction::Quit),
                    _ => {}
                }
            }
        }
    })
}

pub async fn run_commit(
    repo: &GitRepo,
    ai: AiClient,
    commit_style: CommitStyle,
    conventional_preset: ResolvedConventionalPreset,
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
    };

    let mut state = CommitView::new(
        input.staged_files.clone(),
        commit_style,
        conventional_preset,
    );
    state.start_generation(ai, input);
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
        terminal.draw(|frame| draw_config_setup(frame, state))?;

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }

        if let Event::Key(key) = event::read()? {
            match state.mode {
                ConfigMode::Browsing => {
                    if let Some(action) = handle_config_browsing_key(state, key)? {
                        return Ok(Some(action));
                    }
                }
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
) -> Result<Option<ConfigSetupAction>> {
    match key.code {
        KeyCode::Up => state.move_selection(-1),
        KeyCode::Down => state.move_selection(1),
        KeyCode::Left => state.cycle_selected(false),
        KeyCode::Right => state.cycle_selected(true),
        KeyCode::Char('e') => state.begin_editing_selected(),
        KeyCode::Char('s') => match state.save() {
            Ok(action) => return Ok(action),
            Err(error) => state.error = Some(error.to_string()),
        },
        KeyCode::Enter => match state.selected_field() {
            ConfigField::Provider | ConfigField::CommitStyle => state.cycle_selected(true),
            ConfigField::BaseApiUrl | ConfigField::BaseModel | ConfigField::ApiToken => {
                state.begin_editing_selected()
            }
            ConfigField::Save => match state.save() {
                Ok(action) => return Ok(action),
                Err(error) => state.error = Some(error.to_string()),
            },
            ConfigField::Cancel => return Ok(None),
        },
        KeyCode::Esc => return Ok(None),
        _ => {}
    }

    Ok(None)
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
        Line::from("c  commit staged changes"),
        Line::from("p  push current branch"),
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
        GenerationState::Ready if state.split_suggestions.is_empty() => {
            "Choose an option, edit if needed, then press Enter to commit."
        }
        GenerationState::Ready => {
            "Staged changes look mixed. Consider splitting them before committing."
        }
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

    let mut lines = vec![
        Line::from(style_line),
        Line::from(format!("Staged count: {}", state.staged_files.len())),
        Line::from("Left/Right scroll staged tree  PgUp/PgDn jump  Home/End edges"),
        Line::from(status_text.to_string()),
    ];

    if matches!(state.commit_style, CommitStyle::Conventional) {
        lines.insert(
            1,
            Line::from(format!(
                "Allowed types: {}",
                state.conventional_preset.types.join(", ")
            )),
        );
    }

    if matches!(state.generation, GenerationState::Ready) && !state.split_suggestions.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "Suggested split:",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )]));
        for suggestion in &state.split_suggestions {
            lines.push(Line::from(format!("  - {suggestion}")));
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
        Line::from("Configure provider, endpoint, model, token, and commit style."),
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

    let detail: Paragraph = if matches!(state.mode, ConfigMode::Editing) {
        Paragraph::new(state.current_editor_help())
            .block(Block::default().borders(Borders::ALL).title("Edit Help"))
            .wrap(Wrap { trim: false })
    } else {
        Paragraph::new(state.detail_text())
            .block(Block::default().borders(Borders::ALL).title("Details"))
            .wrap(Wrap { trim: false })
    };

    let help_text = match state.mode {
        ConfigMode::Browsing => {
            "Up/Down move  Left/Right cycle choices  Enter edit/select  s save  Esc cancel"
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
    mode: CommitMode,
    staged_files: Vec<String>,
    receiver: Option<Receiver<Result<CommitSuggestions>>>,
    generator: Option<(AiClient, PromptInput)>,
    options: Vec<String>,
    drafts: Vec<String>,
    split_suggestions: Vec<String>,
    list_state: ListState,
    staged_scroll: usize,
    confirmed_message: Option<String>,
    cancelled: bool,
    commit_style: CommitStyle,
    conventional_preset: ResolvedConventionalPreset,
}

impl CommitView {
    fn new(
        staged_files: Vec<String>,
        commit_style: CommitStyle,
        conventional_preset: ResolvedConventionalPreset,
    ) -> Self {
        Self {
            textarea: make_textarea("Draft", "Generating commit message options..."),
            generation: GenerationState::Loading,
            mode: CommitMode::Browsing,
            staged_files,
            receiver: None,
            generator: None,
            options: Vec::new(),
            drafts: Vec::new(),
            split_suggestions: Vec::new(),
            list_state: ListState::default().with_selected(Some(0)),
            staged_scroll: 0,
            confirmed_message: None,
            cancelled: false,
            commit_style,
            conventional_preset,
        }
    }

    fn start_generation(&mut self, ai: AiClient, input: PromptInput) {
        self.generator = Some((ai.clone(), input.clone()));
        self.spawn_generation(ai, input);
    }

    fn restart_generation(&mut self) -> Result<()> {
        let (ai, input) = self
            .generator
            .clone()
            .ok_or_else(|| anyhow!("commit generation is not configured"))?;
        self.spawn_generation(ai, input);
        Ok(())
    }

    fn spawn_generation(&mut self, ai: AiClient, input: PromptInput) {
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        self.generation = GenerationState::Loading;
        self.mode = CommitMode::Browsing;
        self.options.clear();
        self.drafts.clear();
        self.split_suggestions.clear();
        self.list_state.select(Some(0));
        self.staged_scroll = 0;
        self.textarea = make_textarea("Draft", "Generating commit message options...");

        tokio::spawn(async move {
            let result = ai.generate_commit_suggestions(&input).await;
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
                        self.split_suggestions = suggestions.split;
                        self.list_state.select(Some(0));
                        self.load_selected_draft();
                        self.generation = GenerationState::Ready;
                    }
                    Err(error) => {
                        self.generation = GenerationState::Error(error.to_string());
                        self.textarea = make_textarea("Draft", "No commit draft available");
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.receiver = None;
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
    pending_api_token: String,
    token_status: TokenStatus,
    token_present: bool,
    mode: ConfigMode,
    selected: usize,
    list_state: ListState,
    textarea: TextArea<'static>,
    error: Option<String>,
}

impl ConfigSetupView {
    fn new(input: ConfigSetupInput) -> Self {
        Self {
            provider: input.provider,
            base_api_url: input.base_api_url,
            base_model: input.base_model,
            commit_style: input.commit_style,
            pending_api_token: String::new(),
            token_status: input.token_status,
            token_present: input.token_present,
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
            _ => {}
        }
    }

    fn begin_editing_selected(&mut self) {
        let (title, value) = match self.selected_field() {
            ConfigField::BaseApiUrl => ("BASE_API_URL", self.base_api_url.clone()),
            ConfigField::BaseModel => ("BASE_MODEL", self.base_model.clone()),
            ConfigField::ApiToken => ("API_TOKEN", self.pending_api_token.clone()),
            _ => return,
        };

        self.textarea = make_textarea_with_content(title, &value);
        self.mode = ConfigMode::Editing;
        self.error = None;
    }

    fn finish_editing(&mut self, save: bool) -> Result<()> {
        if save {
            let value = self.textarea.lines().join("\n").trim().to_string();
            match self.selected_field() {
                ConfigField::BaseApiUrl => self.base_api_url = value,
                ConfigField::BaseModel => self.base_model = value,
                ConfigField::ApiToken => self.pending_api_token = value,
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

        if matches!(self.token_status, TokenStatus::Missing)
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
            ConfigField::BaseApiUrl => format!("BASE_API_URL: {}", self.base_api_url),
            ConfigField::BaseModel => format!("BASE_MODEL: {}", self.base_model),
            ConfigField::ApiToken => format!("API_TOKEN: {}", self.token_label()),
            ConfigField::CommitStyle => format!("Commit Style: {}", self.commit_style),
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
            ConfigField::BaseModel => {
                "Set the model name sent to the provider.".to_string()
            }
            ConfigField::ApiToken => {
                "Enter a new API token to store it in the system keychain. Leave it blank to keep the current token.".to_string()
            }
            ConfigField::CommitStyle => {
                "Choose `standard` for plain Git subjects or `conventional` for Conventional Commits. Custom Conventional Commit presets can be selected with `gg config set conventional-preset <name>` after defining them in the config file.".to_string()
            }
            ConfigField::Save => {
                "Save the current settings. BASE_API_URL and BASE_MODEL are written to the config file; API_TOKEN is stored in the keychain.".to_string()
            }
            ConfigField::Cancel => "Exit without writing changes.".to_string(),
        }
    }

    fn current_editor_help(&self) -> String {
        match self.selected_field() {
            ConfigField::BaseApiUrl => "Edit the full BASE_API_URL value.",
            ConfigField::BaseModel => "Edit the BASE_MODEL value.",
            ConfigField::ApiToken => "Enter a replacement API token for the keychain.",
            _ => "Edit the selected field.",
        }
        .to_string()
    }

    fn status_line(&self) -> String {
        match &self.error {
            Some(error) => format!("Error: {error}"),
            None => match self.token_status {
                TokenStatus::EnvironmentOverride => {
                    "Environment token override is active; a saved token will be ignored until it is removed.".to_string()
                }
                TokenStatus::Keychain => "A token is already stored in the system keychain.".to_string(),
                TokenStatus::Missing => "No API token is currently configured.".to_string(),
            },
        }
    }

    fn apply_provider_defaults(&mut self) {
        self.base_api_url = self.provider.default_base_api_url().to_string();
        self.base_model = self.provider.default_base_model().to_string();
    }
}

#[derive(Debug, Clone, Copy)]
enum ConfigMode {
    Browsing,
    Editing,
}

#[derive(Debug, Clone, Copy)]
enum ConfigField {
    Provider,
    BaseApiUrl,
    BaseModel,
    ApiToken,
    CommitStyle,
    Save,
    Cancel,
}

impl ConfigField {
    fn all() -> &'static [ConfigField] {
        &[
            ConfigField::Provider,
            ConfigField::BaseApiUrl,
            ConfigField::BaseModel,
            ConfigField::ApiToken,
            ConfigField::CommitStyle,
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
        CommitMode, CommitView, GenerationState, build_commit_status_lines, staged_files_tree_lines,
    };
    use crate::config::{CommitStyle, ResolvedConventionalPreset};

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
            ResolvedConventionalPreset::built_in_default(),
        );
        state.generation = GenerationState::Ready;
        state.mode = CommitMode::Browsing;
        state
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
        state.split_suggestions = vec![
            "feat(billing): add billing summary card".to_string(),
            "fix(subscription): handle null subscription status".to_string(),
        ];

        let lines = plain_text(build_commit_status_lines(&state));

        assert!(lines.contains(
            &"Staged changes look mixed. Consider splitting them before committing.".to_string()
        ));
        assert!(lines.contains(&"Suggested split:".to_string()));
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
        assert!(!lines.contains(&"Suggested split:".to_string()));
    }
}
