# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**gitgud** (binary: `gg`) is a Rust CLI that wraps the system Git binary with an interactive TUI and AI-assisted commit/push workflows. It generates commit message suggestions, detects unsafe diffs, supports Conventional Commits, and can split multi-concern commits.

## Commands

```bash
# Build
cargo build
cargo build --release

# Format (required before committing Rust code)
cargo fmt

# Test
cargo test            # all tests (unit + integration)
cargo test --lib      # unit tests only
cargo test <name>     # single test by name

# Run subcommands
cargo run --bin gg -- commit
cargo run --bin gg -- explain
cargo run --bin gg -- push
cargo run --bin gg -- config
cargo run --bin gg -- doctor
```

## Architecture

The codebase has six core modules with a clean pipeline structure:

| Module | Responsibility |
|--------|---------------|
| `cli.rs` | Clap-based argument parsing; defines the `Command` enum |
| `app.rs` / `lib.rs` | Orchestrator; routes commands, coordinates modules |
| `git.rs` | All Git operations via `Command::new("git")`; unsafe diff detection |
| `config.rs` | Three-tier config (env vars > `~/.config/gitgud/config.toml` > defaults); keychain for secrets |
| `ai.rs` | HTTP calls to AI providers; prompt construction; fallback strategies |
| `tui.rs` | Ratatui TUI; all interactive screens and keybindings |

### Key Data Flow: `gg commit`

1. `app.rs`: validate repo, staged changes exist
2. `git.rs`: run `git diff --cached` and stat
3. `git.rs` + TUI: detect unsafe patterns (secrets, generated files, lock-only, etc.); confirm with user
4. `config.rs`: resolve provider, model, generation mode, commit style
5. `ai.rs` + TUI: generate 1–3 suggestions (or fall back to heuristic); detect multi-concern splits
6. `tui.rs`: user selects/edits suggestion; confirms split if applicable
7. `git.rs`: execute single commit or split commits

### Important Abstractions

- **`ResolvedValue<T>`**: wraps any config setting with its source (env, file, keychain, built-in) — used throughout `config.rs`
- **`GenerationMode`**: `Auto` (AI with heuristic fallback on timeout), `AiOnly`, `HeuristicOnly`
- **`SplitCommitPlan`**: AI-returned struct grouping files by concern for multi-commit flows
- **`UnsafeDiffWarning`**: enum of diff safety issues surfaced before commit
- **`PushPlan`**: enum distinguishing upstream-exists vs. set-upstream-on-push cases

## Agent Workflow Rules (from AGENTS.md)

- Read relevant code paths before editing — do not assume behavior from filenames alone.
- Keep changes scoped to the task; do not bundle unrelated cleanup.
- **If a change affects user-visible behavior, update `README.md` in the same change.** Treat README maintenance as required work, not follow-up.
- Preserve existing TUI patterns, copy style, and interaction model unless the task is explicitly a UX change.
- Do not add new dependencies unless justified by the task.
- Prefer `cargo fmt` and `cargo test` for Rust changes; run the narrowest check first.
