# gg rescue: Guided Git Recovery Wizard

## Summary

Make `gg rescue` the headline feature: a guided recovery wizard for wrong-branch commits, detached HEAD, bad rebases, lost stashes, accidental resets, and force-push mistakes.

Rescue is deterministic first and AI-assisted second. It auto-detects likely incidents from Git state, supports deep links with `gg rescue <incident>`, presents one recommended fix plus alternatives, previews exact commands, creates a safety snapshot before mutating actions, executes interactively, and saves rollback notes under `.git`.

## Implementation

- Add `gg rescue [incident]` with incident slugs:
  - `wrong-branch`
  - `detached-head`
  - `bad-rebase`
  - `lost-stash`
  - `accidental-reset`
  - `force-push`
- Add `r` to the home TUI so Rescue is available alongside ship/commit/push.
- Add a dedicated `src/rescue.rs` module with:
  - `RescueIncident`
  - `RescueContext`
  - `RescuePlan`
  - `RescueOption`
  - `RescueStep`
  - `RescueSnapshot`
  - `RollbackNote`
- Extend `GitRepo` with rescue helpers for reflogs, stash recovery candidates, hidden snapshot refs, ref updates, rebase aborts, and remote restore pushes.
- Save rollback notes under `.git/gitgud/rescue/<timestamp>-<incident>.md`.

## Covered Flows

- Wrong-branch commits:
  preserve the current tip on a rescue branch and optionally move the original branch back to its base.
- Detached HEAD:
  create a branch at the detached commit.
- Bad rebases:
  abort in-progress rebases or recover the pre-rebase tip from reflog history.
- Lost stashes:
  recover from live stash entries or dropped-stash candidates, preferably on a new branch.
- Accidental resets:
  restore the branch to the pre-reset commit from reflog history.
- Force-push mistakes:
  restore the remote branch with `--force-with-lease` using a detected target or a manual SHA/ref fallback.

## Validation

- `cargo test --lib --test git_flow --test rescue_flow`
- The full `cargo test` suite currently still hits the existing `mockito` server restriction in this environment for `tests/ai_provider.rs`.
