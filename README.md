# gitgud

`gitgud` is a Rust CLI that adds a terminal UI and AI-assisted Git workflows on top of normal Git.

<p align="center">
  <img src="./assets/gitgud-mascot.jpg" alt="gitgud repo mascot" width="280">
</p>

## What It Does

- Generates 1-3 commit message options from the staged diff
- Explains staged changes with intent, risks, and test ideas
- Suggests exact Git commands from natural language requests
- Pushes the current branch with explicit confirmation before `--force-with-lease`
- Supports standard commits and Conventional Commits presets
- Uses Gemini by default, or any OpenAI-compatible API including Ollama

## Install

Latest release:

```bash
curl -fsSL https://raw.githubusercontent.com/denyherianto/gitgud/master/install.sh | sh
```

Specific release:

```bash
curl -fsSL https://raw.githubusercontent.com/denyherianto/gitgud/master/install.sh | sh -s -- --version v0.1.0
```

Requirements:

- `git` on `PATH`
- Rust toolchain if building from source
- An API token for AI-backed features unless you use `heuristic-only` mode

## Quick Start

Interactive setup:

```bash
gg config
gg doctor
```

Typical usage:

```bash
gg commit
gg explain
gg push
gg ask "undo last commit but keep changes"
gg "unstage package.json"
```

Build from source:

```bash
cargo run --bin gg -- --help
cargo run --bin gg -- config
cargo run --bin gg -- commit
```

## Commands

| Command | Description |
|---------|-------------|
| `gg` | Open the home TUI with branch, staged/unstaged counts, and remote status |
| `gg commit` | Generate commit options from staged changes and commit after confirmation |
| `gg explain` | Explain the staged diff in four sections |
| `gg push` | Push the current branch and offer `--force-with-lease` only after confirmation |
| `gg ask <query>` | Turn a natural language Git request into exact command(s) with risk guidance |
| `gg config` | Open the interactive setup screen |
| `gg config show` | Print effective config values and their sources |
| `gg config set <key> <value>` | Persist one config value |
| `gg config unset <key>` | Remove one persisted config value |
| `gg auth login` | Store an API token in the system keychain |
| `gg auth status` | Show whether an API token is available and where it comes from |
| `gg auth logout` | Remove the stored API token from the keychain |
| `gg doctor` | Check Git, repo state, token availability, config resolution, and provider reachability |
| `gg git <args>` | Pass a command straight to raw Git |
| `gg <git-subcommand>` | Unknown Git subcommands are passed through directly |
| `gg <natural language>` | Unrecognized input that is not a Git subcommand is routed to `ask` |

## AI Setup

`gitgud` stores non-secret settings in a per-user config file and stores `API_TOKEN` in the system keychain.

Config precedence:

1. Environment variables
2. Global config file
3. Built-in defaults

Environment overrides:

- `API_TOKEN`
- `BASE_API_URL`
- `BASE_MODEL`
- `AI_TIMEOUT_SECS` default `60`

Interactive setup supports:

- provider: `gemini` or `openai-compatible`
- `BASE_API_URL`
- `BASE_MODEL`
- `API_TOKEN`
- commit style: `standard` or `conventional`
- generation mode: `auto`, `ai-only`, or `heuristic-only`
- provider model loading from `/models` after `BASE_API_URL` and `API_TOKEN` are set

Use `gg config show` to print the exact config path and the source of each effective value.

### BYOK

OpenAI-compatible example:

```bash
gg config set provider openai-compatible
gg config set base-api-url https://api.openai.com/v1
gg config set base-model gpt-4.1-mini
gg auth login --token "$OPENAI_API_KEY"
gg doctor
```

One-off environment override:

```bash
export API_TOKEN="$OPENAI_API_KEY"
export BASE_API_URL="https://api.openai.com/v1"
export BASE_MODEL="gpt-4.1-mini"
gg commit
```

Notes:

- the default provider is `gemini`
- when switching providers, set `provider`, `BASE_API_URL`, and `BASE_MODEL` together
- environment `API_TOKEN` overrides the keychain

### Ollama

Ollama works through its OpenAI-compatible API:

```bash
gg config set provider openai-compatible
gg config set base-api-url http://localhost:11434/v1
gg config set base-model llama3.1:8b
gg auth login --token ollama
gg doctor
```

Or with environment variables:

```bash
export API_TOKEN="ollama"
export BASE_API_URL="http://localhost:11434/v1"
export BASE_MODEL="llama3.1:8b"
gg explain
```

Notes:

- `API_TOKEN` must be non-empty, even for local providers
- keep Ollama running if you want the model picker to load `/models`
- if Ollama uses a different host or port, use that full `/v1` base URL

## Commit Modes

- `auto`: use the configured AI provider and fall back to heuristic suggestions on timeout
- `ai-only`: require the AI provider and surface provider errors
- `heuristic-only`: skip the AI provider and generate local suggestions only

## Conventional Commits

Built-in default types:

- `feat`
- `fix`
- `refactor`
- `docs`
- `test`
- `chore`
- `perf`
- `build`
- `ci`

Custom preset example:

```toml
commit_style = "conventional"
generation_mode = "auto"

[conventional_commits]
preset = "team"

[conventional_commits.presets.team]
types = ["feature", "bugfix", "maintenance"]
```

Select or clear a preset:

```bash
gg config set conventional-preset team
gg config unset conventional-preset
```

## Behavior Notes

- `gg commit` and `gg explain` only use staged changes
- `gg commit` warns about risky staged diffs, supports inline editing, and can propose split commits for mixed concerns
- `gg ask` returns recommended and alternative commands with risk badges and explanations
- dangerous actions suggested by `gg ask` require extra confirmation before execution
- `gg push` warns about risky outgoing diffs and does not guess across ambiguous remotes
- `gitgud` shells out to the system `git`, so hooks, credentials, and normal Git config still apply
- detached HEAD is rejected for commit and push flows

## Development

Run checks:

```bash
cargo fmt
cargo test
```

Project docs:

- Contributor guidance: `AGENTS.md`
- Contributing: `CONTRIBUTING.md`
- Code of conduct: `CODE_OF_CONDUCT.md`
- License: `LICENSE`
