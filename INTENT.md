# Plan: Natural Language Git Assistant (`gg ask`)

## Context

git-buddy (`gg`) is a Rust CLI that wraps Git with AI-powered commit messages, diff explanations, and safe push flows. Users often know *what* they want to do with Git but not *which command* does it safely. This feature adds a natural language interface: users describe their intent in plain English and get the exact Git command(s) suggested, explained, and optionally executed with safety guardrails.

## Overview

Add an `ask` command that:
1. Accepts natural language input (e.g., `gg ask "undo last commit but keep changes"`)
2. Uses the AI provider to interpret intent and suggest Git command(s)
3. Shows the suggestion with risk level, explanation, alternatives, and a teaching note
4. Optionally executes after confirmation, with extra gates for dangerous commands
5. Also works via bare passthrough: `gg "undo my last commit"` when the input doesn't match a known Git subcommand

## Implementation

### Phase 1: Risk classification module — `src/risk.rs` (new file)

Create a deterministic, hardcoded risk classifier for Git commands.

**Types:**
```rust
pub enum RiskLevel { Safe, Medium, Dangerous }
pub fn classify_risk(command: &str) -> RiskLevel
```

**Classification rules:**
- **Dangerous:** `reset --hard`, `push --force`/`-f` (not `--force-with-lease`), `clean -f`/`-fd`, `branch -D`, `checkout .`, `restore .`, `rebase` on main/master
- **Medium:** `reset` (soft/mixed), `stash`/`stash drop`, `pull --rebase`, `rebase` (not main), `push --force-with-lease`, `merge`, `cherry-pick`, `commit --amend`
- **Safe:** `status`, `log`, `diff`, `show`, `blame`, `branch` (list/create), `remote`, `tag`, `fetch`, `add`, `stash list`

Register in `src/lib.rs`: add `pub mod risk;`

### Phase 2: AI integration — `src/ai.rs`

Add new public types:

```rust
pub struct AskSuggestion {
    pub recommended: Vec<SuggestedCommand>,
    pub alternative: Option<Vec<SuggestedCommand>>,
    pub explanation: String,
    pub teaching_note: String,
}

pub struct SuggestedCommand {
    pub command: String,       // "git reset --soft HEAD~1"
    pub description: String,   // "Undo last commit, keep changes staged"
}

pub struct AskContext {
    pub branch: String,
    pub staged_count: usize,
    pub unstaged_count: usize,
    pub recent_log: String,
}
```

Add to `AiClient`:
```rust
pub async fn generate_ask_suggestion(&self, query: &str, context: &AskContext) -> Result<AskSuggestion>
```

Add prompt builders:
- `build_ask_system_prompt()` — instructs AI to return JSON with `recommended`, `alternative`, `explanation`, `teaching_note`. All commands must start with `git`.
- `build_ask_user_prompt(query, context)` — includes user query, branch, status counts, recent commits.
- `parse_ask_suggestion(raw)` — JSON parsing with code-fence stripping (reuse `strip_code_fence`).

**AI system prompt design:**
> You are a Git command assistant. Given a natural language description, suggest the exact git command(s). Return valid JSON: `{"recommended":[{"command":"git ...","description":"..."}],"alternative":[...],"explanation":"...","teaching_note":"..."}`. `recommended` is 1-4 commands in execution order. `alternative` is optional (null if no meaningful alternative). Every `command` must start with "git ". No markdown fences.

### Phase 3: Git helpers — `src/git.rs`

Add to `GitRepo`:

```rust
pub fn recent_log(&self, count: usize) -> Result<String>
// runs: git log --oneline -N

pub fn run_suggested_command(&self, command: &str) -> Result<String>
// Parses command string, validates starts with "git", strips "git" prefix, executes via run_raw
```

### Phase 4: CLI changes — `src/cli.rs`

Add `Ask` variant to `Command` enum:
```rust
/// Ask a question in natural language and get suggested git commands
Ask {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    query: Vec<String>,
},
```

This supports both `gg ask undo my last commit` and `gg ask "undo my last commit"`.

### Phase 5: TUI screen — `src/tui.rs`

Add ask-specific types and screen:

```rust
pub enum AskAction {
    RunRecommended,
    RunAlternative,
    Cancel,
}

pub fn run_ask(suggestion: &AskSuggestion, risk_levels: &[RiskLevel]) -> Result<AskAction>
pub fn confirm_dangerous_command(command: &str, description: &str) -> Result<bool>
```

**TUI layout:**
- **Top:** Recommended command(s) with color-coded risk badges (green=Safe, yellow=Medium, red=Dangerous) and descriptions
- **Middle:** Alternative approach (if any) with tradeoff explanation
- **Bottom:** Teaching note + keybindings: `Enter`=run recommended, `2`=run alternative, `Esc`/`q`=cancel

Follow existing patterns from `run_commit` / `run_home` for the event loop and drawing.

### Phase 6: App routing — `src/app.rs`

**Wire the `Ask` command:**
```rust
Command::Ask { query } => run_ask(&repo, &query.join(" ")).await,
```

**Add natural language detection for passthrough:**

Modify the `Command::Passthrough(args)` arm to check if the input looks like natural language rather than a git subcommand. Add `fn is_known_git_subcommand(name: &str) -> bool` with ~40 common git subcommands. If the first arg is NOT a known git subcommand, join args and route to `run_ask`.

**Implement `async fn run_ask(repo, query)`:**
1. Ensure git available + in repo
2. Gather context: branch, status, `recent_log(5)`
3. Load `AiConfig`, create `AiClient`
4. Call `generate_ask_suggestion(query, context)`
5. Classify risk for each recommended/alternative command via `risk::classify_risk`
6. Launch `tui::run_ask()` to display and get user choice
7. If user confirms, execute commands sequentially:
   - For each dangerous command, show `confirm_dangerous_command` before executing
   - On failure, halt and show error + remaining commands not executed
   - On success, print each command's output

## Files to modify

| File | Change |
|------|--------|
| `src/risk.rs` | **New** — `RiskLevel` enum + `classify_risk()` |
| `src/lib.rs` | Add `pub mod risk;` |
| `src/ai.rs` | Add `AskSuggestion`, `SuggestedCommand`, `AskContext`, `generate_ask_suggestion()`, prompt builders, parser |
| `src/git.rs` | Add `recent_log()`, `run_suggested_command()` |
| `src/cli.rs` | Add `Ask` variant to `Command` |
| `src/tui.rs` | Add `run_ask()` screen, `AskAction`, `confirm_dangerous_command()` |
| `src/app.rs` | Add `run_ask()`, `is_known_git_subcommand()`, modify passthrough routing |

## Verification

1. `cargo build` — must compile cleanly
2. `cargo test` — existing + new unit tests pass
3. Manual test: `gg ask "undo last commit but keep changes"` → shows suggestion with risk badge
4. Manual test: `gg "unstage package.json"` → routes to ask (not git passthrough)
5. Manual test: `gg status` → still passes through to git (not treated as NL)
6. Manual test: dangerous command suggestion → requires confirmation dialog
7. Manual test: multi-command suggestion → executes sequentially, halts on error
