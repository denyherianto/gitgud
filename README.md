# Git Buddy

`gitbuddy` is a Rust CLI that adds a terminal UI and AI-assisted workflows on top of normal Git commands.

## Features

- Generate 3-5 commit message options from the staged diff
- Choose an option in the TUI and edit it inline before committing
- Support standard and Conventional Commits styles
- Push the current branch to its upstream automatically
- Offer `--force-with-lease` only after explicit confirmation
- Configure provider, endpoint, model, token, and commit style in one setup screen
- Use Gemini by default, or any OpenAI-compatible provider
- Validate local setup with a `doctor` command

## Requirements

- Rust toolchain
- `git` installed and available on `PATH`
- An API token for your OpenAI-compatible provider

## Configuration

`gitbuddy` supports persistent global config plus secure token storage.

Recommended setup:

```bash
cargo run --bin gitbuddy -- config
```

What this does:

- lets you choose `gemini` or `openai-compatible`
- stores the API token in the system keychain
- stores non-secret defaults in the standard per-user config directory
- keeps environment variables available for one-off overrides

Use `gitbuddy config show` to see the exact config file path on your platform.

Current precedence:

1. Environment variables
2. Global config file
3. Built-in defaults

Supported environment overrides:

- `API_TOKEN`
- `BASE_API_URL`
- `BASE_MODEL`

## Usage

Build or run with Cargo:

```bash
cargo run --bin gitbuddy -- --help
cargo run --bin gitbuddy -- config
cargo run --bin gitbuddy -- config show
cargo run --bin gitbuddy -- auth status
cargo run --bin gitbuddy -- doctor
cargo run --bin gitbuddy -- commit
cargo run --bin gitbuddy -- push
```

After building:

```bash
cargo build --release
./target/release/gitbuddy
./target/release/gitbuddy commit
./target/release/gitbuddy push
```

## Command Behavior

### `gitbuddy`

Opens the home TUI and shows:

- current branch
- staged file count
- unstaged file count
- remote and upstream status

Keys:

- `c` commit staged changes
- `p` push current branch
- `q` quit

### `gitbuddy commit`

- requires staged changes
- reads the staged diff
- asks the configured AI provider for 3-5 commit message options
- lets you choose one option in the TUI
- supports inline editing before commit
- respects the configured commit style, including Conventional Commits mode
- commits only after confirmation

Keys:

- `Up`/`Down` choose an option
- `r` regenerate
- `e` enter edit mode
- `Enter` confirm commit
- `Esc` cancel
- `Ctrl-S` leave edit mode

### `gitbuddy push`

- detects the current branch
- checks whether an upstream already exists
- pushes immediately when the target is unambiguous
- offers `--force-with-lease` only after explicit confirmation if the normal push is rejected
- errors when the first push target is ambiguous instead of guessing across multiple remotes

### `gitbuddy config`

- opens a setup screen by default
- configures:
  - provider: `gemini` or `openai-compatible`
  - `BASE_API_URL`
  - `BASE_MODEL`
  - `API_TOKEN`
  - commit style preference
- stores non-secret settings in the config file
- stores `API_TOKEN` in the system keychain

### `gitbuddy doctor`

Checks:

- `git` availability
- whether the current directory is a Git repository
- AI token availability
- provider, config, and override resolution
- reachability of `BASE_API_URL`

## Notes

- Only staged changes are used to generate the commit message.
- `gitbuddy` shells out to the system `git` binary, so hooks, credentials, and your normal Git config still apply.
- Detached HEAD is rejected for commit and push flows.

## Testing

```bash
cargo fmt
cargo test
```

## Open Source

- License: MIT, see `LICENSE`
- Contributions: see `CONTRIBUTING.md`
- Community expectations: see `CODE_OF_CONDUCT.md`
