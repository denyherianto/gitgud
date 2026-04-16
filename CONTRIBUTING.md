# Contributing

## Development Setup

1. Install Rust and `git`.
2. Clone the repository.
3. Run `cargo test` to verify the project builds locally.
4. Set `API_TOKEN` if you want to exercise the AI-backed commit flow manually.

## Workflow

1. Create a branch for your change.
2. Keep changes scoped and focused.
3. Add or update tests when behavior changes.
4. Run `cargo fmt` and `cargo test` before opening a pull request.
5. Update `README.md` when user-facing behavior changes.

## Pull Requests

- Describe the problem and the approach clearly.
- Include screenshots or terminal captures if the TUI behavior changes materially.
- Call out any provider-specific behavior or environment changes.
- Prefer small, reviewable pull requests over broad refactors.

## Issues

- Include reproduction steps.
- Include the command you ran.
- Include the relevant environment variables, but never paste secrets.
- Include the current branch and remote setup for push-related bugs.
