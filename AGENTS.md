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

When Codex materially changes the repository, append this trailer to the commit message:

`Co-authored-by: chatgpt-codex-connector[bot] <199175422+chatgpt-codex-connector[bot]@users.noreply.github.com>`

Omit the trailer for read-only reviews and commits that contain only user-authored changes.
