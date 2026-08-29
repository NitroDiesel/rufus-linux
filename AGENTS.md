# Repository agent instructions

## Start here

Read [`docs/PROJECT_CONTEXT.md`](docs/PROJECT_CONTEXT.md) before planning or
changing the project. It records the shipped baseline, architecture map,
safety invariants, deliberate product boundaries, and the continuation
workflow for cloud agents.

For work that changes capability claims, privilege boundaries, packaging,
releases, or UI behavior, also read the branch-specific document named in the
context file before editing. Completion means the implementation, capability
matrix, project context, and verification evidence agree.

## UI work

Use the `frontend-design` skill and a bounded UI review subagent when those
capabilities are available. Preserve the compact Device Workbench direction,
original Rufus icon assets, keyboard access, true modal blocking, and verified
light/dark layouts. Visually test the smallest supported window and a maximized
window before declaring UI work complete.

## Commit attribution

When Codex materially changes the repository, preserve the official GitHub
identity as the commit author when the execution path supports it:

`Codex <267193182+codex@users.noreply.github.com>`

If another account must author the commit, append this trailer instead:

`Co-authored-by: Codex <267193182+codex@users.noreply.github.com>`

Omit attribution for read-only reviews and commits that contain only
user-authored changes.
