# Bulk shorts: scan, review, make, export

*   **Author:** Paul Bratslavsky
*   **Status:** Draft (revised after a fresh-eyes review)
*   **Date:** 2026-10-30

--------------------------------------------------------------------------------

## Overview

Turn one long video into many shorts in one sitting. RoughCut scans the whole
video, scores every moment that could stand alone as a short, and shows the
moments as cards with a thumbnail, a score and a preview. The user ticks the
ones they want; RoughCut makes them as 9:16 saved clips framed on the speaker
and exports them as MP4s into one folder.

Two more abilities build on that:

*   **Play order.** A saved clip's pieces can play in any order, for example
    the payoff first as a hook, then the setup.
*   **Multi-moment shorts.** The scan can propose a short made of 2–3 pieces
    that are not next to each other, for example 0:30–0:50 and 1:45–2:05,
    with the material between them dropped.

This extends the clip finder (`core/src/clip_finder.rs`), saved clips
(`core/src/engine/clips.rs`), the MP4 render (`core/src/adapters/video.rs`)
and the Clips tab (`frontend/src/panels/ClipsPanel.tsx`). Everything stays
on-device.

## The Problem

The clip finder today is built for picking a few shorts, not for working
through a video:

*   It returns at most 20 candidates (`count` is clamped to 1–20) and the
    Clips tab asks for 8. Only the best 24 by a rule-based pre-score
    (`POOL_MAX`, raised to `count` when that is larger) reach the model, so on
    a 45-minute video most moments are never judged.
*   Saving is one candidate at a time and exporting is one clip at a time.
*   Every MP4 render decodes the source from 0:00 up to the clip's last piece
    (`render_mp4` trims one un-seeked input). A short from minute 40 of a 4K
    video decodes 40 minutes of footage. Twenty shorts multiply that.
*   A saved clip's pieces always play in video order: `set_ranges` sorts
    them, the preview sorts them (`previewRanges`), and the clip's timeline
    is a sorted partition of the source that `Timeline::normalize()` keeps
    sorted.
*   Each candidate is one continuous stretch inside one story beat. A short
    that skips the middle of a stretch can only be built by hand ("Add to
    clip" in the transcript).

## Goals

*   One action ("Scan for shorts") scans the whole current cut and returns up
    to 60 candidates, spread across the whole video, ranked best first.
*   Each card shows the existing score (0–100), title, opening line, reason,
    playable length and a thumbnail; clicking it plays the candidate in the
    player.
*   The user ticks cards and clicks "Make N shorts": each becomes a vertical
    saved clip with its crop framed on the speaker before the action
    finishes.
*   The user ticks saved clips and clicks "Export N": each renders to
    `NN-<title>.mp4` in one chosen folder, without overwriting files; one
    failure does not stop the rest.
*   An MP4 render decodes only the footage it uses.
*   A saved clip's pieces can be reordered; preview, MP4, NLE exports, SRT and
    TXT all follow the play order.
*   When the on-device model is reachable, the scan also proposes
    multi-moment candidates. Each passes one shared seam check (below).
*   Existing callers keep working: `find_clips` without the new arguments
    returns what it returns today (the `count` limit rises from 20 to 60),
    and old projects load.

## Non-Goals

*   **Predicting virality.** The score judges the transcript: a hook, making
    sense alone, a payoff. It does not see the picture, hear energy or
    laughter, or know how posts perform. The UI calls it a "short score".
*   **Sub-scores per quality** (hook / stands alone / payoff). The existing
    score plus its one-line reason answers "why". See Future Improvements.
*   **Dismissing candidates** so a re-scan never shows them again. Unticked
    candidates stay in the list until the next scan replaces it.
*   **A tool for MCP clients to add candidates.** An MCP client (Claude) can
    already build multi-piece shorts as saved clips with
    `create_clip { segment_ids | ranges }`; the user reviews them in the saved
    list.
*   **Importing from a URL**, **captions drawn onto the video**, **posting or
    scheduling**: each is its own design.
*   **A crop that follows the active speaker.** A clip keeps one `crop_x`.
*   **Cancelling a scan.** The rest of the app stays usable while it runs.
*   **Making `reorder_clip` work on the main timeline.** It is a no-op today
    (see Additional Resources); fixing it is separate work.

## Open Questions

*   **D1: Where the review happens.** The owner chooses; the plan marks the
    files that differ as **(D1a)** or **(D1b)**.
    *   **(a) Grow the Clips tab.** Candidate rows become cards with a
        checkbox and thumbnail; the main preview plays them. *Cost:* about
        3 days of UI. *Trade-off:* the side panel is narrow, so 30–60 cards
        is a long scroll beside the transcript. *Fails when* the user wants
        to compare many moments at a glance.
    *   **(b) A Shorts screen** (a third `AppScreen` beside `editor` and
        `recorder`): the existing `PreviewPanel` on the left with the 9:16
        guide, a grid of cards on the right, a bar with "Scan", "Make N",
        "Export N". It reuses `PreviewPanel` and `usePlaybackEngine` as they
        are; both read the same `viewStore`, and only one screen is mounted at
        a time, so there is one player. The Clips tab gains an "Open review"
        button that switches `screen` to `"shorts"`. *Cost:* about 5–6 days
        of UI. *Trade-off:* to fix the main cut mid-review the user switches
        screens. *Fails when* review and cut editing are interleaved a lot.

## Critical User Journeys (CUJs)

### CUJ 1: Scan, review, make

*   **Given**: a transcribed 45-minute talking-head project, the on-device
    model reachable.
*   **When**: the user clicks "Scan for shorts".
*   **Then**: progress shows "outlining the video…", then "scoring candidates
    on-device — 3/15". When it finishes, up to 40 cards appear, best first,
    from all parts of the video. Clicking a card plays it with a centred
    9:16 guide. The user ticks six and clicks "Make 6 shorts". Progress shows
    "framing 2/6". Six vertical saved clips appear with crops on the speaker;
    the six cards leave the list; the six new clips are ticked in the saved
    list.

### CUJ 2: Export the batch

*   **Given**: six ticked saved shorts.
*   **When**: the user clicks "Export 6" and picks a folder.
*   **Then**: progress shows "2/6 · <title> · rendering MP4… 40%". The folder
    gets `01-<title>.mp4` … `06-<title>.mp4` at 1080×1920. A name that exists
    gets `-2`. If one render fails, the other five land and the failed one is
    listed with its error.

### CUJ 3: Payoff first

*   **Given**: a saved clip with pieces 0:30–0:50 and 1:45–2:05.
*   **When**: the user moves piece 2 above piece 1.
*   **Then**: preview plays 1:45–2:05, then 0:30–0:50. The MP4, Premiere XML,
    SRT and TXT follow that order; the first caption is the payoff's first
    line at 00:00.

### CUJ 4: A multi-moment short

*   **Given**: inside one 2.5-minute stretch, the speaker sets up a problem at
    0:30–0:50, wanders, and resolves it at 1:45–2:05.
*   **When**: the scan runs with the model reachable.
*   **Then**: one card is labelled "2 pieces", shows where each sits on a strip
    of the video's length, and plays 0:30–0:50 then 1:45–2:05. The first piece
    does not open on "and", "so" or "as I said"; each piece ends on a full
    sentence.

### CUJ 5: Re-scan after making shorts

*   **Given**: six shorts made in CUJ 1.
*   **When**: the user scans again.
*   **Then**: the six moments already saved as clips are not offered again.

### CUJ 6: No model

*   **Given**: Ollama is not running.
*   **When**: the user scans.
*   **Then**: candidates are ranked by the rule-based score, single stretches
    only; a note says multi-moment shorts need the on-device model.

## Technical Plan (Architecture)

### Overview

```text
 find_clips { scan, count ≤ 60, min_score, multi_moment }
   │  one scan per project at a time
   ▼
 clip_finder.rs ─ single stretches ───────────┐
   outline → beats (cached)                    │
   sentence windows inside beats               │
   rule score; scan: round-robin pool by beat  │
   model scores in batches of 6                │
                                               ▼
 clip_compose.rs ─ multi-moment (Phase 3) ─► merge: diversify on pieces,
   best spans → model keeps 2–3 runs            drop saved moments,
   seam check, same score formula               write list (one closure)
                                               │
                                               ▼
                      Project.clip_candidates ──► cards ──► create_clips
                                                              │ frame each
                                                              ▼
                                   saved clips ──► export_clips ──► folder
                                        │
                     play order: SavedClip.ranges ──► PlaySpan list ──►
                       render_mp4 (one seeking input per piece), NLE writers,
                       SRT, TXT, preview audition
```

### Data model (`core/src/model.rs`)

*   `ClipCandidate` gains `pieces: Vec<SourceRange>`, `#[serde(default)]`.
    `Project::upgrade()` (which already fills `SavedClip.ranges` for old
    clips) fills an empty `pieces` with `[start, end]`, so no consumer ever
    sees an empty list. `start` / `end` stay as the covered span (earliest
    piece start, latest piece end) for old readers. `duration` is the sum of
    the pieces' playable seconds.
*   `SavedClip.ranges` changes meaning from "sorted" to "in play order".
    `start` / `end` become the minimum piece start and maximum piece end.
    `SavedClip.timeline` is unchanged: a sorted partition of the source, the
    main cut restricted to the pieces. The Timeline invariant (sorted,
    normalized) holds everywhere.
*   New `PlaySpan { source_in: f64, source_out: f64 }`: one stretch of source
    that plays, in output order. Two functions produce lists of them:
    *   `Timeline::play_spans()`: the included clips in vector order (for the
        main cut, the same thing every writer walks today).
    *   `SavedClip::play_spans()`: for each piece in `ranges` order, the
        included clips of `clip.timeline` that fall inside the piece,
        clipped to it. Touching pieces played out of order stay two spans,
        even though the timeline merges them into one included clip.
*   New `ProgressTask` variants `MakeClips` and `ExportClips`.
*   New result types (ts-rs exported): `ClipBatchError { candidate_id: Uuid,
    message: String }` and `ClipExportResult { clip_id: Uuid, path:
    Option<String>, error: Option<String> }`.

"The same moment" keeps today's meaning everywhere: two candidates overlap
when they share more than `MAX_OVERLAP` (30%) of the shorter one's **source**
seconds (cut material included, as `overlap()` measures today). For piece
lists, the shared seconds are summed over all piece pairs.

### Phase 1: scanning, review, batch make and export

**Scan mode.** `find_clips` gains `scan: bool` (default false),
`min_score: f64` (optional) and `multi_moment: bool` (Phase 3, default
false). `count` is clamped to 1–60 in all modes.

*   Without `scan`, the pool is `diversify(windows, POOL_MAX.max(count))` as
    today.
*   With `scan`, the pool is filled **round-robin across beats**: rank each
    beat's windows by rule score, `diversify` within the beat, then take the
    best window of every beat, then the second best of every beat, and so on,
    skipping windows that overlap one already taken, until the pool holds
    `SCAN_POOL` windows (starting value 90 = 15 model calls; tune after
    measuring). Every beat with at least one window is represented before any
    beat gets a second.
*   When the beat-respecting windows are empty and the finder falls back to
    `candidate_windows(None)` (choppy rule-based outlines), there are no beats
    to rotate over: the pool is `diversify(windows, SCAN_POOL)`.
*   A `theme` filters windows first, as today; the round-robin runs on what
    remains.
*   `min_score` drops windows the **model** judged below it. Windows the
    model did not judge (no model, or a failed batch) are not filtered by it:
    they rank after judged ones, as today. With no model at all, the result
    is the top `count` by rule score.

**Writing the list.** The scan builds its result outside the project lock,
then writes it in one `update_project_meta` closure that also drops
candidates overlapping any **current** saved clip. A clip made while the
scan ran is therefore not offered again (CUJ 5). The `Editor` keeps a set of
project ids with a running `find_clips`; a second call for the same project
fails with "a clip scan is already running for this project". The UI
disables "Scan" while one runs.

**Batch make.** `create_clips { project_id, candidate_ids, format? }`:

1.  For each id, build the clip from the candidate's `pieces` and validate it
    (`set_ranges` on a copy). Only after it validates is the candidate
    removed from the list and the clip added. A candidate whose material was
    cut from the main edit since the scan stays listed and is reported in
    `errors`. (`create_clip` gets the same fix: today it removes the
    candidate before validating.)
2.  Then, one clip at a time, run the face pass (`face_crop`) for every
    vertical clip and await it, emitting `MakeClips` progress "framing
    2/6". A clip the pass can't frame (no confident face, or a project that
    composites a screen recording) keeps `crop_x` 0.5; that is not an error.
3.  Return `{ clips: [SavedClipSummary], errors: [ClipBatchError] }`, with
    crops already set.

Framing inside the call (instead of the background task single
`create_clip` starts) means a following export never renders an unframed
crop, and nothing is lost if the app quits.

**Batch export.** `export_clips { project_id, clip_ids, out_dir, target? =
"mp4" }`:

*   Expands `~` in `out_dir` and creates it.
*   File name: `NN-<slug>.<ext>`. `NN` is the 1-based position in `clip_ids`,
    zero-padded to two digits (three past 99). The slug is the title in
    lowercase, every run of characters other than ASCII letters and digits
    replaced by one `-`, trimmed of `-`, cut to 40 characters; an empty slug
    becomes `clip`. `<ext>` comes from `export::default_extension(target)`.
    If the file exists, `-2`, `-3`, … is added before the extension.
*   Renders one clip at a time through the existing `export_clip` path.
*   Progress: `render_mp4` emits its own `Export` events (0–0.99, then
    "render complete") for every render. `export_clips` passes a wrapping
    event sink that turns each into an `ExportClips` event with fraction
    `(i + f) / n` and message `"{i+1}/{n} · {title} · {original}"`, drops the
    per-clip "render complete", and sends one final `1.0` "exported k of n".
    The Export menu (which shows `Export` events) does not see batch events.
*   Returns `{ results: [ClipExportResult] }` in `clip_ids` order.
*   An MCP request asks the user once for the whole batch: "Export N clips
    as <target> to <folder>?", as `export_clip` asks for one.

**Render seeking (`render_mp4`).** `RenderJob` gains `spans: Vec<PlaySpan>`
grouped into pieces (`Vec<Vec<PlaySpan>>`: one inner list per piece; the main
export is one piece). Each piece gets its own input of the source file,
opened with `-ss (piece_start − preroll)` where `preroll = 1 s +
max(audio_offset_s, 0)`; its spans' trims are measured from that seek point.
The audio-offset graph (`audio_sources`) runs per input as it does today on
the one input, and the preroll keeps the delayed audio's first second real
audio instead of `adelay` silence. When compositing a screen recording, the
screen file also gets one input per piece at the same seek point. The concat
order is the piece order. Effects:

*   a short from minute 40 decodes about its own length, not 40 minutes;
*   pieces in any order each read their own input, so ffmpeg does not have to
    hold decoded frames of one piece while it waits for another (to be
    confirmed by watching peak memory on a 4K reordered render, see
    Verification).

### Phase 2: play order

*   **`set_ranges`** keeps the caller's order. If the pieces arrive in video
    order, it behaves as today (frame-snap, merge touching and overlapping
    pieces). If they don't, overlapping pieces are an error ("a moment can't
    play twice") and touching pieces stay separate. The clip's timeline is
    built from a sorted copy (`restricted_to` unchanged).
*   **Exports** take `PlaySpan`s, not `timeline.included_clips()`:
    *   `render_mp4`: already done in Phase 1.
    *   NLE writers (`fcpxml.rs`, `xmeml.rs`, `edl.rs`, `otio.rs`) and the TXT
        writer (`txt.rs`, which walks `tl.clips`) receive the span list.
        `export` passes `timeline.play_spans()`, `export_clip` passes
        `clip.play_spans()`. Output for the main cut is unchanged.
    *   SRT (`srt.rs`): each transcript segment goes to the span holding its
        midpoint (segments whose midpoint is in no span are skipped, as
        today). Its cue start and end are clamped to that span and mapped to
        output time as (sum of earlier spans' lengths) + (time − span start).
        Cues are sorted by output start before numbering. `source_to_output`
        is unchanged and still used for the main timeline (chapters in
        `agent.rs`).
*   **Preview** (`state/viewStore.ts`, `playback/engine.ts`): `previewRanges`
    stops sorting. The audition holds the pieces in play order plus the
    current index. At the end of piece *i* the engine moves to the start of
    piece *i + 1* through a new internal `seekWithinAudition` (the public
    `seekTo` clears the audition and can't be used). The audition ends at the
    end of the last piece played. Excluded material inside a piece is skipped
    by the existing range logic. `addToClipDraft` keeps sorting: a draft is
    built in video order.
*   **UI**: each saved clip in the Clips tab lists its pieces with up/down
    buttons; a move calls `update_clip { ranges }` with the new order.
*   **Mock**: `normalizeRanges` in `mockApi.ts` follows the same rule as
    `set_ranges`, so reordering doesn't revert in the browser build.

### Phase 3: multi-moment candidates (`clip_compose.rs`, new)

Runs inside a scan when `multi_moment` is set and the model is healthy. The
idea is to **condense a stretch**: give the model a few minutes of numbered
sentences and let it keep the runs that make a short, dropping the middle.

1.  **Spans to condense.** Each model-outline beat is a span; so is each pair
    of neighbouring beats whose total length is at most `MAX_SPAN` (starting
    value 3.5 minutes). Spans are ranked by the best single-stretch score
    inside them, and the best `SPANS` (starting value 10) are condensed. Beats
    are at least 60 s and aim at about 2.5 minutes (`story.rs`), so CUJ 4's
    two pieces, 55 s apart, fall inside one span.
2.  **Model call, one per span.** The span's speech sentences, numbered from
    0, kept-in-cut only. The model returns `{ keep: [[first, last], …],
    title, reason, score }`: 2 or 3 runs of consecutive sentence numbers in
    video order. Indexes, not ids, as the finder already does.
3.  **Validation, deterministic.** Runs in range, in order, not overlapping,
    with at least one sentence between runs (otherwise they are one run and
    the candidate is a single stretch, already covered); each run at least
    3 s playable; total playable inside `[min_s, max_s]`.
4.  **Seam check, shared.** One function, `seam_problems(pieces in play
    order)`, used here and by `create_clips` warnings later, built from
    today's lists merged into one place (`clip_finder.rs`'s `DANGLING`,
    `BACKREF`, `COLD_REFERENCE` and `story.rs`'s `CONNECTIVES`):
    *   the first piece must not open on a dangling connective, a back
        reference or a cold pronoun;
    *   every piece must end on `.`, `!`, `?` or `…`;
    *   later pieces may open on a connective ("So the fix is…" is a normal
        payoff opener).
    A candidate with any problem is dropped.
5.  **Score on the same scale as singles.** Today a single scores
    `0.8 × model + 0.2 × rule`. The rule score (`heuristic_score`) is
    generalised to take pieces (opener of the first piece, close of the last,
    beat role, length sweet spot, filler share over kept sentences); a single
    is the one-piece case, so its score is unchanged. Composites use the same
    formula.
6.  **Merge.** Composites and singles go into one `diversify`. Where a
    composite and a single are the same moment, the higher score stays; on a
    tie the single stays (the simpler edit).

Without a model, step 2 is impossible, so no composites (CUJ 6).

Before the Phase 3 UI work, a measurement step: run the composer on two real
videos and grade every composite with the rubric in Verification. Ship only
if at least half pass; otherwise tune spans and prompt first.

### Caller policy (`tools.rs`)

| Tool | Local agent | MCP | UI |
| --- | --- | --- | --- |
| `find_clips` (new arguments) | yes | yes | yes |
| `create_clips` | yes | yes | yes |
| `export_clips` | no (export is never shown to the agent) | yes, user approves once | yes |

### Cards and previews (frontend)

*   **Thumbnail**: the nearest frame from the filmstrip `get_media_assets`
    already caches (one frame about every 10 s on a 45-minute video, 160 px
    wide) at the first piece's start. The filmstrip is empty in demo mode and
    the browser mock: the card then shows a plain block with the timecode.
*   **Card**: checkbox, thumbnail, title, opening line, score, reason,
    playable length, Preview. Phase 3 adds "N pieces" and a strip marking each
    piece on the video's length.
*   **Preview**: `previewRanges(pieces)` with a centred 9:16 guide (face
    framing only runs once a clip is made).
*   **Buttons**: "Scan for shorts" calls `find_clips { scan: true, count: 40,
    min_score: 40, multi_moment: true }` (the count and threshold are
    starting values); "Make N shorts" calls `create_clips`; "Export N" asks
    for a folder through the Tauri dialog plugin (`dialog:default` is already
    granted) and calls `export_clips` with the ticked saved clips.

## Alternatives Considered

### Alternative A: Score every sentence window with the model

*   **Pros**: no rule pre-filter can hide a good moment.
*   **Cons**: thousands of overlapping windows on a 45-minute video; hundreds
    of local model calls.
*   **Reason for Rejection**: a round-robin pool by beat reaches every part
    of the video in about 15 calls.

### Alternative B: Pair distant moments by transcript search

*   **Mechanism**: take a strong moment, search the transcript with its text,
    pair it with hits elsewhere.
*   **Cons**: search finds sentences that sound alike, which are usually
    repeats (a teaser, a recap), not a setup and its payoff. Excluding the
    anchor's own beat also excludes the user's own example, where both pieces
    are about a minute apart.
*   **Reason for Rejection**: condensing a stretch handles the pieces that are
    close together. Pairing far-apart beats by their outline titles and
    summaries is a Future Improvement, once condensing is measured.

### Alternative C: Store play order in the clip's timeline

*   **Mechanism**: reorder `SavedClip.timeline.clips`.
*   **Cons**: breaks the Timeline invariant; `normalize()` re-sorts any
    timeline it touches (that is why `reorder_clip` has no effect today);
    touching pieces merge into one clip and can't be reordered.
*   **Reason for Rejection**: play order lives in `ranges`, and consumers
    read `PlaySpan`s.

### Alternative D: Render each piece to a file, then join them

*   **Mechanism**: seek and render each piece separately, then join with
    ffmpeg's concat demuxer.
*   **Cons**: the AAC encoder adds about 21 ms of priming per file, which
    adds up at every join (seen in the recorder's take joins); the main
    export can have hundreds of spans.
*   **Reason for Rejection**: one input per piece in one filter graph gives
    the same seeking without joins.

### Alternative E: A background queue for face framing

*   **Cons**: an export right after "Make N" can render unframed crops; the
    queue is lost when the app quits.
*   **Reason for Rejection**: `create_clips` frames before it returns.

--------------------------------------------------------------------------------

## Non-Functional Requirements

*   **Privacy**: no new network calls; scoring, condensing and face framing
    stay on-device.
*   **Scan time (estimate; Phase 1 measures it)**: the model outline runs on
    chunks of 120 sentences and is cached. A scan adds up to 15 scoring calls,
    and Phase 3 up to 10 condensing calls. At 5–15 s per call on a local 4B
    model, a 45-minute video takes about 2–6 minutes once the outline exists.
    If it is much slower, lower `SCAN_POOL`.
*   **Make time**: framing grabs 12 frames per clip; six clips should take
    well under a minute. Measured in Phase 1.
*   **Export time**: with per-piece seeking, a 30 s short renders in about the
    time a 30 s render takes today from the start of a file.
*   **MCP timeouts**: a scan can outlast an MCP client's tool timeout, like
    `outline_transcript`. The tool description says so; the list still
    persists and `get_clip_candidates` reads it.
*   **Compatibility**: new fields are `#[serde(default)]` and old candidates
    are upgraded on load. `find_clips` without new arguments is unchanged
    apart from the higher `count` limit. Main-cut NLE, SRT and TXT exports
    are byte-identical; the main-cut MP4 has the same content (same spans in
    the same order), rendered from a seeked input.

--------------------------------------------------------------------------------

### Additional Resources

*   [`CONTEXT.md`](../../CONTEXT.md): `Timeline`, `SavedClip`,
    `ClipCandidate`, `Beat`, `FlowIssue`.
*   [`tool-api.md`](../tool-api.md): the Clips section.
*   [`07-architecture.md`](../07-architecture.md): registry, callers, events.
*   Code: `core/src/clip_finder.rs` (pool, scoring, `diversify`),
    `core/src/story.rs` (beats ≥ 60 s aimed at 2.5 min; `review_flow`
    `CONNECTIVES`), `core/src/engine/clips.rs` (`create_clip`, `set_ranges`,
    `face_crop`), `core/src/adapters/video.rs` (`render_mp4`,
    `audio_sources`), `core/src/export/` (writers, SRT).
*   Found during review, out of scope here: `reorder_clip` on the main
    timeline is a no-op because `apply_edit` runs `normalize()` after every
    op (`core/src/engine/edits.rs`), which re-sorts clips by source time.

### Future Improvements

*   **Sub-scores** (hook / stands alone / payoff), each with a reason.
*   **Dismissing candidates**, remembered across scans.
*   **Pairing far-apart beats** for multi-moment shorts, using the cached
    outline's titles, summaries and roles.
*   **Audio signals** in the score: loudness changes and laughter.
*   **Cancelling a scan** between model batches.
*   **A crop that follows the active speaker.**

--------------------------------------------------------------------------------

## Detailed Implementation Plan

Rough effort: Phase 1 about 2 weeks (D1a) or 2.5 weeks (D1b), the render
change included; Phase 2 about 1 week; Phase 3 about 1–1.5 weeks including
the measurement step. Numbers marked "starting value" are settings to tune.

### Phase 1: scan, review, batch make and export

#### 1. `core/src/model.rs` (Modify)

*   **Type / Scope**: data model.
*   **Proposed Changes**: `ClipCandidate.pieces` (`#[serde(default)]`);
    `Project::upgrade()` fills empty `pieces` with `[start, end]`;
    `PlaySpan`, `Timeline::play_spans()`, `SavedClip::play_spans()`;
    `ClipBatchError`, `ClipExportResult` (ts-rs exported). Tests: an old
    candidate JSON loads with `pieces = [start, end]`; `play_spans` on a clip
    with inner cuts returns only kept material.

#### 2. `core/src/events.rs` (Modify)

*   **Proposed Changes**: `ProgressTask::MakeClips`,
    `ProgressTask::ExportClips`.

#### 3. `core/src/clip_finder.rs` (Modify)

*   **Proposed Changes**:
    *   `FindOptions` gains `scan: bool`, `min_score: Option<f64>`,
        `multi_moment: bool`; `count` clamp 1–60.
    *   `round_robin_pool(windows, beat_of, cap)` for scan mode; the
        no-beat fallback uses `diversify(windows, cap)`. Constant
        `SCAN_POOL = 90`.
    *   `min_score` applied to judged windows only.
    *   `to_candidate` fills `pieces`.
    *   `overlap` takes piece lists (source seconds; a single is one piece),
        so Phase 3 reuses it.
    *   The final write goes through a new `Editor::replace_clip_candidates`
        (item 5) that drops candidates overlapping current saved clips.
    *   Tests: round-robin covers every beat before repeating one; the cap
        holds; the no-beat fallback; `min_score` keeps unjudged windows;
        `scan: false` builds the same pool as today (same input → same
        windows); `overlap` on piece lists.

#### 4. `core/src/engine/mod.rs` (Modify)

*   **Proposed Changes**: `scans: Mutex<HashSet<Uuid>>` on the Editor's
    inner state, with a guard type that removes the id on drop; `find_clips`
    takes the guard first.

#### 5. `core/src/engine/clips.rs` (Modify)

*   **Proposed Changes**:
    *   `create_clip` from a candidate uses `pieces`, validates before
        removing the candidate.
    *   `create_clips(project_id, ids, format) -> (Vec<SavedClipSummary>,
        Vec<ClipBatchError>)`: validate-then-add per id, then `face_crop` per
        vertical clip in sequence with `MakeClips` progress; framing
        failures leave `crop_x` alone.
    *   `replace_clip_candidates(project_id, candidates)`: one
        `update_project_meta` closure that filters out candidates
        overlapping current `saved_clips`, then replaces the list.
    *   Tests: a stale candidate stays listed and is reported; a clip saved
        during a scan is filtered from the scan's result.

#### 6. `core/src/adapters/video.rs` (Modify)

*   **Proposed Changes**: `RenderJob.pieces: Vec<Vec<PlaySpan>>` replaces
    reading `timeline.included_clips()`; one `-ss` input per piece (plus one
    screen input per piece when compositing), preroll `1 s +
    max(audio_offset_s, 0)`, trims relative to the seek point,
    `audio_sources` per input. Tests: filter-graph string tests for one
    piece, two pieces out of order, positive and negative audio offset,
    compositing (extend the existing `audio_sources` tests).

#### 7. `core/src/export/mod.rs` (Modify)

*   **Proposed Changes**: `export` builds `pieces = [timeline.play_spans()]`;
    `export_clip` builds one list per piece from `clip.play_spans()`.
    `Editor::export_clips(project_id, clip_ids, out_dir, target)`: folder
    creation, naming, collision suffix, wrapping event sink, results. Tests:
    the naming rule (padding, slug, empty title, collision); the wrapping
    sink's fraction and message.

#### 8. `core/src/tools.rs` (Modify)

*   **Proposed Changes**: `find_clips` schema and description gain `scan`,
    `min_score`, `multi_moment` and "a scan can take minutes"; new rows
    `create_clips` (agent: true) and `export_clips` (agent: false; one MCP
    confirmation). `create_clips` takes `candidate_ids` (array of ids).

#### 9. `core/tests/e2e.rs` (Modify)

*   **Proposed Changes**: through the registry: `find_clips` with no new
    arguments returns at most 8 and the same shape plus `pieces`; a scan
    with `count: 30`; `create_clips` with three ids including one made stale
    by a cut; a second scan doesn't offer the saved moments; a concurrent
    second scan errors; `export_clips` hidden from the agent caller (extend
    the visibility assertion next to `names.contains(&"find_clips")`).

#### 10. `docs/tool-api.md` (Modify)

*   **Proposed Changes**: Clips section: `find_clips` arguments and the
    `pieces` field; `create_clips` and `export_clips` with their exact
    result shapes; the one-scan rule.

#### 11. `frontend/src/ipc/mockApi.ts` (Modify)

*   **Proposed Changes**: `find_clips` honours `scan`, `count`, `min_score`
    and returns `pieces`; new `create_clips` and `export_clips` (fake paths,
    `make_clips` / `export_clips` progress). A mock switch for "no model" so
    CUJ 6 can be walked in the browser build.

#### 12. `frontend/src/ipc/generated/*`, `frontend/src/ipc/types.ts` (Regenerate / Modify)

*   **Proposed Changes**: regenerate `ClipCandidate.ts`, `ProgressTask.ts`,
    new `PlaySpan.ts`, `ClipBatchError.ts`, `ClipExportResult.ts`; re-export
    the new types from `types.ts`.

#### 13. `frontend/src/ipc/queries.ts` (Modify)

*   **Proposed Changes**: `FindClipsArgs` gains `scan`, `min_score`,
    `multi_moment`; `useCreateClips` (invalidates `clips`,
    `clip-candidates`); `useExportClips`.

#### 14. `frontend/src/panels/CandidateCard.tsx` (Create)

*   **Proposed Changes**: the card, with the thumbnail fallback.

#### 15. `frontend/src/panels/ClipsPanel.tsx` (Modify, D1a) or `frontend/src/panels/ShortsScreen.tsx` (Create, D1b)

*   **Proposed Changes**: cards, selection, "Scan for shorts", "Make N",
    "Export N", progress from `find_clips`, `make_clips`, `export_clips`
    events. Saved clips gain a checkbox; clips made by "Make N" start
    ticked. D1b: the same in a new screen beside `PreviewPanel`; the Clips
    tab gets "Open review".

#### 16. `frontend/src/App.tsx`, `frontend/src/state/viewStore.ts`, `frontend/src/panels/TopBar.tsx` (Modify, D1b only)

*   **Proposed Changes**: `AppScreen` gains `"shorts"`; `App.tsx` renders
    `ShortsScreen`; a "Shorts" button in the top bar.

#### 17. `frontend/src/styles.css` (Modify)

*   **Proposed Changes**: card, selection bar (and the D1b grid), on the
    existing `:root` tokens.

#### 18. `CONTEXT.md`, `core/src/README.md`, `core/src/engine/README.md`, `core/src/export/README.md`, `core/src/adapters/README.md` (Modify)

*   **Proposed Changes**: `ClipCandidate.pieces`; **PlaySpan**; batch make
    frames before returning; one scan per project; render seeks per piece.

### Phase 2: play order

#### 19. `core/src/engine/clips.rs` (Modify)

*   **Proposed Changes**: `set_ranges` keeps order (rules under "Phase 2:
    play order"); `start`/`end` are min/max. Tests: out-of-order pieces keep
    their order; an overlap out of order is rejected; video-order input
    behaves as today.

#### 20. `core/src/model.rs` (Modify)

*   **Proposed Changes**: `SavedClip::play_spans()` follows `ranges` order
    (Phase 1 already uses it; with sorted ranges the result is unchanged).
    Test: two touching pieces swapped give two spans in swapped order.

#### 21. `core/src/export/fcpxml.rs`, `xmeml.rs`, `edl.rs`, `otio.rs`, `txt.rs`, `srt.rs`, `mod.rs` (Modify)

*   **Proposed Changes**: writers take `&[PlaySpan]`; SRT maps by span as
    described. Tests: main-cut output byte-identical to today on the
    existing fixtures; a payoff-first clip's SRT starts at 00:00 with the
    payoff; a cue straddling a span edge is clamped.

#### 22. `frontend/src/state/viewStore.ts`, `frontend/src/playback/engine.ts` (Modify)

*   **Proposed Changes**: ordered audition with index,
    `seekWithinAudition`, end after the last piece played; `previewRanges`
    stops sorting.

#### 23. `frontend/src/panels/ClipsPanel.tsx`, `frontend/src/ipc/queries.ts`, `frontend/src/ipc/mockApi.ts` (Modify)

*   **Proposed Changes**: piece list with up/down; `useUpdateClip`'s
    argument type gains `ranges`; mock `normalizeRanges` keeps order like
    `set_ranges`.

#### 24. `docs/tool-api.md`, `CONTEXT.md`, `core/src/export/README.md` (Modify)

*   **Proposed Changes**: "played back to back in source order" becomes "in
    the order of `ranges`"; the SRT cue rule.

### Phase 3: multi-moment candidates

#### 25. `core/src/seams.rs` (Create), `core/src/story.rs`, `core/src/clip_finder.rs` (Modify)

*   **Proposed Changes**: one module holds the word lists and
    `seam_problems(pieces)`; `story.rs` and `clip_finder.rs` use its lists
    instead of their own. Tests: the three rules; `review_flow` results on
    its existing tests are unchanged.

#### 26. `core/src/clip_compose.rs` (Create), `core/src/lib.rs` (Modify)

*   **Proposed Changes**: `Composite { pieces: Vec<SourceRange>, segs:
    Vec<usize> (kept speech indexes), score, title, reason, judged }`; spans, one model call
    per span, validation, seam check, scoring. Constants `MAX_SPAN`
    (210 s), `SPANS` (10), starting values. Tests with a mock model: valid
    runs become a candidate; touching runs are rejected as a single;
    a first piece opening on "And" is dropped; a 70 s total with
    `max_s` 60 is dropped.

#### 27. `core/src/clip_finder.rs` (Modify)

*   **Proposed Changes**: `heuristic_score` takes pieces (single = one
    piece; existing tests unchanged); when `multi_moment` and the model is
    healthy, call `clip_compose::compose` and merge before the final
    `diversify`. Tests: composing is skipped when `multi_moment` is false or
    the model is down; a composite and an overlapping single keep the
    higher score; a tie keeps the single.

#### 28. `frontend/src/panels/CandidateCard.tsx`, `frontend/src/ipc/mockApi.ts` (Modify)

*   **Proposed Changes**: "N pieces" label and piece strip; the mock returns
    one multi-piece candidate.

### Verification Plan

*   **Rust**: `cargo test -p roughcut-core` (unit, e2e, `doc_parity`,
    `mock_parity`).
*   **Generated types**: `TS_RS_EXPORT_DIR="$(pwd)/frontend/src/ipc/generated"
    cargo test -p roughcut-core --features ts-bindings export_bindings`; a
    second run leaves no diff.
*   **Frontend**: `cd frontend && npx tsc -b && npm run build`. The frontend
    has no test runner; the browser mock (`npm --prefix frontend run dev`)
    is used to walk CUJs 1, 2, 5, 6 and, after Phase 2, CUJ 3.
*   **Real app** (`cargo tauri dev` from `app/src-tauri`), on a transcribed
    video of 30+ minutes:
    *   Phase 1: time the scan with and without a cached outline; count the
        candidates at `min_score` 40; make 6, export them, play every MP4;
        time one export from the last 5 minutes of the video before and
        after the render change.
    *   Phase 2: a payoff-first clip's MP4, Premiere XML, SRT and TXT agree
        on order; watch ffmpeg's peak memory (Activity Monitor) while
        rendering a 4K clip with two 20 s pieces out of order; it should stay
        near a single-piece render's.
    *   Phase 3 measurement: on two videos, grade every composite. Rubric: a
        person who hasn't seen the video watches it and answers yes/no to (1)
        does it make sense without context, (2) does the first piece hook,
        (3) is there a payoff, (4) does any jump feel broken. A composite
        passes with yes, yes, yes, no. Ship when at least half pass.

--------------------------------------------------------------------------------

*Drafted with EGM*
