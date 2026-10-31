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
- **Media** (`model.rs`) — the source video: path, duration, frame rate. Edits
  never touch the file. ⚠ The `Media` record is replaced by `append_media`
  (stitched source) outside the journal — see audit B4.
- **Transcript** (`model.rs`) — the spoken content: an ordered list of
  `TranscriptSegment`. Edits flip clip inclusion, not text — but it is NOT
  immutable: LLM cleanup rewrites `segment.text` (`media.rs::update_segment_texts`)
  and detectors set `is_filler` / take flags in place (audit B13).
- **Word** (`model.rs`) — one token: `{ text, start, end, confidence }`. The word
  key is `text`, not `word`. Sub-segment timing; the unit `snap` aligns cuts to.
- **TranscriptSegment** (`model.rs`) — a phrase: `{ id, start, end, text, words,
  is_filler, is_silence, take_group_id, is_best_take }`.

## Timeline (the mutable editing state)

- **Timeline** (`model.rs`) — ordered `Clip`s forming a **contiguous partition of
  [0, duration]** over the source, plus `global_padding` and derived `cut_count`.
  Invariant: clips sorted, non-overlapping, gap-free — restored by `normalize()`
  after every edit. `cut_count` = merged excluded regions, kept in sync by normalize.
- **Clip** (`model.rs`) — a source span `[source_in, source_out]` (seconds;
  frame-snapped by range ops only — ⚠ transcript/rough-cut/padding ops write
  raw times, audit B10) with `included: bool`, `order`, `origin`. Non-destructive: a
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
- **Journal** (`JournalEntry` in `engine/mod.rs`; logic in `engine/edits.rs`) — the undo/redo history: per entry an applied
  `EditAction` + a snapshot `inverse` + `redo` (both `SetClips`). Capped at 100.
  Undo/redo replays a snapshot; the replay is itself journaled.
- **Checkpoint** (`model.rs`) — a named, self-contained timeline snapshot (full
  clips + padding + cut_count) the user returns to **regardless of undo/redo
  state**. `CheckpointSummary` is the lean listing view. Persisted on the Project.
- **SavedClip** (`model.rs`; "clip" in the UI and tools) — a standalone edit cut
  from the long-form video: one or more **pieces** (`SourceRange`s, played back to
  back in source order) in a **format** — `landscape` (16:9, the source frame) or
  `vertical` (9:16 short, a full-height crop at `crop_x`, framed on the speaker
  by `center_clip_on_face` — Apple Vision over sampled frames — when it becomes
  vertical; a hand-set crop wins). Its **own Timeline**:
  the main cut restricted to the pieces (`Timeline::restricted_to`), so removed
  silences/fillers stay removed. Snapshotted at save and when the pieces change;
  main-cut edits and undo never move it. Project metadata, not journaled.
  `SavedClipSummary` is the lean listing view. Exports run the normal writers over
  it. **Not** the timeline's `Clip` (a partition piece of the main edit, which
  `trim_clip` / `split_clip` operate on). `create_clips` makes several from
  candidates at once and frames each vertical one before it returns;
  `export_clips` writes several into one folder as `NN-<title>.<ext>`.
- **PlaySpan** (`model.rs`) — one stretch of source that plays, in output order
  `{ source_in, source_out }`. `Timeline::play_spans()` lists the main cut's;
  `SavedClip::piece_spans()` lists a clip's grouped by piece. The MP4 render
  reads one seeked input per piece (nearby in-order pieces share one), so it
  decodes about what plays rather than the source from 0:00.

## AI-derived views (read models over the transcript)

- **CutSuggestion** (`model.rs`) — a Tier-2 flag (repeated take / repetition) for
  review, not yet applied: `{ id, segment_ids, start, end, reason, confidence,
  duplicate_of, preview }`. Output of `suggest_cuts`.
- **ClipCandidate** (`model.rs`, from `clip_finder.rs`) — a suggested clip (a short): a
  sentence-aligned stretch of the current cut inside one Beat, scored 0–100 for
  short-form potential (hook, stands alone, payoff) `{ id, start, end, pieces,
  duration, title, hook, reason, score, segment_ids, method: "llm"|"heuristic",
  theme }`. `pieces` are the stretches it plays, in play order (one today);
  `start`/`end` cover them. A `theme` restricts candidates to moments the hybrid
  search finds about it. A **scan** (`find_clips { scan: true }`) judges moments
  from every Beat and skips moments already saved as clips; one run per project
  at a time. The latest run is persisted on the Project until the next run.
- **Shorts screen** (`frontend/src/panels/ShortsScreen.tsx`) — the app screen
  for reviewing a scan: the player beside a grid of candidate cards and the
  saved clips, with "Scan for shorts", "Make N shorts" and "Export N".
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

## Tool registry (the one surface every caller uses)

- **ToolSpec** (`tools.rs`) — one registry row: name, description, JSON schema,
  handler, and flags `agent` (shown to agent loops), `meta` (starts an agent
  loop / opts in to a frontier provider — UI only), `mutating` (lands
  `EditAction`s).
- **Caller** (`tools.rs`) — who invokes the registry: `Ui | Mcp | Agent {
  attribute_to }`. The single home of per-caller policy: which rows are visible
  AND callable (`defs(caller)` ≡ what `dispatch(…, caller)` accepts), whether
  args are schema-checked (not for `Ui`), and the `ActionSource` stamped on
  edits. Visibility ≠ attribution: frontier escalation runs as `Agent` but
  credits edits to `McpClient`.
- **ActionSource** (`model.rs`) — the attribution tag on an `EditAction`
  (`ui` / `local_ai` / `mcp_client`); derived from the `Caller`. Visibility is
  decided by `Caller` alone; handlers still key their confirmation prompts
  (`export`, `delete_project`) and the preferences egress guard on the source
  (moving those guards onto `ToolSpec` is refactor item 8's follow-up).

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
