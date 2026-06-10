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
```

Events emitted by the core (listen via `@tauri-apps/api/event`):

| event | payload |
|---|---|
| `progress` | `{ task: "transcribe"\|"rough_cut"\|"export"\|"render", project_id?, fraction: number (0..1), message: string }` |
| `agent-step` | `{ project_id, step: number, kind: "thinking"\|"tool_call"\|"tool_result"\|"final", tool?: string, args?: object, result?: object, text?: string }` |
| `timeline-changed` | `{ project_id }` — refetch `get_timeline` (TanStack Query invalidation) |
| `transcript-changed` | `{ project_id }` |

### MCP
Same tools exposed over a localhost HTTP JSON-RPC endpoint guarded by a per-install
token; `mcp-shim` proxies stdio ⇄ that endpoint for Claude Desktop.

## Tools

Errors: every tool returns either its result or `{ "error": { "code": string, "message": string } }`
(over Tauri IPC errors are rejections). Mutating tools record an `EditAction`
(undoable) tagged with `source: "ui" | "local_ai" | "mcp_client"`.

### Ingest & transcription
- `import_media { file_path } -> Media`
- `transcribe { project_id, language? } -> Transcript` (long-running; `progress` events)

### Analysis
- `detect_silences { project_id, min_duration_s?, threshold_db? } -> { segments: [{start,end}] }`
- `detect_fillers { project_id } -> { segment_ids: string[], word_ranges: [{start,end}] }` (persists `is_filler` flags; stoplist comes from preferences)
- `detect_takes { project_id } -> { take_groups: [{ id, segment_ids, best_segment_id }] }` (persists take flags — same write path as the rough cut)
- `generate_rough_cut { project_id, aggressiveness?: "natural"|"aggressive" } -> { timeline: Timeline, cut_count: number }`

### Editing
- `cut_range { project_id, start, end } -> { action: EditAction, timeline: Timeline }`
- `restore_range { project_id, start, end } -> { action, timeline }`
- `cut_by_transcript { project_id, segment_ids: string[] } -> { action, timeline }`
- `restore_by_transcript { project_id, segment_ids } -> { action, timeline }`
- `trim_clip { project_id, clip_id, new_source_in, new_source_out } -> { action, timeline }`
- `split_clip { project_id, clip_id, at_time } -> { clips: [Clip, Clip], timeline, action }`
- `reorder_clip { project_id, clip_id, new_order } -> { action, timeline }`
- `set_global_padding { project_id, start_s, end_s, linked } -> { action, timeline }`

### Semantic / LLM
- `find_segments { project_id, query } -> { segments: [TranscriptSegment] }`
- `apply_instruction { project_id, instruction } -> { actions: [EditAction], summary: string }` (runs the agent loop; `agent-step` events)

### Metadata
- `generate_chapters { project_id } -> { chapters: [{ title, start }] }`
- `generate_title_description { project_id } -> { titles: string[], description: string }`
- `generate_captions { project_id } -> { srt: string }`

### Export
- `export { project_id, target: "premiere_xml"|"fcp_xml"|"resolve_xml"|"edl"|"otio"|"mp4"|"srt", out_path } -> { path }`

### Project / state
- `create_project { name, file_path? } -> Project`
- `delete_project { project_id } -> { deleted: true }` (removes edit state + journal; never touches the media file)
- `open_project { project_id } -> Project`
- `save_project { project_id } -> Project`
- `list_projects {} -> { projects: [ProjectSummary] }`
- `get_timeline { project_id } -> Timeline`
- `get_transcript { project_id } -> Transcript | null`
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
  timestamp: string; description: string;
  op: EditOp; inverse: EditOp; redo: EditOp }

interface Project { id: string; name: string; media: Media | null;
  timeline: Timeline; created_at: string; updated_at: string }
// Note: preferences are GLOBAL (get/set_preferences) and read fresh at use
// time — projects no longer carry a settings copy.

interface ProjectSummary { id: string; name: string; updated_at: string }

interface Preferences { default_padding: Padding;
  cut_aggressiveness: "natural" | "aggressive";
  custom_filler_words: string[]; silence_min_duration_s: number;
  export_target: string; language: string;
  model_tier: "auto" | "small" | "medium" | "large";
  inference_endpoint: string; inference_model: string }
```

## Demo mode

`FABLE_DEMO=1` (or no whisper/ffmpeg binaries found) switches the Transcriber /
VideoEngine adapters to fixture-backed mocks so the full edit loop is drivable
without media tooling installed. `import_media` accepts any path in demo mode.
