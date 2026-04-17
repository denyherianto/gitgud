# gitgud

`gitgud` is a Rust CLI that adds a terminal UI and AI-assisted workflows on top of normal Git commands.

<p align="center">
  <img src="./assets/gitgud-mascot.jpg" alt="gitgud repo mascot" width="280">
</p>

## Install

Install the latest GitHub release:

```bash
curl -fsSL https://raw.githubusercontent.com/denyherianto/gitgud/main/install.sh | sh
```

Install a specific release:

```bash
curl -fsSL https://raw.githubusercontent.com/denyherianto/gitgud/main/install.sh | sh -s -- --version v0.1.0
```

The installer expects GitHub release assets named like:

- `gg-darwin-arm64.tar.gz`
- `gg-darwin-x86_64.tar.gz`
- `gg-linux-arm64.tar.gz`
- `gg-linux-x86_64.tar.gz`
- `gg-windows-x86_64.zip`

## Features

- Generate 1-3 commit message options from the staged diff
- Use `auto`, `ai-only`, or `heuristic-only` commit generation modes
- Explain the staged diff with what changed, likely intent, risks, and test ideas
- Detect mixed staged concerns and let you approve file-based split commits in the TUI
- Choose an option in the TUI and edit it inline before committing
- Support standard and Conventional Commits styles, including configurable presets
- Push the current branch to its upstream automatically
- Offer `--force-with-lease` only after explicit confirmation
- **Ask questions in natural language** — describe what you want to do and get exact Git command(s) with risk ratings, explanations, and alternatives
- Configure provider, endpoint, model, token, and commit style in one setup screen
- Load provider model options in setup after entering a base URL and API token
- Use Gemini by default, or any OpenAI-compatible provider
- Pass through normal Git commands like `status`, `log`, `diff`, and `branch`
- Route bare natural language input (unrecognized as a Git subcommand) automatically to `ask`
- Validate local setup with a `doctor` command

## Commands

| Command | Description |
|---------|-------------|
| `gg` | Open the home TUI — shows branch, staged/unstaged counts, and remote status |
| `gg commit` | Generate 1–3 AI commit message options from the staged diff and commit after selection |
| `gg explain` | Explain the staged diff: what changed, likely intent, risk areas, and test suggestions |
| `gg push` | Push the current branch; offers `--force-with-lease` only after explicit confirmation |
| `gg ask <query>` | Describe what you want in plain English and get exact Git command(s) with risk ratings and alternatives |
| `gg config` | Open the interactive setup screen to configure provider, model, token, and commit style |
| `gg config show` | Print the resolved configuration and its sources |
| `gg config set <key> <value>` | Set a single config value (e.g. `commit-style`, `generation-mode`, `conventional-preset`) |
| `gg config unset <key>` | Remove a config value and fall back to the default |
| `gg auth login` | Store an API token in the system keychain |
| `gg auth status` | Show whether an API token is available and where it comes from |
| `gg auth logout` | Remove the stored API token from the keychain |
| `gg doctor` | Check git availability, repo state, AI token, config resolution, and provider reachability |
| `gg git <args>` | Pass a command straight to `git`, bypassing `gg` routing (e.g. `gg git commit --amend`) |
| `gg <git-subcommand>` | Unknown subcommands that match a known Git name are passed through directly |
| `gg <natural language>` | Unrecognized input that does not match a Git subcommand is routed to `gg ask` automatically |

## Requirements

- Rust toolchain
- `git` installed and available on `PATH`
- An API token for AI-backed commit generation or `gg explain`

## Configuration

`gitgud` supports persistent global config plus secure token storage.

Recommended setup:

```bash
cargo run --bin gg -- config
```

What this does:

- lets you choose `gemini` or `openai-compatible`
- lets you open provider-reported model options with `Enter`, navigate them with `Up`/`Down`, and apply one with `Enter` after `BASE_API_URL` and `API_TOKEN` are available
- lets you choose `auto`, `ai-only`, or `heuristic-only` commit generation
- stores the API token in the system keychain
- stores non-secret defaults in the standard per-user config directory
- keeps environment variables available for one-off overrides

Use `gg config show` to see the exact config file path on your platform.

Current precedence:

1. Environment variables
2. Global config file
3. Built-in defaults

Supported environment overrides:

- `API_TOKEN`
- `BASE_API_URL`
- `BASE_MODEL`
- `AI_TIMEOUT_SECS` — override the AI request timeout (default: 60 seconds); useful for slow or remote providers

### Conventional Commit Presets

`conventional` mode supports this built-in default preset:

- `feat`
- `fix`
- `refactor`
- `docs`
- `test`
- `chore`
- `perf`
- `build`
- `ci`

You can also define custom team presets in the config file and select one with:

```bash
gg config set conventional-preset team
gg config set generation-mode heuristic-only
gg config unset conventional-preset
```

Example config:

```toml
commit_style = "conventional"
generation_mode = "auto"

[conventional_commits]
preset = "team"

[conventional_commits.presets.team]
types = ["feature", "bugfix", "maintenance"]
```

## Usage

Build or run with Cargo:

```bash
cargo run --bin gg -- --help
cargo run --bin gg -- config
cargo run --bin gg -- config show
cargo run --bin gg -- config set conventional-preset team
cargo run --bin gg -- config set generation-mode ai-only
cargo run --bin gg -- auth status
cargo run --bin gg -- doctor
cargo run --bin gg -- commit
cargo run --bin gg -- explain
cargo run --bin gg -- push
cargo run --bin gg -- ask "undo last commit but keep changes"
cargo run --bin gg -- ask "how do I squash the last 3 commits"
cargo run --bin gg -- "unstage package.json"
cargo run --bin gg -- status --short
cargo run --bin gg -- log --oneline -5
cargo run --bin gg -- git commit --amend
```

After building:

```bash
cargo build --release
./target/release/gg
./target/release/gg commit
./target/release/gg explain
./target/release/gg push
./target/release/gg ask "undo last commit but keep changes"
```

## Command Behavior

### `gg`

Opens the home TUI and shows:

- current branch
- staged file count
- unstaged file count
- remote and upstream status

Keys:

- `c` commit staged changes
- `p` push current branch
- `q` quit

### `gg ask`

Describe what you want to do in plain English and get suggested Git command(s) with risk ratings, an explanation, and an alternative approach.

```bash
gg ask "undo last commit but keep changes"
gg ask "squash the last 3 commits into one"
gg ask "how do I move a file to another branch"
gg ask how do I see what changed in the last commit
```

Multi-word queries work with or without quotes. `gg` also accepts bare natural language when the first word is not a known Git subcommand:

```bash
gg "unstage package.json"
gg undo my last commit
```

The TUI screen shows:
- **Recommended** command(s) with a color-coded risk badge: `[SAFE]` (green), `[MED ]` (yellow), `[RISK]` (red)
- **Alternative** approach (if any) with its own badge
- **Explanation** of why the recommended approach works
- **Teaching note** explaining the underlying Git concept

Risk levels:
- **Safe** — read-only or non-destructive: `status`, `log`, `diff`, `fetch`, `add`, `push` (normal)
- **Medium** — reversible but consequential: `reset` (soft/mixed), `stash`, `merge`, `rebase` (non-main), `commit --amend`, `push --force-with-lease`
- **Dangerous** — hard to undo: `reset --hard`, `push --force`/`-f`, `clean -f`, `branch -D`, `checkout .`, `restore .`, `rebase main`

Dangerous commands require an extra confirmation dialog before executing.

Keys:

- `Enter` run recommended command(s)
- `2` run alternative command(s) (when available)
- `Esc`/`q` cancel

### Other Git Commands

Unknown commands that match a known Git subcommand are passed straight through to Git:

```bash
gg status
gg diff --cached
gg log --oneline -10
gg branch
```

To force raw Git for a built-in name that `gg` already uses, call `git` explicitly:

```bash
gg git commit --amend
gg git push --force-with-lease
```

### `gg commit`

- requires staged changes
- reads the staged diff
- warns before generation when the staged diff looks unsafe, including `.env` secrets, private keys, huge generated files, minified blobs, lockfile-only changes, and console.log spam
- uses the configured generation mode:
  - `auto`: asks the configured AI provider for 1-3 commit message options and falls back to heuristic options on timeout
  - `ai-only`: asks the configured AI provider and surfaces provider errors instead of falling back
  - `heuristic-only`: skips the AI provider and generates local heuristic options only
- shortens overlong AI-generated subjects to fit the 72-character subject limit
- shows when the staged diff looks like multiple concerns and offers a file-based split commit plan in the TUI
- lets you choose one option in the TUI
- supports inline editing before commit
- respects the configured commit style, including Conventional Commits presets
- commits only after confirmation
- can create the proposed split commits after explicit approval with `s`

Keys:

- `Up`/`Down` choose an option
- `r` regenerate
- `e` enter edit mode
- `s` approve the proposed split plan and create separate commits
- `Enter` confirm commit
- `Esc` cancel
- `Ctrl-S` leave edit mode

### `gg explain`

- requires staged changes
- reads the staged diff
- asks the configured AI provider to explain the change in four sections
- prints:
  - what changed
  - possible intent
  - risk areas
  - test suggestions

### `gg push`

- detects the current branch
- checks whether an upstream already exists
- warns before pushing when the outgoing diff looks unsafe, using the same secret, generated-file, minified-blob, lockfile-only, and console.log checks
- pushes immediately when the target is unambiguous
- offers `--force-with-lease` only after explicit confirmation if the normal push is rejected
- errors when the first push target is ambiguous instead of guessing across multiple remotes

### `gg config`

- opens a setup screen by default
- configures:
  - provider: `gemini` or `openai-compatible`
  - `BASE_API_URL`
  - `BASE_MODEL`
    Load available model options with `Enter`, navigate them with `Up`/`Down`, and apply one with `Enter` after `BASE_API_URL` and `API_TOKEN` are filled, or press `e` to type a custom model manually
  - `API_TOKEN`
  - commit style preference
  - commit generation mode: `auto`, `ai-only`, or `heuristic-only`
  - active Conventional Commit preset selection
- stores non-secret settings in the config file
- stores `API_TOKEN` in the system keychain
- preserves custom Conventional Commit preset definitions already stored in the config file

### `gg doctor`

Checks:

- `git` availability
- whether the current directory is a Git repository
- AI token availability when AI-backed features are configured
- provider, config, and override resolution
- reachability of `BASE_API_URL`

## Notes

- Only staged changes are used to generate the commit message.
- `gitgud` shells out to the system `git` binary, so hooks, credentials, and your normal Git config still apply.
- Detached HEAD is rejected for commit and push flows.

## Testing

```bash
cargo fmt
cargo test
```

## Open Source

- Contributor note: keep `README.md` updated in the same change as any user-visible behavior or workflow change, and add or update tests for behavior changes where appropriate. Agent contributor guidance for Codex, Claude Code, and similar tools lives in `AGENTS.md`.
- License: MIT, see `LICENSE`
- Contributions: see `CONTRIBUTING.md`
- Community expectations: see `CODE_OF_CONDUCT.md`
