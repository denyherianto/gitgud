# gitgud

`gitgud` is a Rust CLI that adds a terminal UI and AI-assisted workflows on top of normal Git commands.

<p align="center">
  <img src="./assets/gitgud-repo-illustration.svg" alt="gitgud mascot icon" width="220">
</p>

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

`gitgud` supports persistent global config plus secure token storage.

Recommended setup:

```bash
cargo run --bin gg -- config
```

What this does:

- lets you choose `gemini` or `openai-compatible`
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

## Usage

Build or run with Cargo:

```bash
cargo run --bin gg -- --help
cargo run --bin gg -- config
cargo run --bin gg -- config show
cargo run --bin gg -- auth status
cargo run --bin gg -- doctor
cargo run --bin gg -- commit
cargo run --bin gg -- push
```

After building:

```bash
cargo build --release
./target/release/gg
./target/release/gg commit
./target/release/gg push
```

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

### `gg commit`

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

### `gg push`

- detects the current branch
- checks whether an upstream already exists
- pushes immediately when the target is unambiguous
- offers `--force-with-lease` only after explicit confirmation if the normal push is rejected
- errors when the first push target is ambiguous instead of guessing across multiple remotes

### `gg config`

- opens a setup screen by default
- configures:
  - provider: `gemini` or `openai-compatible`
  - `BASE_API_URL`
  - `BASE_MODEL`
  - `API_TOKEN`
  - commit style preference
- stores non-secret settings in the config file
- stores `API_TOKEN` in the system keychain

### `gg doctor`

Checks:

- `git` availability
- whether the current directory is a Git repository
- AI token availability
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

- License: MIT, see `LICENSE`
- Contributions: see `CONTRIBUTING.md`
- Community expectations: see `CODE_OF_CONDUCT.md`
