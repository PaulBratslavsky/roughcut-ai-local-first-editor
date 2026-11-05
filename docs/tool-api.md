# Tool API — the shared command surface

One tool registry, three callers: the in-app agent loop (the local model via
llama.cpp/Ollama, or Claude in Hosted mode), external MCP clients (Claude Desktop
via the stdio shim, Claude Code via the shim or directly over HTTP), and the UI
(Tauri IPC).

All times crossing this API are **seconds (f64)**. Internally the core is frame-exact
(rational time, OTIO-style `value/rate`).

## Transport

### UI (Tauri)
One generic command plus events:

```ts
// invoke
invoke<JsonValue>("call_tool", { name: string, args: object })
// returns the tool's JSON result, or rejects with { code, message }

// convenience commands (same dispatch underneath)
invoke("list_tools")          // -> ToolDef[] { name, description, input_schema }
invoke("mcp_endpoint_info")   // -> { url: string, token: string } | null (null until the MCP server is up)
// plus bespoke shell commands OUTSIDE the registry (ADR-0004):
//   core:      demo_mode, confirm_action
//   OS:        reveal_path, open_external (three allowed sites only)
//   setup:     setup_status, download_whisper_model (sha256-checked)
//   local LLM: ollama_pull_model, install_llama_server, download_gguf,
//              start_managed_llm
//   hosted:    model_status, set_model_mode, set_hosted_model,
//              set_hosted_key, check_hosted_key (see "Model switch")
//   terminal:  pty_spawn { id, cols, rows }, pty_write { id, data },
//              pty_resize { id, cols, rows }, pty_kill { id }
//   Resolve:   install_resolve_plugin, resolve_status, send_to_resolve
```

Events emitted by the core (listen via `@tauri-apps/api/event`); `mcp-ready`
is the one exception, emitted by the Tauri shell:

| event | payload |
|---|---|
| `progress` | `{ task: ProgressTask, project_id?, fraction: number (0..1), message: string }` — task names are GENERATED (`core/src/events.rs` → `generated/ProgressTask.ts`): `transcribe`, `rough_cut`, `export`, `model_download`, `model_pull`, `runtime_install`, `find_clips`, `make_clips` (framing in `create_clips`), `export_clips` (the whole batch; `project_id` set) |
| `agent-step` | `{ project_id, step: number, kind: "thinking"\|"tool_call"\|"tool_result"\|"final", tool?: string, args?: object, result?: object, text?: string }` |
| `timeline-changed` | `{ project_id }` — refetch `get_timeline` (TanStack Query invalidation) |
| `transcript-changed` | `{ project_id }` |
| `projects-changed` | `{}` — library changed (create/duplicate/delete/restore); refetch `list_projects` |
| `project-opened` | `{ project_id }` — an MCP client called `open_project`; the app switches to show that project (never sent for the UI's own calls) |
| `clips-changed` | `{ project_id }` — saved clips or clip candidates changed (by any caller); refetch them |
| `confirm-request` | `{ id, summary }` — externally-driven destructive op wants user approval (`confirm_action` Tauri command answers; no answer in 120 s = deny) |
| `mcp-ready` | `{ url, token }` — MCP server is up. Emitted by the Tauri shell, not through the core's event sink; the frontend doesn't listen for it |
| `terminal-output` | `{ id, data }` — a chunk of in-app terminal output; `data` is base64 of the raw bytes |
| `terminal-exit` | `{ id, code? }` — the terminal's shell exited |
| `media-assets-changed` | `{ project_id }` — peaks/thumbnails/playable copy finished generating in the background, or the playable copy failed; refetch `get_media_assets` |
| `model-route` | `{ mode, effective, reason? }` — where the AI steps actually run changed (Hosted fell back to Local, came back, or the key was rejected); `reason`: `offline` \| `rate_limited` \| `server_error` \| `key_rejected` \| `no_key`. The app's LOCAL / HOSTED chip follows it |

### Model switch (app commands, not tools)
Whether the AI steps run on the local model or on Claude (Hosted mode, ADR-0007) is the user's choice in the app. The switch, the hosted model and the API key are changed only by Tauri commands — `model_status`, `set_model_mode { mode, consent }`, `set_hosted_model { model }`, `set_hosted_key { key | null }`, `check_hosted_key` — never by a tool, so no MCP client or agent can turn Hosted on or read the key. `get_preferences` shows `model_mode` and `hosted_model`. In Hosted mode a step can fail with `hosted_no_key`, `hosted_not_consented`, `hosted_key_rejected` or `hosted_bad_request` (shown, never replaced by a rule-based result); `hosted_refused` / `hosted_truncated` affect one request.

### MCP
Same tools exposed over a localhost HTTP JSON-RPC endpoint,
`POST http://127.0.0.1:<port>/mcp` with `Authorization: Bearer <token>` (a
per-install token). The port is stable: saved and reused when free, a new one
only if it is taken. The app writes `{ url, token }` to `<data dir>/mcp.json`.
`mcp-shim` proxies stdio ⇄ that endpoint for Claude Desktop (or Claude Code).
Claude Code can also connect over HTTP directly: the in-app terminal's shell
has `ROUGHCUT_MCP_URL` and `ROUGHCUT_MCP_TOKEN` in its env, and its "Connect
Claude Code" button runs `claude mcp remove` then `claude mcp add --transport
http` with them (so a repeat click refreshes the registration).
`initialize` returns an `instructions` string on how to use the tools.
One tool is left out: the orchestration meta-tool `apply_instruction` is
neither advertised nor callable over MCP — an
external client IS an orchestrator and should compose the granular tools /
`apply_edits` itself. MCP arguments are schema-checked (required fields and
primitive types) before a handler runs.

### Who can call what
One `tools::Caller` decides visibility, argument checks and edit attribution,
so `tools/list` and `tools/call` can never disagree:

| Caller | Sees and may call | Args checked | Edits attributed to |
|---|---|---|---|
| `Ui` | every tool | no (typed frontend) | `ui` |
| `Mcp` | everything except meta tools | yes | `mcp_client` |
| `Agent` | only `agent`-flagged tools | yes | `local_ai` |

## Tools

Errors: every tool returns either its result or `{ "error": { "code": string, "message": string } }`
(over Tauri IPC errors are rejections). Mutating tools record an `EditAction`
(undoable) tagged with `source: "ui" | "local_ai" | "mcp_client"`.

### Ingest & transcription
- `append_media { project_id, file_path } -> { media, note }` — lossless concat to the END of the source (new file; originals untouched; cuts keep timestamps; undo history and checkpoints are extended to cover the new footage; the append itself is not undoable; re-run `transcribe` after)
- `import_media { file_path, project_id? } -> Media` — probes the file. With `project_id`: a project with no media gets it and a fresh timeline; a project whose video moved is relinked to the same video at a new path (matching duration, size, and frame rate keeps cuts, history, transcript, and clips); a different video is refused (`invalid_argument` — create a new project or use `append_media`)
- `transcribe { project_id, language? } -> Transcript` (long-running; `progress` events)

### Analysis
- `remove_silences { project_id, min_duration_s? } -> { applied, actions, note, cut_count, included_duration_s, source_duration_s }` — remove ONLY waveform-confirmed dead air, one undoable batch; never touches words, fillers, takes, or order
- `detect_silences { project_id, min_duration_s? } -> { method: "hybrid"|"transcript", segments: [{start, end, source: "confirmed"|"transcript_only", confidence}] }` — word-gap candidates confirmed + boundary-snapped against audio energy (file-adaptive thresholds; honest fallback without audio)
- `detect_fillers { project_id } -> { segment_ids: string[], word_ranges: [{start,end}] }` (persists `is_filler` flags; stoplist comes from preferences)
- `detect_takes { project_id } -> { take_groups: [{ id, segment_ids, best_segment_id }] }` (persists take flags — same write path as the rough cut)
- `get_suggestions { project_id } -> { suggestions: [{id, segment_ids, start, end, reason, confidence, duplicate_of, preview}] }` — pending rough-cut Tier-2 flags (repeated takes + repetition), not yet applied
- `accept_suggestion { project_id, suggestion_id } -> { action }` — apply one suggestion as an undoable cut
- `accept_all_suggestions { project_id } -> { action, accepted }` — apply all pending suggestions in one undoable batch
- `dismiss_suggestion { project_id, suggestion_id } -> { dismissed }` — discard one suggestion without cutting
- `generate_rough_cut { project_id, aggressiveness?: "natural"|"aggressive" } -> { action: EditAction, timeline: Timeline, cut_count: number, suggestions: number }`

### Editing
- `apply_edits { project_id, edits: [EditOp] } -> { applied, actions: [EditAction], cut_count, included_duration_s, source_duration_s }` (+ `requested`; on a mid-batch failure returns a PARTIAL receipt `{applied, requested, actions, failed_at, error, note}` — applied edits are journaled and undoable) — batch power tool for orchestrators (≤100 ops/call, each individually undoable; lean receipt, use `get_timeline` for clips)
- `cut_passage { project_id, from_segment_id?, to_segment_id?, start?, end? } -> { action, timeline, cut: { start, end, seconds, starts_with, ends_with } }` — cuts ONE whole passage (first to last segment, or seconds) in one undoable cut; both edges snap to the nearest sentence end or clear pause within ~10 s. Refuses spans over 30 minutes (use `plan_duration_cut`)
- `cut_range { project_id, start, end, snap? } -> { action: EditAction, timeline: Timeline }` — `snap: "word"|"sentence"` lands the cut on clean boundaries from word timings (default `none`); also accepted per-edit on `cut_range` ops inside `apply_edits`
- `restore_range { project_id, start, end } -> { action, timeline }`
- `cut_by_transcript { project_id, segment_ids: string[] } -> { action, timeline }`
- `restore_by_transcript { project_id, segment_ids } -> { action, timeline }`
- `trim_clip { project_id, clip_id, new_source_in, new_source_out } -> { action, timeline }`
- `split_clip { project_id, clip_id, at_time } -> { clips: [Clip, Clip], timeline, action }`
- `attach_screen_media { project_id, file_path } -> { attached }` — attach a screen recording that started at the same moment as the camera file (offset 0: both play on one clock)
- `set_layout { project_id, mode?, shape?, corner?, size? } -> Layout` — presentation compositing for projects with a screen recording (pip|camera|screen; rounded|round; tl/tr/bl/br; size 0.10-0.45); metadata, applied at preview/export
- `set_audio_offset { project_id, offset_s } -> { audio_offset_s }` — A/V sync nudge (±2s; positive delays audio); non-destructive metadata, applied at preview + MP4 export; not journaled
- `set_global_padding { project_id, start_s, end_s, linked } -> { action, timeline }`

### Semantic / LLM
- `outline_transcript { project_id, refresh? } -> { beats: [{title, summary, role, cut_priority, segment_ids, start, end}], method: "llm"|"heuristic" }` — the FULL transcript split into story beats (chunked map for long videos; cached per transcript hash; pause-boundary sections without a model)
- `review_flow { project_id, llm? } -> { coherent, boundaries_checked, issues: [{kind, severity, at_segment, description, restore_segment_ids}], method }` — re-reads the edited transcript at every cut point. Default `llm=false` runs only the instant deterministic checks (mid-sentence cuts, orphaned connectives — no model calls, safe for long videos); `llm=true` adds the app's model judgment per boundary (on-device, or Claude in Hosted mode). Run after every batch of cuts and restore what it flags.
- `story_edit { project_id, instruction, target_duration_s? } -> { actions, summary, before_s, after_s, coherent, issues_remaining }` — the cohesive pipeline in one call: outline → cut whole beats against the instruction → review every cut point → restore what reads broken → summarize. Long-running; frontier orchestrators may prefer composing the granular tools
- `plan_duration_cut { project_id, target_duration_s, apply? } -> plan | receipt` — groups still-included segments into whole sentences (punctuation or pauses), ranks them by embedding centrality and cuts the least-central first, gathered into runs (intro/outro protected). `apply: true` (recommended for orchestrators) plans AND cuts in one undoable step, returning `{ applied, action, before_s, included_duration_s, target_s, method, notes, passages }` where `passages` lists the cut stretches longest first (`{ start, end, seconds, starts_with }`, up to 15); without it, returns the full `{ segment_ids, projected_after_s, … }` plan for review
- `find_segments { project_id, query, limit? } -> { segments: [LeanSegment & {score}], method: "hybrid"|"bm25" }` — BM25 + local embeddings (Ollama `nomic-embed-text`) fused by reciprocal rank; BM25 alone when no index
- `read_transcript { project_id, offset?, limit? (≤200, default 50), include_words? } -> { total_segments, offset, returned, language, segments: [LeanSegment] }` — paged, word-arrays omitted by default; the right read for MCP clients and long videos
- `apply_instruction { project_id, instruction, history? } -> { actions: [EditAction], summary: string }` — runs the agent loop (`agent-step` events); `history` is recent chat turns `[{role: "user"|"agent", text}]` so follow-ups like "apply the edits" resolve

### Metadata
- `generate_chapters { project_id } -> { chapters: [{ title, start }] }`
- `generate_title_description { project_id } -> { titles: string[], description: string }`
- `generate_captions { project_id } -> { srt: string }`

### Export
- `export { project_id, target: "premiere_xml"|"fcp_xml"|"resolve_xml"|"edl"|"otio"|"mp4"|"srt"|"txt", out_path } -> { path }` — `txt` is the plain-text transcript of the current cut

### Clips
A saved clip is a standalone edit made of one or more pieces of the long-form video, played back to back in source order, in a 16:9 (`landscape`) or 9:16 (`vertical`, a short) frame. Its timeline is the current cut restricted to those pieces, snapshotted at save (main-cut edits and undo don't move it). Project metadata, not journaled. Not to be confused with the main timeline's clips (`trim_clip`, `split_clip`).
- `find_clips { project_id, theme?, count?, min_s?, max_s?, scan?, min_score?, multi_moment? } -> { candidates: [ClipCandidate], method: "llm"|"heuristic", theme }` — stretches of the current cut (default 15–60 s playable, 8 results, at most 60) that work as standalone shorts, ranked best first (hook, stands alone, payoff, ending). Each starts and ends on a whole sentence (whisper's segments are re-cut at sentence punctuation); with a model, the model reads each topic of the outline and picks each short's first and last sentence. Each candidate has `pieces: [SourceRange]`, the stretches it plays in play order (one for a single stretch); `start`/`end` cover them all. `scorer` names the model that picked it ("Claude Opus 5.5" in Hosted mode, or the local model's name; null for a rule-based score). `theme` keeps only moments about that topic (hybrid transcript search, then the model judges relevance); errors when nothing matches. `scan: true` has the model read every topic of the video (up to 40 requests; a plain run reads the best few topics), or with no model ranks moments from every story beat (every beat's best before any beat's second) — slower, minutes on a long video; the result still persists if a client stops waiting. `min_score` drops moments the model scored below it (rule-scored ones stay). `multi_moment` is accepted and ignored for now. A scan doesn't offer moments already saved as clips (sharing over 30% of the shorter one's source seconds), including one saved while it ran. One run per project at a time: a second errors `invalid_argument` ("a clip scan is already running for this project"). Picked by the app's model (on-device, or Claude in Hosted mode) when reachable; a topic whose reply is unusable keeps rule-scored moments. Replaces the stored candidates
- `get_clip_candidates { project_id } -> { candidates }` — the latest find_clips results, no recompute
- `create_clip { project_id, rank? | candidate_id? | segment_ids? | ranges? | start, end, title?, format? } -> { clip: SavedClipSummary }` — from a candidate by `rank` (1 = top) or id (removes it from the list; defaults to `vertical`), or from pieces: `segment_ids` (whole sentences, anywhere), `ranges: [{start, end}]`, or one `start`+`end` (defaults to `landscape`). Touching pieces merge
- `create_clips { project_id, candidate_ids: [id], format? } -> { clips: [SavedClipSummary], errors: [{ candidate_id, message }] }` — make several candidates at once (each `vertical` unless `format` says otherwise; each leaves the list), then frame every vertical clip's crop on the speaker (the same face pass `center_clip_on_face` runs, except a crop set by someone else meanwhile wins) before returning, emitting `make_clips` progress ("framing 2/6"). A candidate that no longer validates (its material was cut from the main edit since the scan) or isn't listed stays put and is reported in `errors`; the rest are still made. A clip the face pass can't frame keeps `crop_x` 0.5
- `list_clips { project_id } -> { clips: [SavedClipSummary] }` — `{ id, title, ranges, format, crop_x, start, end, duration, score, reason, hook, created_at }`, creation order
- `update_clip { project_id, clip_id, title?, format?, crop_x?, segment_ids? | ranges? | start, end } -> { clip }` — rename, switch format, move the vertical crop (`crop_x` 0 = left … 0.5 = centre … 1 = right), or replace the pieces (re-snapshots the current cut)
- `center_clip_on_face { project_id, clip_id } -> { clip, crop_x, frames_sampled, frames_with_face }` — frame the 9:16 crop on the speaker: grabs up to 12 frames across the clip's kept footage, finds faces on-device (Apple Vision; macOS only), and sets `crop_x` so the window centres on the median position of each frame's biggest face. `crop_x` is `null` (crop unchanged) when under a quarter of the frames show a confident face. Runs by itself in the background when a clip is created as `vertical` or switched to it with its crop never moved; a crop set meanwhile wins. Not for projects that composite a screen recording (set `crop_x` instead)
- `delete_clip { project_id, clip_id } -> { deleted }`
- `export_clip { project_id, clip_id, target, out_path } -> { path }` — the `export` targets over the clip's timeline; captions start at the clip's 00:00. A vertical clip's `mp4` renders 1080×1920, a full-height crop at `crop_x`. External (MCP) requests need the user's approval
- `export_clips { project_id, clip_ids: [id], out_dir, target? } -> { results: [{ clip_id, path, error }] }` — one file per clip into `out_dir` (`~` expands; created if missing), named `NN-<slug>.<ext>`: `NN` the 1-based position in `clip_ids` (two digits, three past 99), the slug the title in lowercase ASCII letters and digits with other runs as one `-` (at most 40 characters, `clip` if empty). An existing file is never overwritten: `-2`, `-3`, … is added. `target` defaults to `mp4`. One failure doesn't stop the rest; `results` follow `clip_ids` order with `path` or `error`. Each name is reserved on disk before its render, and a failed clip's file is removed. Progress is one `export_clips` task: `"2/6 · <title>"` at fraction `1/6` as each clip starts, then `"2/6 · <title> · rendering MP4… 40%"` at `(i + f) / n`, ending `"exported k of n"` (no per-clip `export` events). An unknown `project_id`, an empty `clip_ids`, or an unknown `target` is an error before anything is written or approved. Not available to the agent loop; external (MCP) requests need the user's approval once for the batch

### Project / state
- `create_project { name, file_path? } -> Project`
- `duplicate_project { project_id, name } -> Project` (save-as: same media + cut state, fresh undo history)
- `get_media_assets { project_id } -> MediaAssets` (peaks/thumbnails/playable-copy file paths, plus `playback_error` when the playable copy failed in this run; generated and cached on first call)
- `delete_project { project_id } -> { deleted: true }` — moves the project to the TRASH (restorable for 30 days, then purged at startup); never touches the media file
- `restore_project { project_id } -> Project` — bring a trashed project back, edit state and all
- `open_project { project_id } -> Project` — from an MCP client it also switches the app to that project (`project-opened`)
- `save_project { project_id } -> Project`
- `list_projects {} -> { projects: [ProjectSummary], trash: [ProjectSummary] }`
- `get_timeline { project_id } -> Timeline`
- `get_transcript { project_id } -> Transcript | null` (full fidelity incl. word timestamps — large; UI-oriented). Words are `{ text, start, end, confidence }` — the word key is `text`, not `word`. To cut on a clean boundary use `snap` on `cut_range`/`apply_edits` rather than parsing these.
- `undo_actions { project_id, action_ids } -> { undone, timeline }` — undo a specific recent action set (a chat turn); errors if newer edits exist so it can never revert unrelated work
- `create_checkpoint { project_id, name } -> { checkpoint }` — save the current timeline as a named snapshot (returnable regardless of undo state)
- `get_checkpoints { project_id } -> { checkpoints: [{id, name, created_at, cut_count, auto}] }` — saved checkpoints, newest first
- `restore_checkpoint { project_id, checkpoint_id } -> { action }` — restore a checkpoint as an undoable edit
- `delete_checkpoint { project_id, checkpoint_id } -> { deleted }` — delete a checkpoint
- `get_history { project_id } -> { entries: [{id, kind, source, description, timestamp, applied}], current_index }` — full edit history oldest→newest (applied then undone/future); current_index = newest applied (-1 = original)
- `jump_to { project_id, action_id? } -> { timeline }` — go back/forward to a history point (undo/redo until that action is newest applied); null action_id = the original
- `undo { project_id } -> { action: EditAction|null, timeline }`
- `redo { project_id } -> { action: EditAction|null, timeline }`
- `get_preferences {} -> Preferences`
- `set_preferences { preferences } -> Preferences` — a partial update: only the keys given change (nested objects like `default_padding` merge); unknown keys are rejected. Non-UI callers cannot point `inference_endpoint` at a non-loopback host (zero egress). `model_mode`, `hosted_model` and `hosted_consent_at` (the LOCAL / HOSTED switch) are read-only here for every caller: changing them fails with `invalid_argument`; only the app's own Setup and top-bar switch change them

## Shapes (JSON, as the UI sees them)

> These types are GENERATED from the Rust model (`cargo test -p roughcut-core
> --features ts-bindings export_bindings` → `frontend/src/ipc/generated/`).
> The listing below is documentation; the generated files are the source.

```ts
interface Media { id: string; file_path: string; duration: number; frame_rate: number;
  width: number; height: number; audio_sample_rate: number; codec: string;
  imported_at: string }

interface Word { text: string; start: number; end: number; confidence: number }

interface TranscriptSegment { id: string; start: number; end: number; text: string;
  words: Word[]; is_filler: boolean; is_silence: boolean;
  take_group_id: string | null; is_best_take: boolean }

interface Transcript { id: string; media_id: string; language: string;
  segments: TranscriptSegment[]; model_used: string }

interface Clip { id: string; source_in: number; source_out: number; included: boolean;
  order: number; origin: "ai_cut" | "manual" | "split" | "initial";
  linked_segment_ids: string[] }

interface Padding { start_s: number; end_s: number; linked: boolean }

interface Timeline { id: string; clips: Clip[]; global_padding: Padding;
  cut_count: number; duration: number /* source duration, s */ }

// Edits are DATA: a serializable op plus exact snapshot inverse/redo
// (see docs/adr/0002). The journal persists, so undo survives restarts.
type EditOp =
  | { type: "cut_range"; start: number; end: number }
  // (`snap` is a tool argument only — `cut_range` / `apply_edits` resolve it
  // to plain start/end before building the op)
  | { type: "restore_range"; start: number; end: number }
  | { type: "cut_segments"; segment_ids: string[] }
  | { type: "restore_segments"; segment_ids: string[] }
  | { type: "trim_clip"; clip_id: string; new_source_in: number; new_source_out: number }
  | { type: "split_clip"; clip_id: string; at_time: number }
  | { type: "set_global_padding"; start_s: number; end_s: number; linked: boolean }
  | { type: "rough_cut"; aggressiveness: "natural" | "aggressive" }
  | { type: "set_clips"; clips: Clip[]; global_padding: Padding };

interface EditAction { id: string; kind: string; source: "ui"|"local_ai"|"mcp_client";
  timestamp: string; description: string; op: EditOp }
// exact inverse/redo snapshots live in the persisted per-project JOURNAL,
// not on the public action (ADR-0002)

interface Project { id: string; name: string; media: Media | null;
  timeline: Timeline; created_at: string; updated_at: string;
  /* … plus checkpoints, suggestions, saved_clips, clip_candidates and more:
     see generated/Project.ts */ }
// Note: preferences are GLOBAL (get/set_preferences) and read fresh at use
// time — projects no longer carry a settings copy.

interface ProjectSummary { id: string; name: string; updated_at: string;
  deleted_at: string | null /* set = in the trash */ }

interface Preferences { default_padding: Padding;
  cut_aggressiveness: "natural" | "aggressive";
  custom_filler_words: string[]; silence_min_duration_s: number;
  export_target: string; language: string;
  model_tier: "auto" | "small" | "medium" | "large";
  inference_endpoint: string; inference_model: string; embedding_model: string;
  // The LOCAL / HOSTED switch: read-only here (changed only in the app).
  model_mode: "local" | "hosted"; hosted_model: string; hosted_consent_at: string | null }
```

## Demo mode

`FABLE_DEMO=1` (or no ffmpeg/ffprobe found) switches the Transcriber /
VideoEngine adapters to fixture-backed mocks so the full edit loop is drivable
without media tooling installed. `import_media` accepts any path in demo mode.
A missing whisper model does not start demo mode: `transcribe` fails with
`unavailable` instead.
