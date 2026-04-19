# gitgud

`gitgud` is a Rust CLI that adds a terminal UI and guided Git recovery, commit, and push workflows on top of normal Git.

<p align="center">
  <img src="./assets/gitgud-mascot.jpg" alt="gitgud repo mascot" width="280">
</p>

## What It Does

- Rescues wrong-branch commits, detached HEAD, bad rebases, lost stashes, accidental resets, and force-push mistakes
- Generates 1-3 commit message options from the staged diff
- Explains staged changes with intent, risks, and test ideas
- Captures commit-level Git memory with structured summaries, likely intent, feature labels, and related files
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
gg version
gg rescue
gg ship
gg commit
gg explain
gg memory install
gg memory explain HEAD~1
gg memory search billing
gg memory impact src/app.rs
gg memory stale
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
| `gg version` | Print the installed gitgud version |
| `gg rescue [incident]` | Diagnose common Git mistakes, preview recovery steps, create a safety snapshot, and save rollback notes |
| `gg commit` | Generate commit options from staged changes and commit after confirmation |
| `gg ship` | Run one ship flow: preflight, commit cleanup suggestions, review draft generation, and push |
| `gg explain` | Explain the staged diff in four sections |
| `gg push` | Push the current branch and offer `--force-with-lease` only after confirmation |
| `gg ask <query>` | Turn a natural language Git request into exact command(s) with risk guidance |
| `gg config` | Open the interactive setup screen and install the Git-memory hook when you save inside a repo |
| `gg config show` | Print effective config values and their sources |
| `gg config set <key> <value>` | Persist one config value |
| `gg config unset <key>` | Remove one persisted config value |
| `gg auth login` | Store an API token in the system keychain |
| `gg auth status` | Show whether an API token is available and where it comes from |
| `gg auth logout` | Remove the stored API token from the keychain |
| `gg doctor` | Check Git, repo state, token availability, config resolution, and provider reachability |
| `gg learn` | Rebuild repo-specific memory and backfill commit-intelligence memory for the last 50 commits |
| `gg memory install` | Install or refresh the managed `post-commit` hook for commit-memory capture |
| `gg memory learn` | Backfill commit-intelligence memory for recent history |
| `gg memory explain <commit>` | Explain one commit from stored Git memory |
| `gg memory search <query>` | Search commit memory by feature, intent, file, or commit text |
| `gg memory impact <file>` | Show the features and commits most related to one file |
| `gg memory stale` | Report likely stale files from commit-memory history |
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
- provider model loading from `/models` after `BASE_API_URL` and `API_TOKEN` are set, sorted newest-first when the provider returns creation timestamps, with a scrollable picker for long lists

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

## Git Memory

`gitgud` can turn commit history into a local knowledge base:

- a managed `post-commit` hook runs `gg memory ingest --commit HEAD` after each commit
- each analyzed commit stores structured metadata for `what_changed`, likely `why`, `feature`, and `related_files`
- commit-memory data is stored outside the repo under `~/.config/gitgud/repos/<repo-slug>/git-memory/`
- the hook installer refuses to overwrite a `post-commit` hook it does not manage
- `gg config` installs the hook automatically when you save settings inside a Git repo, and `gg memory install` can install it explicitly
- `gg memory explain`, `search`, `impact`, and `stale` query the stored commit intelligence
- `gg ship` includes recent Git-memory context in the review-planning flow, and `gg commit` reuses cached context when available
- if no Git memory exists yet, `gg memory` commands and `gg learn` backfill recent history on demand

## Commit Modes

- `auto`: use the configured AI provider, fall back to heuristic suggestions on timeout, and ignore malformed AI split plans by showing heuristic split suggestions instead
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

- `gg rescue` auto-detects likely recovery incidents, lets you override the incident, previews exact commands, creates a hidden snapshot ref, and saves rollback notes under `.git/gitgud/rescue/`
- supported rescue incidents are `wrong-branch`, `detached-head`, `bad-rebase`, `lost-stash`, `accidental-reset`, and `force-push`
- force-push rescue can restore from a detected commit or a manual SHA/ref fallback and only fetches remote refs after confirmation
- `gg commit` and `gg explain` only use staged changes
- `gg commit` warns about risky staged diffs, supports inline editing, and can propose split commits for mixed concerns
- `gg ship` can roll staged work into the existing commit flow first, surfaces split/squash cleanup suggestions for the outgoing branch, drafts a review title/body, and pushes
- `gg ask` returns recommended and alternative commands with risk badges and explanations
- dangerous actions suggested by `gg ask` require extra confirmation before execution
- `gg push` warns about risky outgoing diffs and does not guess across ambiguous remotes
- repo memory is built automatically on first use in any repo and refreshed every 7 days; it records commit style, conventional types/scopes, branch naming patterns, and frequently changed directories, then injects that context into every AI prompt — run `gg learn` to force an immediate rebuild and backfill commit intelligence
- repo memory is stored in `~/.config/gitgud/repos/` (one TOML file per repo, never committed)
- Git memory stores one JSON file per analyzed commit plus a JSON index under the same repo-specific config area
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
