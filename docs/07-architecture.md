# 07 — Architecture & Implementation Guide

This document explains how RoughCut works end to end: every layer, every
feature, and where each one lives in the code. It is written for a developer
who has never seen the codebase and wants to either contribute or build their
own version of it. The planning docs (`01`–`06`) explain *why* the product is
shaped this way; this document explains *how* the shipped code actually does it.

Companion reference: [`tool-api.md`](tool-api.md) is the wire-level contract
for the tool registry. This document is the narrative around it.

---

## 1. The big picture

RoughCut is three programs that share one brain:

```
┌─────────────────────────────────────────────────────────────────────┐
│  Desktop app (Tauri v2)                                             │
│                                                                     │
│  ┌──────────────────────────┐      ┌─────────────────────────────┐  │
│  │  React frontend (WebView)│ IPC  │  Tauri shell (Rust)         │  │
│  │  panels, canvas timeline │◄────►│  app/src-tauri/src/lib.rs   │  │
│  │  TanStack Query + Store  │      │  4 commands, event bridge   │  │
│  └──────────────────────────┘      └──────────────┬──────────────┘  │
│                                                   │                 │
│                       ┌───────────────────────────▼──────────────┐  │
│                       │  roughcut-core (Rust crate)              │  │
│                       │  • domain model + frame-exact time       │  │
│                       │  • Editor engine (state, undo/redo)      │  │
│                       │  • TOOL REGISTRY (32 tools)              │  │
│                       │  • local agent loop (Gemma)              │  │
│                       │  • detection (silence/filler/takes)      │  │
│                       │  • export writers (XML/EDL/OTIO/SRT/MP4) │  │
│                       │  • SQLite store                          │  │
│                       │  • MCP server (localhost HTTP)           │  │
│                       └──┬──────────┬──────────┬─────────────────┘  │
│                          │          │          │                    │
│                  ┌───────▼──┐ ┌─────▼─────┐ ┌──▼──────────────┐     │
│                  │ ffmpeg / │ │ whisper-  │ │ Ollama / llama- │     │
│                  │ ffprobe  │ │ cli       │ │ server (HTTP)   │     │
│                  │ (CLI)    │ │ (CLI)     │ │ OpenAI-compat   │     │
│                  └──────────┘ └───────────┘ └─────────────────┘     │
└─────────────────────────────────────────────────────────────────────┘
                                                   ▲
                  ┌────────────────┐  stdio  ┌─────┴──────────┐
                  │ Claude Desktop │◄───────►│ roughcut-mcp-  │  HTTP + Bearer token
                  │ (or any MCP    │         │ shim (proxy)   │──► localhost MCP server
                  │  client)       │         └────────────────┘
                  └────────────────┘
```

The architecture is built around one idea: **a single tool registry with
three callers**. Every operation the app can perform — import, transcribe,
cut, trim, export — is a named tool with a JSON schema. The UI calls tools,
the local LLM agent calls tools, and external MCP clients (Claude Desktop)
call tools. There is exactly one implementation of each operation, and every
mutation, regardless of who triggered it, is recorded, undoable, and tagged
with its source (`ui` / `local_ai` / `mcp_client`).

Six principles shape everything (see `README.md` and `docs/06-build-spec.md`):

1. **Local-first / zero egress by default** — the app is fully functional with
   networking disabled; the only network calls are to localhost (Ollama) unless
   the user explicitly opts into a frontier provider.
2. **Non-destructive editing** — source media is never modified. Edits toggle
   visibility of clip ranges; nothing is ever lost.
3. **The Rust core owns all state** — the frontend is a pure view layer that
   reads via tools and re-reads when the core says something changed.
4. **One tool registry, three callers.**
5. **Frame-exact time internally, seconds at the API edge.**
6. **No accounts, no secrets in the repo.**

### Repository layout

```
core/          roughcut-core — the portable asset; all logic lives here
  src/model.rs        domain types (Project, Timeline, Clip, Transcript…)
  src/time.rs         frame-exact RationalTime, snapping, timecode
  src/engine.rs       the Editor: state, mutations, undo/redo
  src/tools.rs        the tool registry + dispatcher
  src/agent.rs        local LLM agent loop + transcript cleanup + metadata
  src/detect.rs       silence / filler / take detection, keyword search
  src/adapters/       traits + impls: video (ffmpeg), transcribe (whisper),
                      inference (OpenAI-compatible HTTP)
  src/export/         srt, edl, fcpxml, xmeml (Premiere/Resolve), otio
  src/store.rs        SQLite persistence
  src/events.rs       EventSink trait + payload builders
  src/mcp/            MCP server (axum) + frontier client / keychain
  src/demo.rs         fixture media + transcript for demo mode
  tests/e2e.rs        full edit loop + MCP-over-HTTP, runs on fixtures
  examples/agent_live.rs  smoke test against a real local model server

app/src-tauri/ Tauri v2 shell: 4 IPC commands, event bridge, MCP startup
frontend/      React + TanStack UI; runs in a plain browser via a mock API
mcp-shim/      tiny stdio⇄HTTP proxy so Claude Desktop can connect
docs/          planning docs (01–06), tool-api.md, this document
```

Cargo workspace members: `core`, `app/src-tauri`, `mcp-shim`
(root `Cargo.toml`). License: GPL-3.0.

---

## 2. The domain model (`core/src/model.rs`, `core/src/time.rs`)

### Project, Media, Transcript

A **Project** (`model.rs:439`) is the top-level unit: a name, optional
imported `Media`, a `Timeline`, per-project `Preferences`, and timestamps.
A project has at most one source video (MVP scope: single-track talking-head
footage).

**Media** (`model.rs:14`) is probe metadata only — path, duration, frame rate
(possibly fractional, e.g. 29.97), resolution, audio sample rate, codec. The
file itself is never touched.

A **Transcript** (`model.rs:50`) belongs to a media file and holds ordered
**TranscriptSegment**s (`model.rs:37`), each with:

- `start`/`end` in seconds, the spoken `text`, and word-level `words` —
  every **Word** has its own `start`/`end`/`confidence`. Word timing is what
  makes transcript-driven editing possible (click a word to seek, select words
  to cut a precise range).
- Analysis flags written by the detectors: `is_filler`, `is_silence`,
  `take_group_id` + `is_best_take` (repeated takes of the same line are
  grouped; the last take is presumed best).

### Timeline and clips — the editing model

The **Timeline** (`model.rs:108`) is the heart of the editor. It is a
**contiguous partition of the source duration into ordered `Clip`s**. Every
moment of source time belongs to exactly one clip; a clip is either
`included: true` (appears in the output) or `included: false` (a cut). This
representation makes "cutting" trivial and perfectly reversible: a cut is just
a region whose clip(s) are flipped to excluded.

Each **Clip** (`model.rs:89`) stores:

- `source_in` / `source_out` — frame-snapped boundaries in source seconds
- `included` — in or out of the output
- `order` — position in the timeline
- `origin: ClipOrigin` — provenance: `Initial | AiCut | Manual | Split`
- `linked_segment_ids` — which transcript segments motivated this cut, so the
  transcript panel can show a strikethrough exactly where the timeline has a cut

Every mutation ends with `Timeline::normalize()` (`model.rs:149`): sort by
`source_in`, drop empty clips, merge adjacent clips with the same state, and
recompute `order` and `cut_count`. Because normalize runs after every edit,
the partition invariant can never drift, and all editing operations are
implemented as simple range manipulations:

- `set_range_included(start, end, included, origin)` (`model.rs:204`) — the
  primitive behind cut/restore. It ensures boundaries exist at `start` and
  `end` (splitting covering clips), then flips the state of clips in between.
- `cut_linked(...)` (`model.rs:225`) — same, but records the motivating
  transcript segment ids on the affected clips.
- `trim_clip` (`model.rs:240`) — moves a clip's edges while keeping the
  partition contiguous by adjusting neighbors.
- `split_clip` (`model.rs:266`) — splits at a time, marking origin `Split` so
  normalize won't immediately merge the halves back.
- `apply_padding` (`model.rs:296`) — "breathing room": extends included clips
  slightly into adjacent excluded gaps. Implemented as an idempotent delta so
  re-applying with a new value adjusts rather than accumulates.
- `source_to_output(t)` (`model.rs:340`) — maps source time to output time,
  skipping cuts; used by the SRT writer and the player.

### Time: frame-exact inside, seconds at the edge

All public APIs (tools, JSON, frontend) use `f64` seconds — clean to read and
serialize. Internally, anything that must be exact goes through
`core/src/time.rs`:

- `snap_to_frame(seconds, fps)` (`time.rs:48`) — every boundary entering the
  timeline (cut, trim, split) is snapped to the nearest frame **before** the
  mutation, in `engine.rs`. So the model only ever contains frame-aligned values.
- `RationalTime` (`time.rs:12`) — integer `value/rate` arithmetic used by the
  export writers, so a 29.97fps timeline survives the round-trip to an NLE
  without float drift (rescaling goes through `i128`).
- `timecode(seconds, fps)` (`time.rs:62`) — SMPTE-style `HH:MM:SS:FF` for EDL.

### EditAction — the audit trail

Every mutation produces an **EditAction** (`model.rs:367`): kind, timestamp,
human-readable description, and crucially `source: ActionSource` —
`Ui | LocalAi | McpClient`. The UI's chat panel, the undo stack, and the MCP
audit story all hang off this one type: you can always answer "who did this
edit and when, and can I undo it?" (always yes).

---

## 3. The Editor engine (`core/src/engine.rs`)

The `Editor` is the single stateful object in the system. It is a cheap-to-
clone `Arc<Inner>` (`engine.rs:35`) holding:

- `store: Box<dyn Store>` — SQLite persistence
- `video: Box<dyn VideoEngine>` — ffmpeg or mock
- `transcriber: Box<dyn Transcriber>` — whisper.cpp or mock
- `sink: SharedSink` — where events go (the Tauri webview in production)
- `state: Mutex<HashMap<Uuid, ProjectState>>` — per-project in-memory cache:
  the `Project`, its `Transcript`, and the undo/redo stacks

### Bootstrap and demo mode

`Editor::bootstrap(sink)` (`engine.rs:71`) opens SQLite at the platform data
dir and picks adapters via `adapters::demo_mode()` (`adapters/mod.rs:16`):
if `FABLE_DEMO=1`, or ffmpeg/whisper aren't on PATH, the mock adapters are
used and the entire edit loop runs on fixture footage (`core/src/demo.rs`).
This is why CI and a fresh `git clone` work with nothing installed — demo
mode is a first-class citizen, not a test hack. `Editor::test_instance()`
(`engine.rs:89`) is the same thing with an in-memory store.

### The mutation funnel

Every edit goes through one function: `mutate_timeline()` (`engine.rs:337`).
It snapshots the timeline, runs the mutation closure, normalizes, records an
`UndoEntry { action, before, after }`, clears the redo stack, persists project
and transcript, and emits a `timeline-changed` event. This single funnel is
what guarantees properties the product promises: *everything* is undoable,
*everything* is persisted, and the UI *always* finds out.

Undo/redo (`engine.rs:622`) are plain stack pops that swap the stored `before`/
`after` timeline snapshots back in — simple because timelines are small values,
not object graphs.

The public editing methods (`cut_range`, `restore_range`, `cut_by_transcript`,
`trim_clip`, `split_clip`, `reorder_clip`, `set_global_padding`,
`generate_rough_cut`, …) are thin wrappers: snap incoming times to frames,
then call `mutate_timeline` with the corresponding `Timeline` operation.

### Transcription flow

`Editor::transcribe()` (`engine.rs:228`) extracts a 16 kHz mono WAV via the
video adapter, calls the transcriber (which emits `progress` events), persists
the transcript, emits `transcript-changed` — and then, when a local model
server is available, spawns a background **cleanup pass**
(`agent.rs:299`): the LLM fixes casing/punctuation/mishearings in chunks of 40
segments, constrained to *keep exactly the same number of words* so word-level
timestamps stay valid. `update_segment_texts()` (`engine.rs:296`) enforces the
word-count check before applying anything.

---

## 4. The tool registry (`core/src/tools.rs`)

This is the architectural centerpiece. All 32 tools are declared in
`all_defs()` (`tools.rs:39`) as name + description + JSON schema, and executed
by one async dispatcher. The same definitions are served to:

1. the **UI** — via the Tauri `call_tool` command
2. the **local agent** — formatted as OpenAI function-calling schemas
3. **MCP clients** — via `tools/list` / `tools/call`

| Group | Tools |
|---|---|
| Ingest | `import_media`, `transcribe` |
| Analysis | `detect_silences`, `detect_fillers`, `detect_takes`, `generate_rough_cut` |
| Range edits | `cut_range`, `restore_range`, `trim_clip`, `split_clip`, `reorder_clip`, `set_global_padding` |
| Transcript edits | `cut_by_transcript`, `restore_by_transcript`, `find_segments` |
| Agent | `apply_instruction` (meta-tool: runs the agent loop) |
| Metadata | `generate_chapters`, `generate_title_description`, `generate_captions` |
| Export | `export` (target: `premiere_xml \| fcp_xml \| resolve_xml \| edl \| otio \| mp4 \| srt`) |
| Project | `create_project`, `open_project`, `save_project`, `list_projects` |
| Read state | `get_timeline`, `get_transcript` |
| History | `undo`, `redo` |
| Settings | `get_preferences`, `set_preferences` |
| Frontier (opt-in) | `connect_external`, `escalate_to_frontier` |

Exact parameter and result shapes for every tool are in
[`tool-api.md`](tool-api.md); edit tools uniformly return
`{ action: EditAction, timeline: Timeline }`.

Two dispatch layers (`tools.rs:171` and `tools.rs:360`):

- `dispatch_basic()` — every tool *except* `apply_instruction` and
  `escalate_to_frontier`. This is what the agent loop calls, which is how
  recursion is prevented structurally rather than by prompt engineering.
- `dispatch()` — the full set; the entry point for the UI and MCP. It routes
  the two meta-tools to `agent.rs` / `mcp/client.rs` and delegates the rest.

The agent additionally sees only a curated 12-tool subset — `AGENT_TOOLS`
(`tools.rs:106`): search/read tools, the edit tools, `generate_rough_cut`, and
`generate_chapters`. Deliberately excluded: project lifecycle (the agent works
within the open project), `export` (handing off is a user decision), and the
frontier tools (opt-in means a human clicks).

**To add a feature to RoughCut, you almost always add a tool**: implement the
operation on `Editor`, add a `def(...)` entry, add a `dispatch_basic` match
arm — and it is instantly available to the UI, scriptable from Claude Desktop,
and (if you add it to `AGENT_TOOLS`) usable by the local agent.

---

## 5. Adapters: the portability boundary (`core/src/adapters/`)

Everything platform- or binary-dependent sits behind an async trait, with a
production implementation and a fixture mock:

| Trait | Production | Mock | What it does |
|---|---|---|---|
| `VideoEngine` (`video.rs:18`) | `FfmpegCli` | `MockVideoEngine` | `probe` (ffprobe JSON → `Media`), `render_mp4` (trim/concat filtergraph, libx264 crf 18), `extract_audio_wav` (mono 16 kHz PCM for whisper) |
| `Transcriber` (`transcribe.rs:13`) | `WhisperRs` (feature `whisper-native`), else `WhisperCli` | `MockTranscriber` | `WhisperRs` runs whisper.cpp **in-process** via whisper-rs (GPU/Metal where available, real progress via the model's callback); `WhisperCli` shells out to `whisper-cli -ojf` for core builds without the feature. Both resolve the model via `resolve_whisper_model()`: `WHISPER_MODEL` env, then the best file present in `<data dir>/models/` (large-v3-turbo-q5_0 → small-q5_1 → base) |
| `InferenceClient` (`inference.rs:67`) | `OpenAiCompatClient` | (offline fallback in agent.rs) | `chat()` against any OpenAI-compatible `/chat/completions` (Ollama, llama-server, vLLM…); `healthy()` = 2s-timeout GET `/models` |
| `Store` (`store.rs:12`) | `SqliteStore` | in-memory SQLite | documents-in-SQLite persistence (§8) |

The whisper model path comes from `WHISPER_MODEL` or defaults to
`<data dir>/models/ggml-base.bin`; the inference endpoint/model come from
preferences, seeded by `INFERENCE_ENDPOINT` (default
`http://localhost:11434/v1`) and `INFERENCE_MODEL` (default `gemma4:26b`).

Because the inference client speaks the OpenAI protocol, "local Gemma via
Ollama" and "Claude via api.anthropic.com" are the *same code path* — the
frontier escalation (§7) just constructs the client with a different base URL
and an API key.

---

## 6. The AI layer

### Deterministic detection (`core/src/detect.rs`)

The "AI first pass" is mostly *not* an LLM — it's fast, explainable heuristics
over the transcript:

- **Silences** (`detect.rs:26`): explicit silence segments plus gaps between
  speech longer than `silence_min_duration_s` (default 0.8s), merged.
- **Fillers** (`detect.rs:67`): a stoplist (`um, uh, uhm, er, ah, hmm, mhm,
  erm` + user's custom words). Whole segments of filler get `is_filler`;
  filler words *inside* sentences are returned as word-level time ranges so
  they can be cut individually.
- **Takes** (`detect.rs:97`): consecutive non-silence segments are compared by
  Jaccard similarity over normalized word sets; ≥ 0.5 similarity groups them
  as repeated takes, and the *last* take is marked best (people usually re-do
  a line until they nail it).
- **`find_segments`** (`detect.rs:209`): stopword-filtered keyword scoring,
  top 8 matches — used by the UI search and as the agent's retrieval tool.

`generate_rough_cut` (`detect.rs:155`, orchestrated by `engine.rs:583`)
composes them: remove silences (leaving a small "breath" margin — 0.12s
natural / 0.04s aggressive), remove filler segments and words, remove
non-best takes, then apply the user's default padding. All resulting cuts are
tagged `ClipOrigin::AiCut`. One click, undoable as a single action.

### The local agent loop (`core/src/agent.rs`)

`apply_instruction` runs a textbook tool-calling loop, entirely against a
local model:

1. **Context**: `project_context()` (`agent.rs:45`) renders the project stats
   and the full transcript as `id | start–end | flags | text` lines (capped at
   24 KB). The system prompt (`agent.rs:29`) tells the model the workflow:
   *find segments → inspect → apply the edit → reply with a one-sentence
   summary; make the requested change and nothing more.*
2. **Loop** (`run_instruction_with`, `agent.rs:118`): up to 12 iterations of
   chat-completion (temperature 0.1) with the 12 agent tools. Each tool call
   is dispatched through `dispatch_basic`, its result appended as a `tool`
   message, and an `agent-step` event emitted (`thinking` / `tool_call` /
   `tool_result` / `final`) so the chat panel can stream progress live.
   `EditAction`s are harvested from results into the final
   `InstructionOutcome { actions, summary }`.
3. **Graceful degradation** (`agent.rs:209`): if `healthy()` fails (no model
   server running), a deterministic fallback handles the common case — a
   cut-intent instruction is served by `find_segments` + `cut_by_transcript`
   on the best match; anything else gets an honest "this needs a model server"
   reply. The app never hard-requires the LLM.

The same loop also powers transcript cleanup (§3) and metadata generation
(`generate_chapters` / `generate_title_description`, `agent.rs:380` and
`agent.rs:441`), each with a heuristic fallback for offline use (chapters from
silence boundaries, title from the first strong sentence).

---

## 7. MCP: external agents drive the editor (`core/src/mcp/`, `mcp-shim/`)

### Server (`mcp/server.rs`)

When the app starts, the Tauri shell spawns `mcp::server::start(editor)`
(`server.rs:30`):

- an **axum** HTTP server on `127.0.0.1:<random port>`, single route
  `POST /mcp`, speaking JSON-RPC 2.0 (MCP protocol version `2024-11-05`)
- guarded by a **per-install 40-char Bearer token**, generated once and kept
  in the SQLite KV store
- the endpoint + token are written to `<data dir>/mcp.json` so local clients
  can discover the running instance

It implements `initialize`, `ping`, `tools/list` (all 32 defs), and
`tools/call` — which goes through the very same `dispatch()` with
`ActionSource::McpClient`. An edit made by Claude Desktop lands in the same
undo stack and triggers the same `timeline-changed` event, so the UI updates
live while an external agent edits.

### The shim (`mcp-shim/src/main.rs`)

Claude Desktop spawns MCP servers as stdio subprocesses, but our server must
live *inside* the running app (it needs the Editor). The shim bridges the two:
a ~100-line binary that reads JSON-RPC lines from stdin, discovers the app's
endpoint (CLI args → `ROUGHCUT_MCP_ENDPOINT`/`ROUGHCUT_MCP_TOKEN` env vars →
`mcp.json`), forwards each request over HTTP with the Bearer token, and writes
responses to stdout. Claude Desktop config just points at the shim binary
(see README "Claude Desktop (MCP)").

### Frontier escalation (`mcp/client.rs`) — the only egress in the app

`connect_external` stores an API key in the OS keychain (macOS `security`
CLI; elsewhere a 0600 file under `<data dir>/secrets/`) and records an
`ExternalConnection`. `escalate_to_frontier` maps the provider to an endpoint
(`claude`/`anthropic` → `api.anthropic.com`, `openai` → `api.openai.com`, or a
custom URL), builds an `OpenAiCompatClient` with the key, and runs the *same*
agent loop from §6 against it. Both tools are excluded from the agent's tool
set: only a human action can trigger network egress.

---

## 8. Persistence and events

### Store (`core/src/store.rs`)

Persistence is deliberately boring: **SQLite as a document store**. Tables
`projects`, `transcripts`, `preferences`, and `kv` each hold whole serialized
JSON documents — the Rust types in `model.rs` *are* the schema
(`schema_version` on `Project` reserves room for migrations). Everything lives
under the platform data dir (`store.rs:72`; macOS:
`~/Library/Application Support/roughcut/`) — `library.db`, `models/` for
downloaded weights, `mcp.json`, `secrets/`.

Saves happen automatically inside the mutation funnel; `save_project` exists
as a tool but users never need to think about saving.

### Events (`core/src/events.rs`)

The core pushes, the frontend pulls. `EventSink` is a one-method trait
(`emit(event, payload)`); production wires it to Tauri's event system, tests
use a channel. Five events cover everything:

| Event | Payload | Meaning |
|---|---|---|
| `progress` | `{ task, project_id, fraction, message }` | transcribe / rough cut / export progress |
| `timeline-changed` | `{ project_id }` | any edit, undo, redo |
| `transcript-changed` | `{ project_id }` | transcription or cleanup finished |
| `agent-step` | `{ project_id, step, kind, … }` | live agent progress for the chat panel |
| `mcp-ready` | `{ url, token }` | emitted by the shell once the MCP server is up |

Note the change events carry **no data** — just "something changed". The
frontend re-fetches via tools. This keeps the core's event surface tiny and
makes the frontend trivially correct (it can never render stale partial
updates; it always re-reads the source of truth).

---

## 9. The Tauri shell (`app/src-tauri/src/lib.rs`)

The shell is intentionally tiny (~80 lines). On setup it:

1. wraps the app handle in `TauriSink` (`lib.rs:13`) — the `EventSink` impl
   that forwards core events to the webview,
2. calls `Editor::bootstrap(sink)`,
3. registers `AppState { editor, mcp }` (`lib.rs:21`), and
4. spawns the MCP server, storing the endpoint and emitting `mcp-ready`.

It exposes six commands (`lib.rs`):

- **`call_tool(name, args)`** — the entire functional surface; delegates to
  `tools::dispatch(..., ActionSource::Ui)`
- `list_tools()` — registry introspection
- `mcp_endpoint_info()` — for a future "connect Claude Desktop" settings UI
- `demo_mode()` — so the UI can label fixture data (the demo banner)
- `setup_status()` — first-run toolchain report (`core/src/setup.rs`):
  ffmpeg resolved? which whisper model is installed? native engine compiled in?
- `download_whisper_model(tier)` — streams a model from Hugging Face into
  `<data dir>/models/` with `progress` events (task `model_download`),
  writing to a `.part` file and renaming on completion

There is no per-feature IPC. New core features need **zero shell changes**.

Config highlights (`tauri.conf.json`): 1440×920 window, drag-drop enabled,
asset protocol enabled with scope `**` (lets the `<video>` tag play local
files via `convertFileSrc`), Vite dev server pinned to port 1420.

---

## 10. The frontend (`frontend/`)

Stack: **React 18 + TypeScript (strict) + Vite**, with three TanStack
libraries doing the heavy lifting: **Query** (server state), **Store**
(ephemeral view state), **Virtual** (transcript list virtualization).

### The IPC seam and browser mock (`src/ipc/`)

`api.ts` is the only file that knows about Tauri:

- `callTool<T>(name, args)` (`api.ts:13`) — `invoke("call_tool", …)` in the
  app; `mockCallTool` in a plain browser (detected via `__TAURI_INTERNALS__`)
- `onAppEvent(event, handler)` (`api.ts:24`) — Tauri `listen` or the mock
  emitter
- `mediaSrc(path)` (`api.ts:41`) — `convertFileSrc` or `null`

`mockApi.ts` is a complete in-memory implementation of the tool registry —
fixture transcript, working undo/redo stacks, even simulated streaming
`agent-step` events. This is why `npm run dev` in a browser exercises the full
UX with nothing else installed, and why UI development never blocks on the
Rust build. `types.ts` mirrors the Rust model types by hand (the shapes are
frozen in `tool-api.md`).

### State management (`src/ipc/queries.ts`, `src/state/viewStore.ts`)

Two stores with a clean division:

- **Server state** lives in TanStack Query under keys `["projects"]`,
  `["project", id]`, `["timeline", id]`, `["transcript", id]`,
  `["preferences"]`. Each edit is a mutation hook wrapping `callTool`
  (`useCutByTranscript`, `useTrimClip`, `useApplyInstruction`, …) that
  invalidates the affected keys. On top of that,
  `useCoreEventInvalidation()` (`queries.ts:83`, mounted in `App.tsx`)
  subscribes to `timeline-changed` / `transcript-changed` and invalidates —
  so edits made by the agent or an MCP client refresh the UI exactly like the
  user's own clicks. This event→invalidate→refetch loop *is* the frontend
  architecture.
- **View state** lives in a small TanStack Store (`viewStore.ts:9`): playhead,
  `seekNonce` (bumped to force the `<video>` element to seek), playing/rate,
  timeline `zoom`/`scrollX`, selected clip and segment ids, `showCuts` /
  `skipCuts` toggles, and the active left-panel tab. Nothing here is persisted
  or owned by the core.

### Layout and panels

`App.tsx` renders the Gling-style layout: `TopBar` over a main row (left:
tabbed **Script/Chat** panel at 55%; right: **Preview** above **Inspector**)
over a bottom strip (**TransportBar** + canvas **Timeline**). Before a project
exists, `EmptyState.tsx` runs onboarding: pick/drop a file →
`create_project` → `import_media` → `transcribe`, with a progress bar driven
by `progress` events.

- **TranscriptPanel** (`panels/TranscriptPanel.tsx`) — the primary editing
  surface. Virtualized segment list; silences render as chips; cut content
  renders struck-through with a Restore button. Click a word →
  `seekTo(word.start)`. Select paragraphs (⌘-click multi-select) and press
  Delete → `cut_by_transcript`. Select *text across words* and right-click →
  context menu reads `data-wstart`/`data-wend` off the word spans and issues a
  precise `cut_range`. Toolbar hosts search and the one-click **Rough cut**
  button.
- **ChatPanel** (`panels/ChatPanel.tsx`) — conversational editing. Sends
  `apply_instruction`, renders streamed `agent-step` events as the agent
  thinks/calls tools, then the final summary with an "Undo this edit" link.
- **PreviewPanel** (`panels/PreviewPanel.tsx`) — a `<video>` element fed by
  `mediaSrc` (or an rAF clock in mock mode). Syncs play/pause/rate/seek with
  the view store; when **Skip cuts** is on, `skipTarget()` jumps the playhead
  over excluded ranges during playback — previewing the edit without
  rendering.
- **InspectorPanel** (`panels/InspectorPanel.tsx`) — padding sliders
  (`set_global_padding`), cut aggressiveness, custom filler words
  (`set_preferences`).
- **TopBar / ExportMenu** — project switcher, undo/redo, and export to the
  seven targets (writes to `~/Downloads/<project>-export.<ext>`).

### The canvas timeline (`src/timeline/`)

`Timeline.tsx` owns a DPR-aware canvas with an rAF draw loop and pointer
handling; `renderer.ts` is pure drawing + math, the most subtle part being
`buildTimeMap()` (`renderer.ts:14`): when **Show cuts** is off, excluded
ranges collapse and the map converts source↔display time both ways — layout,
scrubbing, and trims all round-trip through it. Clips render as rounded rects
(hatched when excluded) with deterministic placeholder waveforms (seeded per
clip id; real peaks are a planned M7 item). Drag a clip edge (6px hit zone) to
trim with live preview → `trim_clip` on release; click elsewhere to scrub;
wheel scrolls, vertical wheel zooms around the playhead. `TransportBar` has
play/pause, speed, the Show/Skip cuts toggles, split-at-playhead
(`split_clip`), and zoom buttons.

---

## 11. Feature walkthroughs (end to end)

**Import & transcribe.** Drop a file → `create_project` + `import_media`
(ffprobe → `Media`) → `transcribe`: ffmpeg extracts 16 kHz mono WAV →
`whisper-cli` writes full JSON → parsed into segments with per-word timing →
persisted, `transcript-changed` emitted → Query invalidates → transcript
renders. In the background, the LLM cleanup pass fixes punctuation without
moving a single timestamp.

**One-click rough cut.** Transcript panel button → `generate_rough_cut` →
detect silences/fillers/takes (§6) → one composed timeline mutation tagged
`AiCut` → `{ timeline, cut_count }` returns, badge shows "N cuts made". One
undo reverts the whole pass.

**Edit by transcript.** Select two rambling paragraphs, press Delete →
`cut_by_transcript` → `Timeline::cut_linked` flips those ranges to excluded
and links the segment ids → strikethrough appears in the script *and* the
timeline gap appears in the canvas, because both views re-derive from the
same timeline.

**Chat edit.** "cut the tangent about hiking" → `apply_instruction` → agent
calls `find_segments("hiking")`, inspects, calls `cut_by_transcript` →
`agent-step` events stream into the chat → summary + Undo link. Offline?
The deterministic fallback does the find+cut anyway.

**Preview the cut.** Toggle Skip cuts → playhead jumps over excluded ranges
during playback. Toggle Show cuts off → the timeline collapses cuts via the
time map, showing output time.

**Export.** ExportMenu → `export { target, out_path }` → the matching writer
(§ `core/src/export/`) renders included clips with `RationalTime` math:
`premiere_xml`/`resolve_xml` (FCP7 XMEML), `fcp_xml` (FCPXML 1.9), `edl`
(CMX3600), `otio` (OTIO JSON), `srt` (captions remapped to output time via
`source_to_output`), or `mp4` (ffmpeg trim/concat render).

**Claude Desktop session.** Claude spawns the shim → reads `mcp.json` →
`tools/list` → user says "tighten up my video" → Claude calls
`generate_rough_cut` over JSON-RPC → the running app's timeline updates live
in front of the user, tagged `mcp_client`, one Cmd-Z away from reverting.

---

## 12. Building, testing, and your own version

### Dev workflow

```sh
cargo test -p roughcut-core         # unit + e2e + MCP-over-HTTP, offline (fixtures)
cd frontend && npm run dev          # UI alone in a browser, full mock API
cargo tauri dev                     # the real app (frontend npm install first)
cargo run -p roughcut-core --example agent_live   # agent loop vs real Ollama
FABLE_DEMO=1 cargo tauri dev        # full app on fixtures, nothing installed
```

CI (`.github/workflows/ci.yml`) runs core tests on macOS/Linux/Windows with
`FABLE_DEMO=1`, builds the shim, builds the frontend, then builds the Tauri
app. The e2e test (`core/tests/e2e.rs`) is the best "executable documentation"
in the repo: it walks create → transcribe → rough cut → transcript edit →
trim → padding → chat instruction → undo → all exports → persistence, then
does the full JSON-RPC dance against a live MCP server.

### Extension points — where to plug in

| You want to… | Do this |
|---|---|
| Add an operation/feature | Add an `Editor` method → `def(...)` in `tools.rs` → `dispatch_basic` arm → (optional) mutation hook in `queries.ts` + UI. It's automatically available over MCP. |
| Let the agent use it | Add the name to `AGENT_TOOLS` (`tools.rs:106`). Mind the budget: more tools = worse small-model tool selection. |
| Swap the transcriber (e.g. whisper-rs, Parakeet) | Implement `Transcriber` (`adapters/transcribe.rs:13`), select it in `Editor::bootstrap_with_store`. Contract: segments + word-level timestamps. |
| Use a different LLM/server | No code: set `INFERENCE_ENDPOINT`/`INFERENCE_MODEL` (anything OpenAI-compatible). New protocol: implement `InferenceClient`. |
| Add an export format | New writer in `core/src/export/`, register it in `render_text_target` (`export/mod.rs:31`), add the target string to the `export` tool def and `ExportMenu`. |
| Add a panel | New component + `callTool`/query hooks; subscribe to events if it shows live state. No Rust changes needed. |
| Replace the storage layer | Implement `Store` (`store.rs:12`) — it's seven methods of JSON document I/O. |
| Embed the core elsewhere (CLI, server, another shell) | Depend on `roughcut-core`, provide an `EventSink`, call `Editor::bootstrap` + `tools::dispatch`. The Tauri shell is 80 lines; yours can be too. |

### Known gaps (honest list)

Per README status: no M0 WebView spike on Windows/Linux yet (WebCodecs/canvas
performance is the project's top technical risk), waveforms are placeholder
peaks, no WebCodecs frame-accurate preview, no bundled llama.cpp or ffmpeg
sidecars, and no signed bundles (M7). The CLI-based `WhisperCli` fallback
still reports no fine-grained progress; the default in-app `WhisperRs` engine
does.
