use std::{
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
    ai::{AiClient, PromptInput, validate_commit_message},
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

pub enum PushAction {
    Confirmed(PushPlan),
    Cancelled,
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

pub async fn run_commit(repo: &GitRepo, ai: AiClient) -> Result<CommitAction> {
    let branch = repo.current_branch().or_else(|error| {
        show_message("Cannot Commit", &error.to_string())?;
        Err(error)
    })?;
    let staged = repo.staged_changes()?;

    if staged.staged_files.is_empty() {
        let message = "No staged changes found. Stage files before generating a commit message.";
        show_message("Cannot Commit", message)?;
        bail!(message);
    }

    let input = PromptInput {
        branch,
        staged_files: staged.staged_files.clone(),
        diff_stat: staged.diff_stat,
        diff: staged.diff,
    };

    let mut state = CommitView::new(input.staged_files.clone());
    state.start_generation(ai, input);
    let action = with_terminal(|terminal| commit_loop(terminal, &mut state))?;

    match action {
        CommitAction::Confirmed(message) => Ok(CommitAction::Confirmed(message)),
        CommitAction::Cancelled => Ok(CommitAction::Cancelled),
    }
}

pub fn run_push(repo: &GitRepo) -> Result<PushAction> {
    let plan = repo.plan_push().or_else(|error| {
        show_message("Cannot Push", &error.to_string())?;
        Err(error)
    })?;

    if let PushPlan::ChooseRemote { remotes, branch } = &plan {
        if remotes.is_empty() {
            let message = "No remotes found for this repository.";
            show_message("Cannot Push", message)?;
            bail!(message);
        }

        with_terminal(|terminal| push_picker_loop(terminal, remotes, branch))
    } else {
        with_terminal(|terminal| push_confirm_loop(terminal, &plan))
    }
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
                        if let Some(result) = state.take_confirmed_message()? {
                            return Ok(CommitAction::Confirmed(result));
                        }
                        if state.cancelled {
                            return Ok(CommitAction::Cancelled);
                        }
                    }
                }
                CommitMode::Editing => handle_commit_editing_key(state, key)?,
            }
        }
    }
}

fn push_confirm_loop(terminal: &mut AppTerminal, plan: &PushPlan) -> Result<PushAction> {
    loop {
        terminal.draw(|frame| draw_push_confirm(frame, plan))?;

        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Enter => return Ok(PushAction::Confirmed(plan.clone())),
                KeyCode::Esc => return Ok(PushAction::Cancelled),
                _ => {}
            }
        }
    }
}

fn push_picker_loop(
    terminal: &mut AppTerminal,
    remotes: &[String],
    branch: &str,
) -> Result<PushAction> {
    let mut state = ListState::default().with_selected(Some(0));

    loop {
        terminal.draw(|frame| draw_push_picker(frame, remotes, branch, &state))?;

        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Up => {
                    let current = state.selected().unwrap_or(0);
                    let next = current.saturating_sub(1);
                    state.select(Some(next));
                }
                KeyCode::Down => {
                    let current = state.selected().unwrap_or(0);
                    let next = (current + 1).min(remotes.len().saturating_sub(1));
                    state.select(Some(next));
                }
                KeyCode::Enter => {
                    let index = state.selected().unwrap_or(0);
                    let remote = remotes
                        .get(index)
                        .ok_or_else(|| anyhow!("invalid remote selection"))?;
                    return Ok(PushAction::Confirmed(PushPlan::SetUpstream {
                        remote: remote.clone(),
                        branch: branch.to_string(),
                    }));
                }
                KeyCode::Esc => return Ok(PushAction::Cancelled),
                _ => {}
            }
        }
    }
}

fn handle_commit_browsing_key(state: &mut CommitView, key: KeyEvent) -> Result<bool> {
    match key.code {
        KeyCode::Char('r') => {
            state.restart_generation()?;
            Ok(false)
        }
        KeyCode::Char('e') => {
            if matches!(state.generation, GenerationState::Ready) {
                state.mode = CommitMode::Editing;
                state.textarea.set_cursor_line_style(Style::default());
            }
            Ok(false)
        }
        KeyCode::Enter => {
            let message = validate_commit_message(&state.textarea.lines().join("\n"))?;
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

fn handle_commit_editing_key(state: &mut CommitView, key: KeyEvent) -> Result<()> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
        state.mode = CommitMode::Browsing;
        return Ok(());
    }

    if key.code == KeyCode::Esc {
        state.mode = CommitMode::Browsing;
        return Ok(());
    }

    let input = key_event_to_textarea_input(key);
    state.textarea.input(input);
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
    .block(Block::default().borders(Borders::ALL).title("Git Buddy"))
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
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let status_text = match &state.generation {
        GenerationState::Loading => "Generating commit message from staged diff...",
        GenerationState::Ready => "Review the message, press Enter to commit, or e to edit.",
        GenerationState::Error(error) => error,
    };

    let status_block = Paragraph::new(vec![
        Line::from(format!("Staged files: {}", state.staged_files.join(", "))),
        Line::from(status_text),
    ])
    .block(Block::default().borders(Borders::ALL).title("Commit Draft"))
    .wrap(Wrap { trim: false });

    let help_text = match state.mode {
        CommitMode::Browsing => "r regenerate  e edit  Enter commit  Esc cancel".to_string(),
        CommitMode::Editing => "Editing mode: type freely, Ctrl-S or Esc to return".to_string(),
    };

    let help = Paragraph::new(help_text)
        .block(Block::default().borders(Borders::ALL).title("Keys"))
        .alignment(Alignment::Center);

    frame.render_widget(status_block, layout[0]);
    frame.render_widget(&state.textarea, layout[1]);
    frame.render_widget(help, layout[2]);

    if matches!(state.mode, CommitMode::Editing) {
        let (x, y) = state.textarea.cursor();
        frame.set_cursor_position((x as u16, y as u16));
    }
}

fn draw_push_confirm(frame: &mut ratatui::Frame<'_>, plan: &PushPlan) {
    let message = match plan {
        PushPlan::Upstream { branch } => {
            format!("Push branch '{branch}' to its configured upstream?")
        }
        PushPlan::SetUpstream { remote, branch } => {
            format!("Push branch '{branch}' and set upstream to '{remote}'?")
        }
        PushPlan::ChooseRemote { .. } => "Choose a remote".to_string(),
    };

    draw_message(frame, "Push Confirmation", &message);
}

fn draw_push_picker(
    frame: &mut ratatui::Frame<'_>,
    remotes: &[String],
    branch: &str,
    state: &ListState,
) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let header = Paragraph::new(format!("Select a remote for branch '{branch}'"))
        .block(Block::default().borders(Borders::ALL).title("Push"))
        .alignment(Alignment::Center);

    let items = remotes
        .iter()
        .map(|remote| ListItem::new(remote.clone()))
        .collect::<Vec<_>>();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Remotes"))
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan))
        .highlight_symbol("> ");

    let help = Paragraph::new("Up/Down move  Enter select  Esc cancel")
        .block(Block::default().borders(Borders::ALL).title("Keys"))
        .alignment(Alignment::Center);

    frame.render_widget(header, layout[0]);
    frame.render_stateful_widget(list, layout[1], &mut state.clone());
    frame.render_widget(help, layout[2]);
}

fn draw_message(frame: &mut ratatui::Frame<'_>, title: &str, message: &str) {
    let popup = centered_rect(70, 30, frame.area());
    frame.render_widget(Clear, popup);

    let paragraph = Paragraph::new(message)
        .block(Block::default().borders(Borders::ALL).title(title))
        .alignment(Alignment::Center)
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
        KeyCode::Enter => Input {
            key: Key::Enter,
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
    receiver: Option<Receiver<Result<String>>>,
    generator: Option<(AiClient, PromptInput)>,
    confirmed_message: Option<String>,
    cancelled: bool,
}

impl CommitView {
    fn new(staged_files: Vec<String>) -> Self {
        let mut textarea = TextArea::default();
        textarea.set_block(Block::default().borders(Borders::ALL).title("Message"));
        textarea.set_line_number_style(Style::default().fg(Color::DarkGray));
        textarea.set_cursor_line_style(Style::default());
        textarea.set_placeholder_text("Generating commit message...");
        Self {
            textarea,
            generation: GenerationState::Loading,
            mode: CommitMode::Browsing,
            staged_files,
            receiver: None,
            generator: None,
            confirmed_message: None,
            cancelled: false,
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
        self.textarea = TextArea::default();
        self.textarea
            .set_block(Block::default().borders(Borders::ALL).title("Message"));
        self.textarea
            .set_line_number_style(Style::default().fg(Color::DarkGray));
        self.textarea
            .set_placeholder_text("Generating commit message...");

        tokio::spawn(async move {
            let result = ai.generate_commit_message(&input).await;
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
                    Ok(message) => {
                        self.textarea =
                            TextArea::from(message.lines().map(str::to_string).collect::<Vec<_>>());
                        self.textarea
                            .set_block(Block::default().borders(Borders::ALL).title("Message"));
                        self.textarea
                            .set_line_number_style(Style::default().fg(Color::DarkGray));
                        self.textarea.set_placeholder_text("Edit commit message");
                        self.generation = GenerationState::Ready;
                    }
                    Err(error) => {
                        self.generation = GenerationState::Error(error.to_string());
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

    fn take_confirmed_message(&mut self) -> Result<Option<String>> {
        Ok(self.confirmed_message.take())
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
