# Tool API — the shared command surface

One tool registry, three callers: the local agent loop (Gemma via llama.cpp/Ollama),
external MCP clients (Claude Desktop via the stdio shim), and the UI (Tauri IPC).

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
invoke("mcp_endpoint_info")   // -> { url: string, token: string } for the MCP server
// plus bespoke shell commands OUTSIDE the registry (ADR-0004): demo_mode,
// setup_status, download_whisper_model, ollama_pull_model,
// install_llama_server, download_gguf, start_managed_llm, reveal_path,
// confirm_action, install_resolve_plugin, resolve_status, send_to_resolve,
// record_devices, record_start, record_status, record_stop
```

Events emitted by the core (listen via `@tauri-apps/api/event`):

| event | payload |
|---|---|
| `progress` | `{ task: ProgressTask, project_id?, fraction: number (0..1), message: string }` — task names are GENERATED (`core/src/events.rs` → `generated/ProgressTask.ts`): `transcribe`, `rough_cut`, `export`, `model_download`, `model_pull`, `runtime_install` |
| `agent-step` | `{ project_id, step: number, kind: "thinking"\|"tool_call"\|"tool_result"\|"final", tool?: string, args?: object, result?: object, text?: string }` |
| `timeline-changed` | `{ project_id }` — refetch `get_timeline` (TanStack Query invalidation) |
| `transcript-changed` | `{ project_id }` |
| `projects-changed` | `{}` — library changed (create/duplicate/delete/restore); refetch `list_projects` |
| `confirm-request` | `{ id, summary }` — externally-driven destructive op wants user approval (`confirm_action` Tauri command answers) |
| `mcp-ready` | `{ url, token }` — MCP server is up |
| `media-assets-changed` | `{ project_id }` — peaks/thumbnails/playable copy finished generating in the background; refetch `get_media_assets` |

### MCP
Same tools exposed over a localhost HTTP JSON-RPC endpoint guarded by a per-install
token; `mcp-shim` proxies stdio ⇄ that endpoint for Claude Desktop. Exception:
the orchestration meta-tools (`apply_instruction`, `connect_external`,
`escalate_to_frontier`) are not advertised over MCP — an external client IS an
orchestrator and should compose the granular tools / `apply_edits` itself.

## Tools

Errors: every tool returns either its result or `{ "error": { "code": string, "message": string } }`
(over Tauri IPC errors are rejections). Mutating tools record an `EditAction`
(undoable) tagged with `source: "ui" | "local_ai" | "mcp_client"`.

### Ingest & transcription
- `import_media { file_path, project_id? } -> Media` (with `project_id`, attaches the media to that project)
- `transcribe { project_id, language? } -> Transcript` (long-running; `progress` events)

### Analysis
- `detect_silences { project_id, min_duration_s? } -> { segments: [{start,end}] }`
- `detect_fillers { project_id } -> { segment_ids: string[], word_ranges: [{start,end}] }` (persists `is_filler` flags; stoplist comes from preferences)
- `detect_takes { project_id } -> { take_groups: [{ id, segment_ids, best_segment_id }] }` (persists take flags — same write path as the rough cut)
- `generate_rough_cut { project_id, aggressiveness?: "natural"|"aggressive" } -> { action: EditAction, timeline: Timeline, cut_count: number }`

### Editing
- `apply_edits { project_id, edits: [EditOp] } -> { applied, actions: [EditAction], cut_count, included_duration_s, source_duration_s }` — batch power tool for orchestrators (≤100 ops/call, each individually undoable; lean receipt, use `get_timeline` for clips)
- `cut_range { project_id, start, end } -> { action: EditAction, timeline: Timeline }`
- `restore_range { project_id, start, end } -> { action, timeline }`
- `cut_by_transcript { project_id, segment_ids: string[] } -> { action, timeline }`
- `restore_by_transcript { project_id, segment_ids } -> { action, timeline }`
- `trim_clip { project_id, clip_id, new_source_in, new_source_out } -> { action, timeline }`
- `split_clip { project_id, clip_id, at_time } -> { clips: [Clip, Clip], timeline, action }`
- `reorder_clip { project_id, clip_id, new_order } -> { action, timeline }`
- `set_global_padding { project_id, start_s, end_s, linked } -> { action, timeline }`

### Semantic / LLM
- `outline_transcript { project_id, refresh? } -> { beats: [{title, summary, role, cut_priority, segment_ids, start, end}], method: "llm"|"heuristic" }` — the FULL transcript split into story beats (chunked map for long videos; cached per transcript hash; pause-boundary sections without a model)
- `review_flow { project_id } -> { coherent, boundaries_checked, issues: [{kind, severity, at_segment, description, restore_segment_ids}], method }` — re-reads the edited transcript at every cut point: deterministic checks (mid-sentence cuts, orphaned connectives) + LLM judgment per boundary
- `story_edit { project_id, instruction, target_duration_s? } -> { actions, summary, before_s, after_s, coherent, issues_remaining }` — the cohesive pipeline in one call: outline → cut whole beats against the instruction → review every cut point → restore what reads broken → summarize. Long-running; frontier orchestrators may prefer composing the granular tools
- `plan_duration_cut { project_id, target_duration_s, apply? } -> plan | receipt` — ranks still-included segments by embedding centrality (tangents first, intro/outro protected). `apply: true` (recommended for orchestrators) plans AND cuts in one undoable step, returning `{ applied, action, before_s, included_duration_s, target_s, method, notes }`; without it, returns the full `{ segment_ids, projected_after_s, … }` plan for review
- `find_segments { project_id, query, limit? } -> { segments: [LeanSegment & {score}], method: "hybrid"|"bm25" }` — BM25 + local embeddings (Ollama `nomic-embed-text`) fused by reciprocal rank; BM25 alone when no index
- `read_transcript { project_id, offset?, limit? (≤200, default 50), include_words? } -> { total_segments, offset, returned, language, segments: [LeanSegment] }` — paged, word-arrays omitted by default; the right read for MCP clients and long videos
- `apply_instruction { project_id, instruction, history? } -> { actions: [EditAction], summary: string }` — runs the agent loop (`agent-step` events); `history` is recent chat turns `[{role: "user"|"agent", text}]` so follow-ups like "apply the edits" resolve

### Metadata
- `generate_chapters { project_id } -> { chapters: [{ title, start }] }`
- `generate_title_description { project_id } -> { titles: string[], description: string }`
- `generate_captions { project_id } -> { srt: string }`

### Export
- `export { project_id, target: "premiere_xml"|"fcp_xml"|"resolve_xml"|"edl"|"otio"|"mp4"|"srt", out_path } -> { path }`

### Project / state
- `create_project { name, file_path? } -> Project`
- `duplicate_project { project_id, name } -> Project` (save-as: same media + cut state, fresh undo history)
- `get_media_assets { project_id } -> MediaAssets` (peaks/thumbnails/playable-copy file paths; generated and cached on first call)
- `delete_project { project_id } -> { deleted: true }` — moves the project to the TRASH (restorable for 30 days, then purged at startup); never touches the media file
- `restore_project { project_id } -> Project` — bring a trashed project back, edit state and all
- `open_project { project_id } -> Project`
- `save_project { project_id } -> Project`
- `list_projects {} -> { projects: [ProjectSummary], trash: [ProjectSummary] }`
- `get_timeline { project_id } -> Timeline`
- `get_transcript { project_id } -> Transcript | null` (full fidelity incl. word timestamps — large; UI-oriented)
- `undo { project_id } -> { action: EditAction|null, timeline }`
- `redo { project_id } -> { action: EditAction|null, timeline }`
- `get_preferences {} -> Preferences`
- `set_preferences { preferences } -> Preferences`

### MCP client (opt-in; never auto-invoked)
- `connect_external { provider, api_key } -> ExternalConnection`
- `escalate_to_frontier { project_id, instruction, connection_id } -> { actions, summary }`

## Shapes (JSON, as the UI sees them)

> These types are GENERATED from the Rust model (`cargo test -p roughcut-core
> --features ts-bindings export_bindings` → `frontend/src/ipc/generated/`).
> The listing below is documentation; the generated files are the source.

```ts
interface Media { id: string; file_path: string; duration: number; frame_rate: number;
  width: number; height: number; audio_sample_rate: number; codec: string }

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
  | { type: "restore_range"; start: number; end: number }
  | { type: "cut_segments"; segment_ids: string[] }
  | { type: "restore_segments"; segment_ids: string[] }
  | { type: "trim_clip"; clip_id: string; new_source_in: number; new_source_out: number }
  | { type: "split_clip"; clip_id: string; at_time: number }
  | { type: "reorder_clip"; clip_id: string; new_order: number }
  | { type: "set_global_padding"; start_s: number; end_s: number; linked: boolean }
  | { type: "rough_cut"; aggressiveness: "natural" | "aggressive" }
  | { type: "set_clips"; clips: Clip[]; global_padding: Padding };

interface EditAction { id: string; kind: string; source: "ui"|"local_ai"|"mcp_client";
  timestamp: string; description: string; op: EditOp }
// exact inverse/redo snapshots live in the persisted per-project JOURNAL,
// not on the public action (ADR-0002)

interface Project { id: string; name: string; media: Media | null;
  timeline: Timeline; created_at: string; updated_at: string }
// Note: preferences are GLOBAL (get/set_preferences) and read fresh at use
// time — projects no longer carry a settings copy.

interface ProjectSummary { id: string; name: string; updated_at: string;
  deleted_at: string | null /* set = in the trash */ }

interface Preferences { default_padding: Padding;
  cut_aggressiveness: "natural" | "aggressive";
  custom_filler_words: string[]; silence_min_duration_s: number;
  export_target: string; language: string;
  model_tier: "auto" | "small" | "medium" | "large";
  inference_endpoint: string; inference_model: string; embedding_model: string }
```

## Demo mode

`FABLE_DEMO=1` (or no whisper/ffmpeg binaries found) switches the Transcriber /
VideoEngine adapters to fixture-backed mocks so the full edit loop is drivable
without media tooling installed. `import_media` accepts any path in demo mode.
