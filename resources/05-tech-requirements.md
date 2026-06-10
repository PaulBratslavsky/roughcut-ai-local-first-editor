# Technical Requirements

The engineering blueprint. Translates the requirements (Stage 3) and tech decisions (Stage 4) into concrete artifacts: data models, the tool/command surface, MCP exposure, component breakdown, the agent loop, and state. Tech context: **Rust core + Tauri v2 + React/TanStack web UI (one codebase, all platforms), Gemma 4 via llama.cpp/Ollama over an OpenAI-compatible API, whisper.cpp, FFmpeg (`ffmpeg-the-third`), `rmcp` for MCP, GPL-3.0.**

---

## Architecture layers (recap)

```
Tauri v2 + React/TanStack (one codebase, all platforms)
  • chat panel (React)               ──Tauri IPC──>  Rust core  ──traits──>  adapters
  • transcript/script panel (React)   (commands,       • agent loop          • VideoEngine (FFmpeg)
  • inspector / padding (React)        events,         • tool registry       • InferenceClient (llama.cpp/Ollama)
  • timeline (Canvas WebGL/WebGPU,     channels,       • project model       • Transcriber (whisper.cpp)
    TanStack Virtual)                  asset proto     • MCP server + client • Store (SQLite/files)
  • preview (WebCodecs / piped         for binary)     • undo/redo
    frames from FFmpeg)
```

The **tool registry** is the keystone: one set of tool definitions, invoked by either (a) the local Gemma 4 orchestrator, (b) an external MCP client like Claude Desktop, or (c) the UI directly. All three paths call the same core functions. Tauri being Rust means the core links directly in — no FFI bridge.

---

## Data models

All data is local. Times are in a consistent unit — recommend **rational frame-exact time** (e.g., ticks or a rational `num/den` like OTIO) internally, surfaced as seconds in the API. Below uses seconds (`f64`) for readability; implement frame-exact.

### Project
| Field | Type | Notes |
|-------|------|-------|
| id | uuid | primary key |
| name | string | e.g., "i-quit-rough-draft" |
| source_media_id | uuid | FK → Media |
| created_at | timestamp | |
| updated_at | timestamp | |
| timeline_id | uuid | FK → Timeline |
| settings | ProjectSettings | padding defaults, cut aggressiveness, language, export target |
| schema_version | int | for migration |

### Media
| Field | Type | Notes |
|-------|------|-------|
| id | uuid | primary key |
| file_path | string | absolute local path to source video |
| duration | f64 | seconds |
| frame_rate | f64 | fps (for frame-exact ops) |
| width / height | int | |
| audio_sample_rate | int | |
| codec | string | container/codec info |
| imported_at | timestamp | |

### Transcript
| Field | Type | Notes |
|-------|------|-------|
| id | uuid | primary key |
| media_id | uuid | FK → Media |
| language | string | detected/selected language |
| segments | [TranscriptSegment] | ordered |
| model_used | string | whisper model id (for reproducibility) |

### TranscriptSegment / Word
| Field | Type | Notes |
|-------|------|-------|
| id | uuid | |
| start / end | f64 | seconds, frame-aligned |
| text | string | |
| words | [Word] | each Word: { text, start, end, confidence } |
| is_filler | bool | flagged by filler detection |
| is_silence | bool | gap segment |
| take_group_id | uuid? | groups repeated takes of the same line |
| is_best_take | bool | within a take group |

### Timeline
| Field | Type | Notes |
|-------|------|-------|
| id | uuid | primary key |
| clips | [Clip] | ordered, the current cut |
| global_padding | Padding | { start_s: f64, end_s: f64, linked: bool } |

### Clip
| Field | Type | Notes |
|-------|------|-------|
| id | uuid | primary key |
| source_in / source_out | f64 | in/out points in the SOURCE media (seconds, frame-exact) |
| included | bool | false = cut (kept for non-destructive restore) |
| order | int | position in timeline |
| origin | enum | how it was created: ai_cut / manual / split |
| linked_segment_ids | [uuid] | transcript segments this clip covers |

> **Non-destructive principle:** cuts never delete source; a "cut" sets `included=false` or trims `source_in/out`. Everything is reversible.

### ProjectSettings / Preferences
| Field | Type | Notes |
|-------|------|-------|
| default_padding | Padding | global padding defaults |
| cut_aggressiveness | enum | natural / aggressive / custom |
| custom_filler_words | [string] | user stoplist |
| silence_threshold | f64 | dB / duration params |
| export_target | enum | premiere / fcp / resolve / mp4 / srt |
| language | string | preferred transcription language |
| model_tier | enum | auto / small / medium / large (hardware-based) |

### EditAction (for undo/redo + audit)
| Field | Type | Notes |
|-------|------|-------|
| id | uuid | |
| type | enum | cut / restore / trim / split / pad / reorder / ai_batch |
| payload | json | inverse-able description of the change |
| timestamp | timestamp | |
| source | enum | ui / local_ai / mcp_client (who made the edit) |

### ExternalConnection (optional, MCP client path)
| Field | Type | Notes |
|-------|------|-------|
| id | uuid | |
| provider | string | e.g., "claude" |
| api_key_ref | string | reference to OS keychain entry, NOT the raw key |
| enabled | bool | user opt-in flag |

---

## The tool / command surface (the core API)

These are the functions the Rust core exposes. **Each is callable by the local orchestrator, by an MCP client, and by the UI.** Defined once; surfaced to LLMs as JSON-schema tools and to the UI as core methods. Auth is N/A (local). Below: name, inputs, outputs, description.

### Ingest & transcription
- **`import_media(file_path) -> Media`** — open a local file, probe metadata.
- **`transcribe(media_id, language?) -> Transcript`** — on-device STT, time-aligned. Long-running → emits progress events.

### Analysis (the AI first pass)
- **`detect_silences(media_id, threshold?) -> [Segment]`** — find dead-air ranges.
- **`detect_fillers(transcript_id, custom_words?) -> [Segment]`** — flag filler words.
- **`detect_takes(transcript_id) -> [TakeGroup]`** — group repeated takes, mark best.
- **`generate_rough_cut(project_id, aggressiveness) -> Timeline`** — orchestrated: runs silence/filler/take detection and produces the initial included/excluded clip set. Returns cut count.

### Editing (timeline ops)
- **`cut_range(project_id, start, end) -> EditAction`** — exclude a time range (non-destructive).
- **`restore_range(project_id, start, end) -> EditAction`** — re-include.
- **`cut_by_transcript(project_id, segment_ids) -> EditAction`** — delete via transcript selection (the text-based editing path).
- **`trim_clip(clip_id, new_source_in, new_source_out) -> EditAction`** — drag-handle boundary adjustment, frame-exact.
- **`split_clip(clip_id, at_time) -> [Clip]`** — split at playhead.
- **`reorder_clip(clip_id, new_order) -> EditAction`** — reposition.
- **`set_global_padding(project_id, start_s, end_s, linked) -> EditAction`** — apply padding to all talking clips. (All mutating tools record an `EditAction` and are undoable; readers like `get_timeline` return current state.)

### Semantic / conversational (LLM-facing)
- **`find_segments(project_id, query) -> [Segment]`** — natural-language search over the transcript ("the tangent about my weekend", "where I fumble the demo"). The orchestrator uses this to locate, then calls `cut_*`.
- **`apply_instruction(project_id, instruction) -> [EditAction]`** — high-level entry the local model expands into a tool-call sequence (find → cut/pad/split), returned for review.

### Metadata
- **`generate_chapters(project_id) -> [Chapter]`** — from transcript (must-have).
- **`generate_title_description(project_id) -> {titles[], description}`** — should-have.
- **`generate_captions(project_id) -> SRT`** — caption export.

### Export
- **`export(project_id, target, out_path) -> file`** — target ∈ {premiere_xml, fcp_xml, resolve_xml, edl, mp4, srt}. MP4 renders via FFmpeg; XML/EDL via OTIO + target writers.

### Project / state
- **`create_project / open_project / save_project / list_projects`**
- **`undo(project_id) / redo(project_id) -> EditAction`**
- **`get_timeline(project_id) -> Timeline`** — current cut state for the UI.
- **`get_preferences / set_preferences`**

### MCP client (optional, opt-in)
- **`connect_external(provider, api_key) -> ExternalConnection`** — user-initiated.
- **`escalate_to_frontier(project_id, instruction, connection_id) -> [EditAction]`** — send context to a frontier model via MCP, return proposed edits for review. Never auto-invoked.

---

## MCP exposure

- **Server (`rmcp`):** registers the editing tools above (import, transcribe, detect_*, generate_rough_cut, cut_*, trim, split, set_global_padding, find_segments, generate_*, export, undo/redo, get_timeline). Tools are described with JSON schemas generated from the same Rust definitions the local orchestrator uses.
- **Transport (important):** Claude Desktop launches MCP servers as **stdio subprocesses** — it cannot launch the running GUI app. Pattern: the app listens on a **local socket/HTTP MCP endpoint**, and we ship a tiny **stdio shim binary** that Claude Desktop spawns, which proxies stdio ⇄ the running app's local endpoint. (MCP clients that support HTTP transport can connect to the endpoint directly.) If the app isn't running, the shim reports a clear error (or optionally launches it).
- **Local endpoint security:** any local process could otherwise drive the editor. The local MCP endpoint requires a **per-install auth token** (generated at first run, stored in the app's data dir, passed to the shim via its config). Bind to localhost only.
- **Client (`rmcp`):** optional outbound connection to a frontier MCP endpoint; gated behind explicit user opt-in and a locally stored credential reference.
- **Safety:** MCP-driven edits go through the same non-destructive `EditAction` path (tagged `source=mcp_client`) and are undoable. Destructive-feeling operations (export, overwrite) should require confirmation when invoked by an external client.

---

## The agent loop (local orchestrator)

1. User types an instruction in chat (or an MCP client sends one).
2. Core sends the instruction + current project context (transcript summary, timeline state) + the tool schemas to Gemma 4 via the OpenAI-compatible API.
3. Model returns tool call(s) — e.g., `find_segments("weekend tangent")` → core executes → returns matches → model calls `cut_by_transcript(segment_ids)`.
4. Core executes each tool against the project model, recording `EditAction`s.
5. Loop until the model returns a final message (no more tool calls).
6. UI shows the diff (what changed) for review; all changes undoable.

Constraints: constrain tool-call output with JSON-schema/GBNF for reliability; cap loop iterations; surface each step in the chat UI for transparency.

---

## Pages & key components (Tauri + React/TanStack)

### Main editor window
- **Purpose:** the single primary workspace (matches the Gling reference layout). One window, panel layout.
- **Components (React):**
  - **Files / Script tab switcher** (left panel header).
  - **Transcript/Script panel** — editable, time-linked text; show-cuts/skip-cuts toggle; search; chapter markers; "Enhance" entry point. Virtualized with **TanStack Virtual** for long transcripts.
  - **Chat panel** — conversational editing; shows the agent loop's steps and diffs; opt-in frontier toggle.
  - **Video preview** — playback of the current cut; cut counter ("205 Cuts"); playhead synced to timeline + transcript. Frames via **WebCodecs** where supported, else **piped from the Rust/FFmpeg side** over Tauri's asset/stream protocol.
  - **Timeline** — **Canvas (WebGL/WebGPU, e.g., PixiJS)**, *not* DOM: thumbnail filmstrip, audio waveform, playhead, draggable clip handles, split tool, zoom; play + speed (1x/2x); show-cuts/skip-cuts; Pace control. Virtualized for long footage.
  - **Inspector / Padding panel** — global padding (Start/End sliders, link toggle, Apply), cut aggressiveness, filler stoplist.
  - **Export control** — target picker + export.
- **First-run setup flow** — hardware detection, model download (sized to machine), offline-ready confirmation.

### State management
- **Source of truth:** the Rust core owns project/timeline state. The React frontend holds view state and subscribes to core updates (timeline changed, transcription progress, agent-loop steps) via **Tauri events/channels**. **TanStack Query** caches core reads and invalidates on events; **TanStack Store** holds local view state.
- **Large binary data** (frames, waveform peaks, thumbnails) flows over Tauri's **asset/custom protocol or a stream**, never JSON IPC.
- **Undo/redo:** owned by the core (`EditAction` stack), exposed via commands.
- **Persistence:** core writes project + preferences to local store on change/save.
- **No global client cache of media** — the UI requests frames/waveform data from the VideoEngine adapter on demand.

## Background / long-running tasks
- **Transcription** — async, progress events to UI.
- **Rough-cut generation** — async, may chain several detectors.
- **Export/render** — async with progress.
- **Model download (first run)** — async with progress; resumable if possible.
- **Local inference server lifecycle** — core starts/stops the llama.cpp/Ollama sidecar; health-check before agent calls.

## Environment / configuration (no secrets in repo)
- `MODEL_DIR` — where downloaded models live (default in app data dir).
- `INFERENCE_ENDPOINT` — local server URL (default `localhost:<port>`).
- `INFERENCE_RUNTIME` — `llamacpp` | `ollama`.
- `MODEL_TIER` — `auto` | `small` | `medium` | `large`.
- `FFMPEG_PATH` — bundled FFmpeg binary/lib location.
- `WHISPER_MODEL` — transcription model id.
- External API keys (if MCP client used) — stored in **OS keychain**, referenced by `api_key_ref`, never in config files or the repo.

---

### Open assumptions to confirm (Stage 5)
1. **Frame-exact internal time model** (OTIO-style rational time) rather than float seconds — agree? It matters for clean NLE export and accurate trims.
2. **`apply_instruction` as the single high-level LLM entry point** (model expands it into granular tool calls) vs exposing only granular tools and letting the model compose — I've included both; keep both?
3. **Core owns all state; the React frontend is a thin view layer** — agree? (This is what keeps the core portable and the tool registry the single source of truth.)
4. **External-client destructive ops require confirmation** (export/overwrite when driven by Claude Desktop) — keep this guard?
5. **Keychain for any external API keys** — agree, vs an encrypted local file?
