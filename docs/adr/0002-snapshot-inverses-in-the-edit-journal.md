# ADR-0002: Snapshot inverses in the edit journal (not analytic inverses)

**Status:** accepted (2026-06)

## Context

Every mutation is an `EditOp` applied through `Editor::apply_edit`, recorded
as an `EditAction { op, inverse, redo }` in a per-project journal persisted to
the store (docs/05 requires an "inverse-able payload"). The inverse of an op
is not always expressible as another small op: `cut_range`'s true inverse
depends on which parts of the range were already excluded, and `rough_cut`'s
redo would re-run detectors against possibly-changed preferences.

## Decision

`inverse` and `redo` are `EditOp::SetClips` values — full snapshots of the
clip arrangement before/after the op. `op` itself is kept verbatim as the
audit record. Undo/redo replay snapshots; audit/replay tooling reads `op`.

## Consequences

- Undo/redo are exact by construction and survive restarts; no per-op inverse
  logic to maintain or get wrong.
- Journal entries carry two clip vectors each; the journal is capped at 100
  entries per project. If projects ever hold thousands of clips, revisit with
  structural sharing or analytic inverses for the cheap ops.
- Replaying `op` history onto different footage is possible (ops are data),
  but redo deliberately does NOT re-run ops — determinism wins.
