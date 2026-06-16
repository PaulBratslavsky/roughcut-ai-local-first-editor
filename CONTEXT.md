# CONTEXT — domain glossary

Canonical names for the core domain. Terse and definitional: one line per noun —
definition · invariants · home. This is the **source of truth for names** during
the architecture-deepening work (`docs/refactor-checklist.md`); the narrative
"how it fits together" lives in `docs/07-architecture.md`, not here.

Keep entries short. When a refactor names a new module, add the term here.

## Core nouns

- **Project** (`model.rs`) — the editing unit: `Media` + `Timeline` + `Transcript`
  + metadata (`checkpoints`, `suggestions`, preferences). Persisted whole via the
  `Store`. Has a lifecycle: Draft → Exported → Trashed (today inferred, not an enum
  — see state machines).
- **Media** (`model.rs`) — the source video: path, duration, frame rate. Immutable;
  edits never touch it.
- **Transcript** (`model.rs`) — the spoken content: an ordered list of
  `TranscriptSegment`. Immutable input data (edits flip clip inclusion, not text).
- **Word** (`model.rs`) — one token: `{ text, start, end, confidence }`. The word
  key is `text`, not `word`. Sub-segment timing; the unit `snap` aligns cuts to.
- **TranscriptSegment** (`model.rs`) — a phrase: `{ id, start, end, text, words,
  is_filler, is_silence, take_group_id, is_best_take }`.

## Timeline (the mutable editing state)

- **Timeline** (`model.rs`) — ordered `Clip`s forming a **contiguous partition of
  [0, duration]** over the source, plus `global_padding` and derived `cut_count`.
  Invariant: clips sorted, non-overlapping, gap-free — restored by `normalize()`
  after every edit. `cut_count` = merged excluded regions, kept in sync by normalize.
- **Clip** (`model.rs`) — a source span `[source_in, source_out]` (seconds,
  **frame-snapped**) with `included: bool`, `order`, `origin`. Non-destructive: a
  "cut" flips `included`, never deletes source. Invariant: `source_in < source_out`
  (else duration clamps to 0).
- **Padding** (`model.rs`) — `{ start_s, end_s, linked }`. The pad *currently
  applied* to clip boundaries; re-applying computes a delta so it stays idempotent.

## Edits (the mutation language)

- **EditOp** (`model.rs`) — the sum type of all timeline mutations (CutRange,
  RestoreRange, CutSegments, RestoreSegments, TrimClip, SplitClip, ReorderClip,
  SetGlobalPadding, RoughCut, SetClips). Both the **public edit vocabulary** and
  the **internal journal format**. `SetClips` is the universal exact inverse/redo.
  ⚠ Adding a variant currently touches ~7 disjoint places (refactor item 1).
- **EditAction** (`model.rs`) — the *audit record* of one applied `EditOp`:
  `{ id, kind, source, timestamp, description, op }`. The op is normalized
  (frame-snapped). Exact inverse/redo snapshots live in the **journal**, NOT here
  (they'd bloat every tool response — ADR-0002).
- **Journal** (`engine/edits.rs`) — the undo/redo history: per entry an applied
  `EditAction` + a snapshot `inverse` + `redo` (both `SetClips`). Capped at 100.
  Undo/redo replays a snapshot; the replay is itself journaled.
- **Checkpoint** (`model.rs`) — a named, self-contained timeline snapshot (full
  clips + padding + cut_count) the user returns to **regardless of undo/redo
  state**. `CheckpointSummary` is the lean listing view. Persisted on the Project.

## AI-derived views (read models over the transcript)

- **CutSuggestion** (`model.rs`) — a Tier-2 flag (repeated take / repetition) for
  review, not yet applied: `{ id, segment_ids, start, end, reason, confidence,
  duplicate_of, preview }`. Output of `suggest_cuts`.
- **Outline** (`story.rs`) — the narrative map: `{ beats, method: "llm"|"heuristic" }`.
- **Beat** (`story.rs`) — one story unit: `{ title, summary, role, cut_priority
  (1=spine…5=cut first), segment_ids, start, end }`.
- **FlowReview** (`story.rs`) — coherence verdict over the edited cut:
  `{ coherent, boundaries_checked, issues, method }`. Fast deterministic by
  default; `llm=true` adds an on-device semantic pass.
- **FlowIssue** (`story.rs`) — one seam problem: `{ kind (broken_sentence |
  abrupt_start | flow), severity, at_segment, description, restore_segment_ids }`.
- **Boundary** (`story.rs`, → move to time/boundary module per item 5) — a
  word/sentence edge derived from `Word` timings; what `snap` aligns cuts to.

## Seams (adapters — swappable behind traits)

- **Store** (`store.rs`) — persistence facade over SQLite (projects, transcripts,
  embeddings, preferences, kv). A deep seam; intentionally one trait.
- **VideoEngine** (`adapters/video.rs`), **Transcriber** (`adapters/transcribe.rs`),
  **Embedder** (`adapters/embed.rs`), **InferenceClient** (`adapters/inference.rs`)
  — the portability seam; each has an `Auto*` variant that picks real-vs-mock at
  runtime via `capabilities::probe`.
- **EventSink** (`events.rs`) — core → UI push; `CoreEvent` is the one typed event
  vocabulary, serialized at a single choke point.

## Implicit state machines (today inferred — make explicit in item 6)

- **Transcript state** — None → Partial → Complete, inferred from `Option`/flags.
  Mutated in `media.rs` and `edits.rs`.
- **Undo/redo position** — current index into the journal; mutated in `edits.rs`,
  read in `projects.rs` via `history()`. No single "here's how undo works" home.
- **Suggestion lifecycle** — pending → accepted/dismissed, drained as the user
  acts. A fresh transcribe clears suggestions, which can strand a held id.
