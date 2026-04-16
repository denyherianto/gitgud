# AGENTS.md

This guide applies to automated coding contributors working in this repository, including Codex, Claude Code, and similar agentic tools.

## Scope

- Treat this file as the primary contributor guide for agent-driven development work.
- Follow project code and documentation conventions already present in the repository.
- Prefer minimal, targeted changes over broad refactors unless the task explicitly requires wider changes.

## Workflow

- Read the relevant code paths before editing. Do not assume behavior from filenames alone.
- Preserve existing terminal UI patterns, copy style, and interaction model unless the task is explicitly a UX redesign.
- Keep changes scoped to the user request. Do not bundle unrelated cleanup into the same change.
- If a change affects user-visible behavior, update documentation in the same change.

## README Maintenance

- Keep `README.md` in sync with any user-visible behavior changes.
- Update `README.md` whenever commands, flags, keybindings, setup, defaults, configuration, or workflows change.
- Do not leave documented behavior stale after code changes.
- Treat README maintenance as required work, not follow-up work.

## Editing Rules

- Prefer small patches that are easy to review.
- Preserve existing naming and structure unless there is a clear correctness or maintainability reason to change them.
- Avoid destructive git operations and do not revert user changes that are unrelated to the task.
- Do not add new dependencies unless they are justified by the task and consistent with the project direction.

## Validation

- Run the narrowest useful verification first, then broader checks when appropriate.
- Add or update tests when behavior changes or when fixing a bug that should stay fixed.
- For Rust code changes, prefer `cargo fmt` and `cargo test` unless the task only touches documentation.
- If you cannot run a relevant check, say so explicitly in your final handoff.

## Handoff

- Summarize the behavior change, not just the files touched.
- Call out any remaining risks, assumptions, or follow-up work when relevant.
- Cite updated files and tests run when reporting completion.
