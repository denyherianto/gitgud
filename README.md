# Git Buddy

`git-buddy` is a Rust CLI that adds a terminal UI and AI-assisted workflows on top of normal Git commands.

## Features

- Generate commit messages from the staged diff
- Review and edit the suggested message before committing
- Push the current branch to its upstream automatically
- Choose a remote in the TUI when the first push is ambiguous
- Use Gemini by default through an OpenAI-compatible API surface
- Validate local setup with a `doctor` command

## Requirements

- Rust toolchain
- `git` installed and available on `PATH`
- An API token for your OpenAI-compatible provider

## Configuration

`git-buddy` now supports persistent global config plus secure token storage.

Recommended setup:

```bash
cargo run -- auth login
cargo run -- config set base-api-url https://generativelanguage.googleapis.com/v1beta/openai
cargo run -- config set base-model gemini-2.5-flash
```

What this does:

- stores the API token in the system keychain
- stores non-secret defaults in the standard per-user config directory
- keeps environment variables available for one-off overrides

Use `git-buddy config show` to see the exact config file path on your platform.

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
cargo run -- --help
cargo run -- auth status
cargo run -- config show
cargo run -- doctor
cargo run -- commit
cargo run -- push
```

After building:

```bash
cargo build --release
./target/release/git-buddy
./target/release/git-buddy commit
./target/release/git-buddy push
```

## Command Behavior

### `git-buddy`

Opens the home TUI and shows:

- current branch
- staged file count
- unstaged file count
- remote and upstream status

Keys:

- `c` commit staged changes
- `p` push current branch
- `q` quit

### `git-buddy commit`

- requires staged changes
- sends the staged diff to the configured AI provider
- shows the generated commit message in a TUI editor
- commits only after confirmation

Keys:

- `r` regenerate
- `e` enter edit mode
- `Enter` confirm commit
- `Esc` cancel
- `Ctrl-S` leave edit mode

### `git-buddy push`

- pushes to the configured upstream if one already exists
- otherwise uses `origin` when available
- otherwise uses the only remote if there is exactly one
- otherwise shows a remote picker

### `git-buddy doctor`

Checks:

- `git` availability
- whether the current directory is a Git repository
- AI token availability
- config and override resolution
- reachability of `BASE_API_URL`

## Notes

- Only staged changes are used to generate the commit message.
- `git-buddy` shells out to the system `git` binary, so hooks, credentials, and your normal Git config still apply.
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
