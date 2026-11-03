# 07 — Architecture & Implementation Guide

This document explains how RoughCut works end to end: every layer, every
feature, and where each one lives in the code. It is written for a developer
who has never seen the codebase and wants to contribute or build their own
version. The planning docs (`01`–`06`) explain *why* the product is shaped this
way; this document explains *how* the code does it.

Checked against the code on 2026-11-03 (branch `feat/shorts`, 166afad):
bulk shorts, the LOCAL / HOSTED switch, OBS import instead of in-app
recording, and the in-app terminal are all covered.

Companions:
- [`tool-api.md`](tool-api.md) — the wire-level contract for every tool,
  event and shell command.
- [`../CONTEXT.md`](../CONTEXT.md) — the domain glossary. Terms here follow it.
- [`designs/SITEMAP.md`](designs/SITEMAP.md) — every design doc and ADR.
- The module READMEs (`core/src/README.md`, `core/src/engine/README.md`,
  `core/src/adapters/README.md`, `core/src/mcp/README.md`, …) — the contract
  of each module in detail.

---

## 1. The big picture

RoughCut runs as **one desktop process**. Everything it needs runs on the
user's Mac.

```
┌─ your Mac ─────────────────────────────────────────────────────────────────┐
│                                                                            │
│  ┌─ RoughCut.app · one process ─────────────────┐   adapters               │
│  │                                              │                          │
│  │  React UI (WebView): screens, timeline       │   ┌───────────────┐      │
│  │     ▲ events             │ call_tool         │   │ ffmpeg /      │      │
│  │     │                    ▼                   │   │ ffprobe       │      │
│  │  Tauri shell: 25 commands, PTY               │   ├───────────────┤      │
│  │     ▲ events             │ dispatch          │   │ Ollama /      │      │
│  │     │                    ▼                   │   │ llama-server  │      │
│  │  ┌─ roughcut-core ──────────────────────┐    │   ├───────────────┤      │
│  │  │ Editor · registry (65) · agent loop  ├────┼──►│ Apple Vision  │      │
│  │  │ whisper.cpp, in-process              │    │   ├───────────────┤      │
│  │  │ MCP server 127.0.0.1:<port>/mcp      │    │   │ SQLite        │      │
│  │  └─────────────────▲────────────────────┘    │   └───────────────┘      │
│  └────────────────────┼─────────────────────────┘                          │
│                       │ POST /mcp + Bearer token                           │
│         ┌─────────────┴────────────────┐                                   │
│   In-app terminal                Claude Desktop                            │
│   Claude Code, direct HTTP       via roughcut-mcp-shim (stdio → HTTP)      │
└────────────────────────────────────────────────────────────────────────────┘
   Only Hosted mode (when the user turns it on) sends text to api.anthropic.com.
```

- The **React UI** draws the screens and keeps no data of its own.
- The **Tauri shell** passes calls in and events out, and hosts the in-app
  terminal's shell process.
- The **Rust core** (`roughcut-core`) owns every project and does all the work.
  It reaches the outside world only through **adapters**, so each helper can be
  swapped or mocked.
- Outside AI clients use the core's **MCP server**. Claude Code in the in-app
  terminal connects to it directly over HTTP; Claude Desktop (or Claude Code
  outside the app) goes through `roughcut-mcp-shim`.

### The principles every change keeps

1. **Local-first / zero egress by default.** The app works with networking off.
   The only outbound traffic is downloads the user starts (models, runtimes)
   and, only after the user turns it on, Hosted mode: text sent to Claude with
   the user's own key (ADR-0007). Embeddings never leave the machine.
2. **Non-destructive editing.** Source files are never changed. A cut hides a
   range; restoring brings it back.
3. **The Rust core owns all state.** The frontend reads through tools and
   re-reads when the core says something changed.
4. **One tool registry, three callers.** Every capability is one registry row;
   who may call it is decided by `Caller`, nowhere else.
5. **Frame-exact time inside, seconds at the API edge.**
6. **No accounts, no secrets in the repo.** The Hosted key lives in the OS key
   store.

### Repository layout

```
core/                 roughcut-core — all logic lives here
  src/tools.rs          the tool registry, Caller policy and every handler
  src/engine/           the Editor (see §3)
  src/model.rs          domain types (Project, Timeline, Clip, SavedClip, …)
  src/time.rs           frame snapping and timecode
  src/agent.rs          chat agent loop, transcript cleanup, metadata
  src/detect.rs         silences, fillers, takes, keyword search
  src/clip_finder.rs    short-form candidates (find_clips, the scan)
  src/story.rs          outline, flow review, story edits
  src/llm.rs            small JSON-prompt helpers over the inference client
  src/llm_runtime.rs    the managed llama-server (install, start, stop)
  src/adapters/         video (ffmpeg), transcribe (whisper), inference
                        (OpenAI-compatible), anthropic (Claude API), embed,
                        face (Apple Vision)
  src/export/           edl, fcpxml, xmeml (Premiere/Resolve), otio, srt, txt
  src/mcp/              MCP server (axum)
  src/pty.rs            the in-app terminal's shell
  src/store.rs          SQLite persistence
  src/events.rs         CoreEvent (every event the core can emit)
  src/secrets.rs        the Hosted API key in the OS key store
  src/setup.rs, capabilities.rs   first-run checks, demo-mode detection
  src/resolve.rs        DaVinci Resolve hand-off
  src/demo.rs           fixture media + transcript for demo mode
  tests/                e2e.rs, doc_parity.rs, mock_parity.rs
  examples/             live smoke tests (agent_live, playable_check, …)
app/src-tauri/        Tauri v2 shell (§12)
frontend/             React + TanStack UI; runs in a browser on a mock API
mcp-shim/             stdio ⇄ HTTP proxy for MCP clients that spawn a process
resolve-plugin/       DaVinci Resolve Scripts-menu integration
scripts/              build-macos-app.sh — signed local release build
docs/                 planning docs, tool-api.md, this document, adr/, designs/
```

Cargo workspace members: `core`, `app/src-tauri`, `mcp-shim`. License: GPL-3.0.

---

## 2. The tool registry (`core/src/tools.rs`)

This is the centrepiece. Every operation is one `ToolSpec` row in `REGISTRY`:
a name, a description, a JSON schema, a handler, and the flags `agent`,
`meta` and `mutating`. One async dispatcher runs them all:

```
tools::dispatch(editor, name, args, caller)   // run a tool
tools::defs(caller)                           // what this caller may see
```

There are **65 rows**. **28** are `agent: true` (the in-app chat assistant may
use them). **1** is `meta: true`: `apply_instruction`, which starts the agent
loop itself.

`Caller` decides what is visible *and* callable, so listing and calling can
never disagree:

| Caller | Who | Sees | Extra rules | Edits credited to |
|---|---|---|---|---|
| `Ui` | the user's clicks, via the Tauri `call_tool` command | all 65 | none | `ui` |
| `Agent { attribute_to }` | the chat assistant's loop | the 28 agent rows, never meta | arguments schema-checked | `attribute_to`: `local_ai` for the chat |
| `Mcp` | Claude Code, Claude Desktop, any MCP client | 64 (all but meta) | arguments schema-checked; `export`, `export_clip`, `export_clips` and `delete_project` wait for the user's approval (§9) | `mcp_client` |

Because the agent never sees meta tools, a loop can't start another loop: the
recursion guard is structural, not a prompt. The LOCAL / HOSTED switch, the
Hosted key and toolchain installs are **not tools at all**; they are Tauri
commands only the UI can call (ADR-0004, ADR-0007), so no AI client can turn
on egress or read the key.

### All tools by job (`*` = agent-visible, `†` = meta)

| Group | Tools |
|---|---|
| Media | `import_media`, `transcribe`, `append_media`, `attach_screen_media`, `set_layout`, `set_audio_offset`, `get_media_assets` |
| Analysis | `detect_silences`\*, `remove_silences`\*, `detect_fillers`, `detect_takes`, `generate_rough_cut`\* |
| Suggestions | `get_suggestions`\*, `accept_suggestion`\*, `accept_all_suggestions`\*, `dismiss_suggestion`\* |
| Range edits | `cut_range`\*, `restore_range`\*, `trim_clip`\*, `split_clip`\*, `set_global_padding`\*, `apply_edits`\* |
| Transcript | `cut_by_transcript`\*, `restore_by_transcript`\*, `find_segments`\*, `read_transcript`\*, `get_transcript` |
| Story | `outline_transcript`\*, `review_flow`\*, `story_edit`\*, `plan_duration_cut`\* |
| Agent | `apply_instruction`† |
| Metadata | `generate_chapters`\*, `generate_title_description`, `generate_captions` |
| Clips & shorts | `find_clips`\*, `get_clip_candidates`\*, `create_clip`\*, `create_clips`\*, `list_clips`\*, `update_clip`, `center_clip_on_face`, `delete_clip` |
| Export | `export`, `export_clip`, `export_clips` |
| Projects | `create_project`, `duplicate_project`, `delete_project`, `restore_project`, `open_project`, `save_project`, `list_projects` |
| History | `undo`, `redo`, `undo_actions`, `get_history`, `jump_to` |
| Checkpoints | `create_checkpoint`, `get_checkpoints`, `restore_checkpoint`, `delete_checkpoint` |
| Read / settings | `get_timeline`\*, `get_preferences`, `set_preferences` |

Exact arguments and results are in [`tool-api.md`](tool-api.md). Result shapes
vary: most edit tools return `{ action, timeline }`, `split_clip` adds the new
clip ids, `generate_rough_cut` adds `cut_count` and `suggestions`, and
`apply_edits` / `remove_silences` return lean receipts without the timeline.

---

## 3. The Editor (`core/src/engine/`)

The `Editor` is the one stateful object in the system: a cheap-to-clone handle
around shared state (`engine/mod.rs`). It holds the store, the adapters, the
event sink, a per-project cache (project, transcript, undo journal), and
smaller pieces of runtime state: pending confirmations, the running scans, the
model router.

| Submodule | Owns |
|---|---|
| `projects.rs` | create, open, save, list, duplicate; trash and restore; the preferences patch and its egress guard |
| `media.rs` | import and relink, transcription, `append_media`, legacy screen media |
| `edits.rs` | the edit seam, the journal, undo/redo, history and `jump_to`, checkpoints, rough-cut suggestions |
| `clips.rs` | saved clips and candidates, face framing, the one-scan-per-project guard |
| `semantic.rs` | the embedding index, hybrid search, `plan_duration_cut` |
| `confirm.rs` | approvals for destructive ops from MCP clients |
| `route.rs` | the LOCAL / HOSTED switch: which model each run uses |

### Bootstrap and demo mode

`Editor::bootstrap(sink)` opens SQLite in the platform data dir and wires the
adapters. Video and transcription use `AutoVideoEngine` / `AutoTranscriber`,
which pick the real tool or the fixture mock **on every call** through
`capabilities::probe()`. **Demo mode** means `FABLE_DEMO=1` is set or
ffmpeg/ffprobe can't be found; the whole edit loop then runs on fixture footage
(`demo.rs`). A missing whisper model does not start demo mode: `transcribe`
fails and says the model is missing. Bootstrap also purges trash older than 30
days and deletes the legacy plaintext `secrets/` folder.
`Editor::test_instance()` is the same with an in-memory store.

### The edit seam

Every change to the cut goes through `apply_edit` / `apply_edit_labeled` in
`engine/edits.rs`, whoever asked for it:

```
 React UI ──call_tool(cut_range)──► Tauri shell ──dispatch(Ui)──► tools.rs
 MCP client ──POST /mcp──────────────────────────dispatch(Mcp)──► tools.rs
                                                                     │
                                         handler builds an EditOp    ▼
                                                              Editor::apply_edit
                                   check bounds · snap to frames · apply_op
                                   Timeline::normalize · journal the inverse
                                                                     │
                                       save project + journal (SQLite)
                                                                     │
 React UI ◄── timeline-changed ──────────────────────────────────────┘
   drop cached data → get_timeline → re-render
```

- **Edits are data.** An `EditOp` is one of `CutRange`, `RestoreRange`,
  `CutSegments`, `RestoreSegments`, `TrimClip`, `SplitClip`,
  `SetGlobalPadding`, `RoughCut`, or `SetClips` (journal only). Tool handlers
  build the op; there are no per-edit Editor methods.
- **Undo stores snapshots, not math** (ADR-0002). Each `JournalEntry` keeps the
  action plus an inverse and a redo, both `SetClips` snapshots of the clips.
  Undo and redo (`step_history`) replay them through the same `apply_op`. The
  journal keeps the last 100 entries and is saved under kv `journal:{id}`.
- **Every applied edit is an `EditAction`**: id, op, description, time and
  source (`ui` / `local_ai` / `mcp_client`). History, `undo_actions` and
  `jump_to` all work off these.
- **Not everything is journaled.** Saved clips, checkpoints, layout, audio
  offset, suggestions and appended footage are saved immediately but are not
  undone by undo.
- Because an MCP edit goes through the same seam, the UI updates live while
  Claude edits, and one undo reverts an edit whoever made it.

### Transcription

`Editor::transcribe` (`engine/media.rs`) extracts a 16 kHz mono WAV from the
first audio track, runs the transcriber, saves the transcript, clears stale
suggestions, and emits `transcript-changed`. When a model is available it then
runs a **cleanup pass** (`agent.rs`): the model fixes casing, punctuation and
mishearings in chunks and must keep exactly the same number of words, so the
word timings stay valid (`clean_transcript` checks this). After cleanup the
**semantic index** is built (§6). In demo mode both steps are skipped.

---

## 4. The editing model (`core/src/model.rs`, `core/src/time.rs`)

### Project

A **Project** is the unit of work: a name, the source `Media`, a `Timeline`,
`saved_clips`, `clip_candidates`, checkpoints, suggestions, `audio_offset_s`,
`deleted_at` and `schema_version` (2; `Project::upgrade()` runs on every
load). Old two-file projects also keep `screen_media` and a `layout`. The
**Transcript** is stored separately, keyed by project. **Preferences** are
global (since schema v2), not per project.

**Media** is probe metadata only: path, duration, frame rate (possibly
fractional), resolution, audio sample rate, codec, import time. The file
itself is never touched.

A **Transcript** holds ordered segments with `start`/`end`, text, word-level
timings (`words`, each with its own start/end/confidence), and analysis flags
(`is_filler`, `is_silence`, take grouping).

### Timeline and clips

The **Timeline** is a contiguous partition of the source duration into ordered
`Clip`s. Every moment of source time belongs to exactly one clip, and a clip is
either included (in the output) or excluded (a cut):

```
imported            [────────────── included 0–60 s ──────────────]
cut_range(24, 33)   [── included 0–24 ──][/// cut ///][── 33–60 ──]
restore_range(…)    [────────────── included 0–60 s ──────────────]   ← normalize merges
```

Each `Clip` has `source_in` / `source_out`, `included`, `order`, an origin
(`Initial`, `AiCut`, `Manual`, `Split`) and `linked_segment_ids`, the
transcript segments that caused a cut (so the script shows a strike-through
exactly where the timeline has a gap).

`Timeline::normalize()` runs after every edit: it sorts clips by `source_in`,
drops empty ones, merges neighbours with the same state (except deliberate
splits of included clips) and renumbers. Because it always runs, the partition
can never drift, and every edit is a simple range operation:
`set_range_included` (cut / restore), `cut_linked`, `trim_clip` (with a small
magnet and a minimum kept length), `split_clip`, and `apply_padding` (an
idempotent "breathing room" delta).

### Time

Tools, JSON and the frontend use `f64` seconds. Every boundary that enters the
timeline is snapped to a frame first (`snap_to_frame`, applied in
`normalize_op` in `engine/edits.rs`), so the model only ever holds
frame-aligned values. The export writers work in whole frames (`to_frame`;
EDL timecodes from `timecode_frames`), so a 29.97 fps timeline survives the
trip to an NLE.

### Other things in the model

- **Checkpoint** — a named snapshot of the cut to jump back to. Up to 40;
  automatic ones are dropped first. The rough cut saves "Before rough cut"
  before it runs.
- **CutSuggestion** — a reviewable cut proposed by the rough cut (repeated
  takes, near-repeats). Accepting one applies it as an edit; dismissing drops it.
- **SavedClip** — a standalone short or excerpt: one or more pieces of the
  source, landscape or vertical (9:16), and `crop_x` for the vertical crop. It
  keeps the main cut's removals inside its pieces and isn't changed by later
  main-cut edits. Not the same thing as a timeline `Clip` (see CONTEXT.md).
- **ClipCandidate** — a moment `find_clips` proposed: pieces, title, hook,
  reason, score and the model that scored it. Kept until the next run.
- **Trash** — `delete_project` sets `deleted_at`; `restore_project` brings it
  back; projects in the trash longer than 30 days are purged at startup.
- **ModelMode / ModelStatus / RouteReason** — the LOCAL / HOSTED switch and
  what the top-bar chip shows (§6).

---

## 5. Chat versus the terminal

Both end up calling the same tools through the same seam. The difference is
**who runs the loop** and **which model thinks**.

```
 CHAT TAB                                   TERMINAL TAB
 you type ─► apply_instruction              Claude Code (its own loop, its own
                │                           model, online)
 ┌──────────────┼─── inside RoughCut.app ──────────┼─────────────────────────┐
 │              ▼                                  │ POST /mcp               │
 │   agent loop (core, ≤ 12 steps) ◄─► app's model │                         │
 │              │                      (Local or ◄─┼── AI tools it calls     │
 │              ▼                       Hosted)    ▼   still use this model  │
 │        28 agent tools                    64 MCP tools (risky ones ask)    │
 │              └──────────────► Editor ◄──────────┘                         │
 └───────────────────────────────────────────────────────────────────────────┘
```

| | Chat tab | Terminal tab |
|---|---|---|
| Who runs the loop | RoughCut, inside the core (`agent.rs`) | Claude Code, outside the app |
| Model | the app's model: Local or Hosted | Claude Code's own model |
| Tools | 28, about editing the open project | 64: projects, export, history, checkpoints, settings too |
| Works offline | yes, in Local mode | no |
| Edits credited to | `local_ai` | `mcp_client` |

AI-backed tools that Claude Code calls (`find_clips`, `story_edit`,
`outline_transcript`, `review_flow` with `llm`, `generate_chapters`) still run
on the **app's** model through `editor.inference()`.

---

## 6. The AI layer

### LOCAL or HOSTED (`engine/route.rs`, `adapters/anthropic.rs`, `secrets.rs`)

The user's LOCAL / HOSTED switch decides where every AI step runs (ADR-0007,
[`designs/hosted-models.md`](designs/hosted-models.md)).

- An AI step asks `editor.inference()` (one-off jobs) or
  `editor.inference_conversation()` (the chat). The mode is re-read on every
  request.
- **Local**: an OpenAI-compatible `/chat/completions` server, Ollama or
  llama-server (`adapters/inference.rs`). Defaults come from preferences,
  seeded by `INFERENCE_ENDPOINT` (`http://localhost:11434/v1`) and
  `INFERENCE_MODEL` (`gemma4:26b`). The app can install and run its own
  llama-server (`llm_runtime.rs`, port 47861).
- **Hosted**: a `HostedRouter` sends the same OpenAI-shaped request to Claude's
  Messages API, translated by `adapters/anthropic.rs`, with the user's key from
  the OS key store (`secrets.rs`; macOS Keychain service
  `dev.localfirst.roughcut.hosted`). Models: Opus, Sonnet and Haiku 5.5;
  default `claude-opus-5-5`. Text only: no video or audio ever leaves.
- **Failures are visible.** If Claude can't be reached, a one-off job falls
  back to the local model; a chat that already used Claude stops instead. A
  missing or rejected key is a `CoreError::Hosted` that every call site
  re-raises (`is_fatal_model_error`), never a quiet fallback to rules.
- **Only a human turns it on.** Mode, model, key and consent change only
  through Tauri commands; `set_preferences` rejects those fields. The chip
  follows `model-route` events.
- **Embeddings always stay local** (`adapters/embed.rs`, `OllamaEmbedder`,
  model `EMBEDDING_MODEL`, default `nomic-embed-text`).

### Rough cut (`detect.rs`, `engine/edits.rs`)

`generate_rough_cut` saves a "Before rough cut" checkpoint, then adds cuts on
top of what's already there:

- **Silences** — gaps between words, confirmed against the audio peaks. A small
  breath margin (0.12 s natural / 0.04 s aggressive) applies only where the
  audio didn't confirm the gap.
- **Fillers** — a stoplist (`um`, `uh`, `er`, …) plus the user's words. Whole
  filler segments, and filler words inside sentences as word-level ranges.
- **Takes and repeats are not cut.** Repeated takes (word overlap) and
  near-repeats (embedding cosine ≥ 0.93) become **suggestions** the user
  accepts or dismisses.

The whole pass is one undoable action, tagged `AiCut`.

### Search (`engine/semantic.rs`)

The embedding index (one vector per segment, in SQLite) is built after
transcript cleanup, and backfilled the first time an older project is opened.
The `find_segments` tool runs **BM25 and embedding cosine** and merges the two
rankings with reciprocal-rank fusion (`1 / (60 + rank)`); default limit 8,
clamped to 1–50. It reports `method: "hybrid"` or `"bm25"` (no index or no
embedder). The keyword-only `detect::find_segments` is used only by the
offline fallback.

### The chat assistant (`agent.rs`)

`apply_instruction` runs a tool-calling loop on the app's model:

1. **Context.** Project stats and the transcript as `id | start–end | flags |
   text` lines, plus the last turns of the chat. The system prompt tells the
   model the workflow: find segments at most once, batch edits, send silence
   requests to `remove_silences`, narrative edits to `story_edit`, length
   targets to `plan_duration_cut`, shorts to `find_clips` / `create_clip`, and
   handle suggestions with the suggestion tools.
2. **Loop.** Up to 12 steps (`MAX_STEPS`) with the 28 agent tools, each call
   dispatched with `Caller::Agent`. Every step is emitted as an `agent-step`
   event (`thinking` / `tool_call` / `tool_result` / `final`) so the chat panel
   streams it live.
3. **Guards for small models.** Repeated calls are answered from cache; two
   read-only rounds in a row get a steering message; bad arguments come back
   with the schema; `project_id` is filled in; `story_edit` ends the turn. The
   final receipt is built from the edits that actually landed. After cuts, the
   chat runs `review_flow` with the model to catch broken seams.
4. **Offline fallback (Local mode only).** If no model server answers, simple
   requests are handled by rules: a cut request becomes `find_segments` +
   `cut_by_transcript`, silence requests cut confirmed silences, shorts
   requests run `find_clips`. Anything else gets a plain "this needs a model
   server" reply. Hosted mode never falls back to rules.

### Story tools (`story.rs`)

- `outline_transcript` — beats in chunks of 120 segments, cached per
  transcript and model (kv `outline:{id}`).
- `review_flow` — finds seams that read broken after cuts (mid-sentence cuts,
  orphaned connectives). Rule-based by default; `llm: true` adds the model.
- `story_edit` — a whole story-level edit in one call: outline, choose beats to
  cut against the instruction, apply, re-read every cut point, restore what
  breaks the flow, summarize. Several model calls; one per project at a time.
- `plan_duration_cut` — ranks segments by importance to hit a target length;
  previews, then applies in one batch.

### Metadata (`agent.rs`)

`generate_chapters` and `generate_title_description` are single JSON prompts,
each with a rule-based fallback (chapters from silence boundaries, a title from
a strong early sentence). `generate_captions` needs no model: it returns SRT
written from the transcript.

---

## 7. From a recording to a transcript

The user records in OBS as one file and imports it. In-app recording was
removed (ADR-0008, [`designs/remove-recording.md`](designs/remove-recording.md)).

1. **Import.** `create_project { name, file_path }` probes the file with
   ffprobe and attaches it in one step. MP4, MOV, MKV, WebM and AVI work.
2. **Extract audio.** ffmpeg writes a 16 kHz mono WAV from the first audio
   track (OBS track 1).
3. **Transcribe.** `WhisperRs` runs whisper.cpp in-process (Metal on Apple
   silicon, real progress). It uses `WHISPER_MODEL` or the best model in
   `<data dir>/models/`: large-v3-turbo-q5_0, then small-q5_1, then base.
   `WhisperCli` is the fallback for core builds without the `whisper-native`
   feature.
4. **Clean and index** (§3, §6).
5. **Media assets.** `get_media_assets` builds waveform peaks and a thumbnail
   filmstrip in `cache/<key>/`. For a file the WebView can't play (MKV, WebM,
   AVI, or an MP4 with its index at the end) it starts a **playable copy**,
   `cache/<key>/play.mp4` (first video and first audio track, stereo AAC,
   H.264 when needed). `media-assets-changed` says when it's ready or failed;
   the preview shows "Preparing preview…" until then.
6. **Edit and export.** The preview plays the copy; edits, renders and exports
   always read the original file.

`append_media` (the Footage tab) joins more footage onto a project's source:
`video::concat_media` writes `<data dir>/media/joined-<ts>-<hex>.{mp4|mkv}`,
and the result is transcribed. Old projects with a separate screen file still
play and composite with their saved layout.

### The video adapter (`adapters/video.rs`)

`VideoEngine` covers `probe`, `extract_audio_wav`, `extract_frame`,
`media_assets` and `render_mp4`. A render takes a `RenderJob`: the pieces to
play, the audio offset, an optional screen file with a camera bubble, and an
optional 9:16 crop position (renders 1080×1920). Each group of pieces is one
seeked input, so long sources render quickly.

---

## 8. Clips and shorts

**Clips** (the Clips tab) are standalone excerpts saved from the transcript
(select text → "Add to clip") or from `find_clips` candidates. **Shorts** are
the same thing done in bulk, on their own screen
([`designs/bulk-shorts.md`](designs/bulk-shorts.md)):

1. **Scan.** `find_clips { scan: true }` (`clip_finder.rs`) uses the story
   beats to make sentence-aligned windows of 15–60 s, scores them by rules,
   optionally filters by a theme through hybrid search, then has the model
   rescore them in batches of 6 (a pool of 90 when scanning, 24 otherwise).
   Picks that overlap more than 30 % are dropped, and moments already saved as
   clips are skipped. One scan per project at a time.
2. **Candidates** are saved on the project; `clips-changed` tells the Shorts
   screen to show them as cards, best first.
3. **Review.** Clicking a card's picture plays its pieces with a centred 9:16
   window; the user ticks the ones they want.
4. **Make.** `create_clips` turns candidates into saved clips (vertical by
   default) and frames each on the speaker: `adapters/face.rs` uses Apple
   Vision to find faces in sampled frames, and the median position becomes
   `crop_x`. (`center_clip_on_face` does the same for one clip; it refuses
   projects that composite a screen file.) A candidate whose material was cut
   from the main edit since the scan is reported in `errors`.
5. **Export.** `export_clips` renders each clip into one folder as
   `NN-<title>.<ext>` (MP4 by default, 1080×1920 for vertical), never
   overwriting. One failure doesn't stop the rest.

Progress for scan, make and export is kept per project in the frontend's
`shortsStore`, so the user can leave the Shorts screen and come back.

---

## 9. MCP: outside clients drive the editor (`core/src/mcp/`, `mcp-shim/`)

### The server (`mcp/server.rs`)

When the app starts, the shell spawns `mcp::server::start(editor)`:

- **axum** on `127.0.0.1`, one route `POST /mcp`, hand-rolled JSON-RPC 2.0
  (ADR-0001). `initialize` echoes the client's protocol version and returns an
  `instructions` string that tells clients how to work: read the transcript
  yourself, locate with `find_segments`, snap cuts to words, batch with
  `apply_edits`, check with `review_flow`, and call `open_project` when the
  user asks to open or switch to a project.
- A **per-install Bearer token** (kv `mcp_token`).
- A **stable port**: saved in kv `mcp_port` and reused when free; a new port
  only if another program took it. So a client registered once keeps working.
- The endpoint and token are written to `<data dir>/mcp.json` for the shim.
- `tools/list` returns the 64 non-meta tools; `tools/call` goes through
  `dispatch(…, Caller::Mcp)`.

Two MCP-only behaviours:

- **Confirmations** (`engine/confirm.rs`). `export`, `export_clip`,
  `export_clips` and `delete_project` from an MCP client emit
  `confirm-request`; the app shows a dialog and answers with the
  `confirm_action` command. No answer in 120 s counts as no.
  `ROUGHCUT_NO_CONFIRM=1` turns this off.
- **Opening switches the app.** When an MCP client calls `open_project`, the
  core emits `project-opened` and the app shows that project. The UI's own
  `open_project` calls are refetches of what's on screen, so they never send it.

### Two ways in

| Route | For | How |
|---|---|---|
| Direct HTTP | Claude Code in the in-app terminal | The terminal's shell has `ROUGHCUT_MCP_URL` and `ROUGHCUT_MCP_TOKEN` in its environment. **Connect Claude Code** types `claude mcp add --transport http roughcut "$ROUGHCUT_MCP_URL" --header "Authorization: Bearer $ROUGHCUT_MCP_TOKEN"`, so the token never appears on screen. |
| The shim | Claude Desktop, or Claude Code outside the app | `roughcut-mcp-shim` finds the endpoint (arguments → `ROUGHCUT_MCP_ENDPOINT` / `ROUGHCUT_MCP_TOKEN` → `mcp.json`) and forwards each stdio line over HTTP with the token, retrying while the app starts. |

### The in-app terminal (`core/src/pty.rs`, `frontend/src/state/terminalSession.ts`)

`pty.rs` uses portable-pty to start the user's login shell (`$SHELL -l`) in
their home folder with `TERM=xterm-256color` and the MCP URL and token in its
environment. Output streams to the WebView as base64 `terminal-output` events
(raw bytes, so a character split across reads survives); `terminal-exit`
reports the end. The shell commands `pty_spawn`, `pty_write`, `pty_resize` and
`pty_kill` drive it.

On the frontend, `terminalSession.ts` owns **one** xterm and one shell for the
whole app. The editor's Terminal tab and the drawer under the Shorts player
move the same element between them (the newest host wins), so switching tabs
or screens never kills a running `claude`. The shell starts on first show;
only **Restart** or quitting the app ends it.

---

## 10. The frontend (`frontend/`)

Stack: **React 18 + TypeScript (strict) + Vite**, with TanStack **Query**
(data from the core), **Store** (view state) and **Virtual** (the transcript
list).

### Screens and layout

`viewStore.screen` is `"editor"` or `"shorts"`. With no projects,
`EmptyState` asks for a file.

```
┌──────────────────────────────────────────────────────────────────────────────────────────────────┐
│ TopBar: project picker · trash · LOCAL/HOSTED chip · Editor/Shorts · undo/redo · export · setup  │
├──────────────────────────────────────┬───────────────────────────────────────────────────────────┤
│ TranscriptPanel                      │ PreviewPanel (video, skip cuts, 9:16 crop guide)          │
│ click a word to seek; select text    ├───────────────────────────────────────────────────────────┤
│ to cut or add to a clip; review      │ tabs: tools · chat · clips · footage · history · terminal │
│ rough-cut suggestions                │                                                           │
├──────────────────────────────────────┴───────────────────────────────────────────────────────────┤
│ TransportBar: play, speed, show/skip cuts, split, zoom, level meter                              │
│ Timeline (canvas): real peaks + thumbnails, drag an edge to trim, right-click to cut             │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
```

The **Shorts** screen has the player, a note and the terminal drawer on the
left, and the candidate cards and saved shorts on the right.

| Panel | What it does |
|---|---|
| `TranscriptPanel` | The main editing surface: segments, silences, cuts (struck through), suggestions with accept/dismiss, a menu with Cut / Restore / Add to clip / Play from here, search, the Rough cut button |
| `ChatPanel` | Sends `apply_instruction` with the last 8 turns; streams `agent-step`; "Undo all N edits" calls `undo_actions` |
| `InspectorPanel`, `MetadataPanel` | The Tools tab: padding, aggressiveness, filler words, target length, audio sync, layout; chapters, title, captions |
| `ClipsPanel`, `SavedClipRow`, `CandidateCard` | Find and save clips; rename, format, crop, export |
| `ShortsScreen` | Scan, review cards, make and export shorts in bulk |
| `FootagePanel` | `append_media` + transcribe |
| `HistoryPanel` | `get_history` / `jump_to` |
| `TerminalPanel` | The shared terminal session and **Connect Claude Code** |
| `PreviewPanel`, `CropGuide` | Video (the playable copy when there is one), crop dragging, "◎ face" |
| `SetupPanel`, `ModelChip` | Toolchain status and installs; the LOCAL / HOSTED chip |

Keyboard (`hooks/useEditorKeyboard.ts`): Space, ←/→ by word, ⌥ by frame, ⇧ by
second, `?` for the shortcut sheet. Keys are ignored while typing, including
in the terminal.

### The IPC seam and the browser mock (`src/ipc/`)

- `api.ts` holds `callTool` (Tauri `invoke("call_tool")`, or the mock in a
  plain browser), `onAppEvent`, `mediaSrc`, and wrappers for about 20 shell
  commands (model switch, setup, local LLM, Resolve, terminal, `open_external`).
  A few files also use Tauri plugins directly: file dialogs, drag-and-drop,
  and the `confirm_action` answer in `App.tsx`.
- `mockApi.ts` implements every registry tool in memory (one project plus one
  in the trash), so `npm run dev` works with no Rust build; `core/tests/
  mock_parity.rs` fails if a tool is missing. URL flags like `?nomodel`,
  `?failexport`, `?failtranscribe` or `?failimport` simulate failures.
- **Types are generated from Rust** by ts-rs into `ipc/generated/` (ADR-0003).
  `types.ts` re-exports them; only event payloads and result envelopes are
  written by hand.

### Server state and events (`src/ipc/queries.ts`)

Data from the core lives in TanStack Query under keys such as `["projects"]`,
`["project", id]`, `["timeline", id]`, `["transcript", id]`,
`["media-assets", id]`, `["suggestions", id]`, `["history", id]`,
`["clips", id]` and `["clip-candidates", id]`. Mutation hooks wrap `callTool`.
`useCoreEventInvalidation()` (mounted in `App.tsx`) turns core events into
invalidations, so edits by the agent or an MCP client refresh the UI exactly
like the user's own clicks:

| Event | Invalidates |
|---|---|
| `timeline-changed` | timeline, project, projects, suggestions, history |
| `transcript-changed` | transcript, media-assets |
| `projects-changed` | projects |
| `media-assets-changed` | media-assets |
| `clips-changed` | clips, clip-candidates |

Other listeners: `progress` (progress bars, `shortsStore`), `agent-step`
(chat), `model-route` (`modelStore`), `confirm-request` (`App.tsx`),
`project-opened` (`App.tsx`), `terminal-output` / `terminal-exit`
(`terminalSession.ts`).

**Which project is open:** the one the user picked, if it still exists;
otherwise the most recently updated. `setProjectId` resets playhead, scroll,
selection, clip draft and crop. On `project-opened` the app refreshes the
project list, then switches, and stays on the current screen.

### View state (`src/state/`)

| Store | Holds | Saved |
|---|---|---|
| `viewStore` | screen, open project, playhead, zoom, scroll, selection, active right-hand tab, audition, clip draft, crop being framed | show/skip cuts, collapsed preview and the Shorts terminal drawer, in localStorage `rc-view-prefs` |
| `shortsStore` | per project: picks, ticks, scan / make / export progress and results | no |
| `modelStore` | the chip's copy of `model_status` (numbered reads, so a stale reply can't win) | no |
| `transcribeStore` | per project: the last transcription error, shown in the transcript panel and on the Shorts screen. An import transcribes after the app has switched to the new project, so `ingestFile` keeps its error here | no |
| `terminalStore` | whether the shell is running, or its exit code | no |

The theme (`theme.ts`) is saved as `roughcut-theme`; dark is the default.
`styles.css` is one sheet built on `:root` tokens, with `[data-theme="light"]`
swapping the variables.

### Playback and the timeline

`playback/engine.ts` (`usePlaybackEngine`) drives the `<video>` element: it
skips excluded ranges when **Skip cuts** is on, plays clip pieces back to back
for auditions, and keeps the audio offset. `timeline/Timeline.tsx` owns a
DPR-aware canvas; `renderer.ts` is pure drawing and math. Its `buildTimeMap()`
collapses excluded ranges when **Show cuts** is off and converts between
source and display time both ways. Real waveform peaks and thumbnails come
from `get_media_assets` (`timeline/assets.ts`); a seeded placeholder waveform
is only the fallback in the mock or before assets load.

---

## 11. Persistence and events

### Store (`core/src/store.rs`)

SQLite as a document store: the Rust types are the schema. Tables:
`projects`, `transcripts`, `preferences`, `kv`, and `embeddings` (project,
segment, model, f32 vector). The `Store` trait has 12 methods;
`Store::delete_project` hard-deletes, while `Editor::delete_project` only
moves a project to the trash.

```
~/Library/Application Support/roughcut/     (macOS data dir)
├── library.db     projects · transcripts · preferences · kv · embeddings
├── mcp.json       MCP URL + token, for the shim
├── models/        whisper models
├── cache/<key>/   waveform peaks, thumbnails, play.mp4, face samples
├── media/         footage joined by append_media
└── bin/           runtimes the app installed (e.g. llama-server)
```

kv keys: `journal:{id}` (undo history), `outline:{id}`, `mcp_token`,
`mcp_port`. The Hosted key is in the OS key store, never on disk. Saving is
automatic inside the edit seam; `save_project` exists but users never need it.

### Events (`core/src/events.rs`)

The core pushes, the frontend pulls. Every event is a `CoreEvent` variant
serialized at one point; `EventSink` is a one-method trait (Tauri in the app,
a channel in tests). There are 13:

| Event | Payload | Meaning |
|---|---|---|
| `progress` | `{ task, project_id?, fraction, message }` | long jobs: transcribe, rough_cut, export, model_download, model_pull, runtime_install, find_clips, make_clips, export_clips |
| `agent-step` | `{ project_id, step, kind, tool?, args?, result?, text? }` | live chat-assistant steps |
| `timeline-changed` | `{ project_id }` | any edit, undo or redo |
| `transcript-changed` | `{ project_id }` | transcription or cleanup finished |
| `media-assets-changed` | `{ project_id }` | peaks, thumbnails or the playable copy are ready |
| `projects-changed` | `{}` | project created, copied, deleted or restored |
| `project-opened` | `{ project_id }` | an MCP client opened a project: show it |
| `clips-changed` | `{ project_id }` | saved clips or candidates changed |
| `model-route` | `{ mode, effective, reason? }` | where AI steps really run changed |
| `confirm-request` | `{ id, summary }` | an MCP client wants to export or delete |
| `terminal-output` | `{ id, data }` | base64 shell output |
| `terminal-exit` | `{ id, code? }` | the shell ended |
| `mcp-ready` | `{ url, token }` | the MCP server is up; emitted directly by the shell |

The change events carry only an id. The frontend re-fetches through tools, so
it can never render a stale, half-applied state.

---

## 12. The Tauri shell (`app/src-tauri/src/lib.rs`)

One generic command runs every tool. The other commands are the things that
must stay out of the registry (OS access, the model switch and key, installs,
the terminal):

| Purpose | Commands |
|---|---|
| Core | `call_tool`, `list_tools`, `mcp_endpoint_info` (null until the server is up), `demo_mode`, `confirm_action` |
| OS | `reveal_path`, `open_external` (three allowed sites) |
| Setup | `setup_status`, `download_whisper_model` (checked against a pinned sha256) |
| LOCAL / HOSTED | `model_status`, `set_model_mode`, `set_hosted_model`, `set_hosted_key`, `check_hosted_key` |
| Local model | `ollama_pull_model`, `install_llama_server`, `download_gguf`, `start_managed_llm` |
| Terminal | `pty_spawn`, `pty_write`, `pty_resize`, `pty_kill` |
| DaVinci Resolve | `install_resolve_plugin`, `resolve_status`, `send_to_resolve` |

At startup the shell registers the single-instance plugin (a second launch
focuses the first window), the dialog plugin, an `EventSink` that forwards core
events to the WebView, and the Editor; then it starts the MCP server and emits
`mcp-ready`. On exit it stops the managed model server and kills every
terminal. The app builds the core with `whisper-native`.

Config (`tauri.conf.json`): drag-and-drop on, the asset protocol enabled (the
`<video>` element plays local files through `convertFileSrc`), minimum window
1100×700, Vite dev server on port 1420.

---

## 13. Feature walkthroughs

**Import and transcribe.** Drop an OBS file → `create_project { name,
file_path }` (probe + attach) → `transcribe` in the background: WAV → whisper
→ segments with word timings → saved, `transcript-changed` → the script
appears. Cleanup and the search index follow; peaks, thumbnails and (for MKV)
the playable copy land via `media-assets-changed`.

**One-click rough cut.** The Rough cut button → `generate_rough_cut` →
checkpoint, confirmed silences and fillers cut, takes and repeats offered as
suggestions → the badge reads "N cuts · M to review". One undo reverts it.

**Edit by transcript.** Select paragraphs and press Delete →
`cut_by_transcript` → `Timeline::cut_linked` excludes those ranges and links
the segment ids → the strike-through in the script and the gap in the canvas
both appear, because both re-read the same timeline.

**Chat edit.** "cut the tangent about hiking" → `apply_instruction` → the agent
calls `find_segments("hiking")`, then `cut_by_transcript` → `agent-step` events
stream into the chat → a summary with "Undo all N edits".

**Bulk shorts.** Shorts screen → Scan (`find_clips { scan: true }`) → cards →
tick → Make (`create_clips`, face framing) → Export (`export_clips` to a
folder).

**Claude Code session.** Terminal tab → Connect Claude Code → `claude` → "open
my AI doom video and make three shorts" → `open_project` (the app switches) →
`find_clips` / `create_clips` → the Shorts screen fills in live through
`clips-changed`; exports ask the user first.

**Export.** Export menu → `export { target, out_path }` → the matching writer
in `core/src/export/`: `premiere_xml` / `resolve_xml` (FCP7 XMEML), `fcp_xml`
(FCPXML), `edl` (CMX3600), `otio`, `srt` (captions in output time), `txt`
(plain transcript) or `mp4` (ffmpeg render). Files go to
`~/Downloads/<project>-export.<ext>` and are revealed in Finder; "Send to
DaVinci Resolve" appears when Resolve is installed.

---

## 14. Building and testing

```sh
cargo test -p roughcut-core            # unit + e2e + MCP-over-HTTP + parity guards (fixtures, offline)
cd frontend && npx tsc -b && npm run build
npm --prefix frontend run dev          # UI alone in a browser on the mock API
cargo tauri dev                        # the real app (from app/src-tauri)
FABLE_DEMO=1 cargo tauri dev           # full app on fixtures, nothing installed
cargo run -p roughcut-core --example agent_live      # agent loop against a real local model
cargo run -p roughcut-core --example playable_check  # OBS-style playable copy
scripts/build-macos-app.sh             # signed release RoughCut.app
```

- `core/tests/e2e.rs` is the best executable documentation: it walks the whole
  edit loop through the registry and drives a live MCP server over HTTP.
- `core/tests/doc_parity.rs` fails if a registry tool is missing from
  `tool-api.md` (or the doc lists one that doesn't exist).
- `core/tests/mock_parity.rs` fails if a registry tool is missing from the
  browser mock.
- **Signing.** `scripts/build-macos-app.sh` signs with a local "RoughCut Local
  Signing" identity when it exists (so camera and microphone grants survive
  rebuilds), ad hoc otherwise. CI's `release.yml` builds an Apple silicon
  `.dmg` and signs and notarizes it once the Apple secrets are set; unsigned
  until then.

---

## 15. Extension points

| You want to… | Do this |
|---|---|
| Add an operation | Implement it on the Editor (`core/src/engine/`) → a handler and a `tool!` row in `tools.rs` → document it in `tool-api.md` and add it to `mockApi.ts` (the parity tests insist) → a query or mutation hook and the UI. MCP clients get it with no extra work. |
| Let the chat assistant use it | Flag the row `agent: true`. Every extra tool makes a small model choose worse, so add only what it needs. |
| Change model types | Regenerate the frontend types: `TS_RS_EXPORT_DIR="$(pwd)/frontend/src/ipc/generated" cargo test -p roughcut-core --features ts-bindings export_bindings`. |
| Use a different local model or server | No code: change the endpoint and model in Setup (or `INFERENCE_ENDPOINT` / `INFERENCE_MODEL`). Anything OpenAI-compatible works. |
| Swap the transcriber | Implement `Transcriber` (`adapters/transcribe.rs`). Contract: segments with word-level timings. |
| Add an export format | A new writer in `core/src/export/`, registered in `export/mod.rs`; add the target to the `export` tool and the Export menu. |
| Add a panel | A component plus `callTool` / query hooks; subscribe to the events it shows. No Rust changes. |
| Embed the core elsewhere | Depend on `roughcut-core`, provide an `EventSink`, call `Editor::bootstrap` and `tools::dispatch`. |

---

## 16. Known issues (found while writing this)

- **`mcp-ready` bypasses `CoreEvent`.** The shell emits it directly,
  `CoreEvent::McpReady` is unused, and nothing listens for it.
- **Not yet built:** no WebCodecs frame-accurate preview. By design, ffmpeg,
  model weights and the llama-server runtime are not bundled; Setup finds them
  or downloads them on request (checksum-verified). CI builds and tests the core
  and the app on macOS, Linux and Windows, but day-to-day use is on macOS
  (Apple silicon).
