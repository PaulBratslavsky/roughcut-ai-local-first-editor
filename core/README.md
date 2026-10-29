# roughcut-core Blueprint

## System Overview

`roughcut-core` (`core/Cargo.toml`: "Portable core of the local-first AI video
editor: models, tool registry, agent loop, adapters, MCP") owns all of
RoughCut's state and behaviour. The Tauri app (`app/src-tauri`) and its
browser UI, external MCP clients (through the localhost server in `src/mcp`),
and the local chat agent act on a project by calling named tools in one
registry (`src/tools.rs`), which dispatch to the stateful `Editor`
(`src/engine`). The Tauri app also calls some core functions directly (see
Global Invariants). The `Editor` keeps projects, transcripts, and undo
journals in memory and persists them to SQLite. Video, transcription,
inference, and embeddings sit behind the adapter traits in `src/adapters`.
Face finding and capture are free functions there. Model-server probes and
the Ollama/llama-server runtime (`src/setup.rs`, `src/llm_runtime.rs`), and
the ffmpeg helpers `video::load_raw_peaks`, `video::ensure_playable_copy`, and
`record::combine_recordings`, call out without a trait.

Build features (`core/Cargo.toml`):

- `whisper-native` — in-process whisper.cpp via `whisper-rs` (off by default
  so core builds and tests stay light; the app crate turns it on). On macOS
  `whisper-rs` is built with `metal`.
- `ts-bindings` — `ts-rs` exports to `frontend/src/ipc/generated` (dev-time
  only) of the model types plus `ProgressTask` and the setup, Resolve,
  `MediaAssets`/`ThumbnailRef`, and recording types.
- macOS-only dependencies: `objc2`, `objc2-foundation`, and `objc2-vision` for
  on-device face finding.

Environment variables (read at runtime):

- `FABLE_DEMO=1` — force demo mode (fixture adapters).
- `ROUGHCUT_NO_CONFIRM=1` — skip confirmation prompts for MCP-requested
  writes.
- `INFERENCE_ENDPOINT`, `INFERENCE_MODEL`, `EMBEDDING_MODEL` — defaults for
  `Preferences`; saved preferences win.
- `WHISPER_MODEL`, `FFMPEG_PATH`, `FFPROBE_PATH` — file paths tried before the
  usual lookup.
- `FRONTIER_MODEL` — overrides the frontier provider's default model.

## Subdirectories & System Boundaries

- src: [./src/README.md](./src/README.md) - The crate source: the data model,
  the 66-row tool registry and its `Caller` policy, the AI layer (agent loop,
  story beats and flow review, clip finder, deterministic detection, strict
  JSON LLM calls), SQLite persistence, events, errors, frame snapping,
  provisioning, in-app terminals (`pty`), and the DaVinci Resolve bridge
  (`resolve`). Child packages:
  - [./src/adapters/README.md](./src/adapters/README.md) - `VideoEngine`,
    `Transcriber`, `InferenceClient`, `Embedder`, face finding, and capture.
    Video, transcription, and embeddings have `Mock*` fixtures.
  - [./src/engine/README.md](./src/engine/README.md) - `Editor`: the project
    cache, the journaled edit path, media and transcription, saved clips,
    semantic search, and confirmations.
  - [./src/export/README.md](./src/export/README.md) - NLE interchange, SRT,
    plain text, and MP4 export.
  - [./src/mcp/README.md](./src/mcp/README.md) - The localhost MCP server and
    the opt-in frontier-provider client.
- examples: [./examples/README.md](./examples/README.md) - Manual smoke tests
  and diagnostics (`cargo run -p roughcut-core --example <name>`) that drive
  the real API against real devices, services, and files. Compiled but not run
  by `cargo test`.
- tests (no contract; test-only) - `e2e.rs` (the editing loop through the
  registry, on fixture adapters), `doc_parity.rs` (every tool is documented in
  `docs/tool-api.md`), and `mock_parity.rs` (every tool exists in the browser
  mock `frontend/src/ipc/mockApi.ts`).

## Global Invariants

- One tool registry, three callers. Every operation MCP clients and agent
  loops can reach is a `ToolSpec` row in `src/tools.rs`; `Caller::can_use`
  alone decides who may see and call it (the UI sees all 66 tools, MCP clients
  63, the agent loop 27), and `tools::dispatch` is their single entry point.
  The Tauri app (`app/src-tauri/src/lib.rs`) also calls the core outside the
  registry: recording, setup and model downloads, the LLM runtime, terminals,
  Resolve, and confirmation answers are app commands only, and
  `send_to_resolve` and `attach_screen_media` call `Editor::export` and
  `Editor::attach_screen_media` directly.
- The core owns all state. Projects, transcripts, and journals live in the
  `Editor`'s cache and the `Store`; front ends read through tools and refresh
  on `CoreEvent`s (`src/events.rs`).
- Editing is non-destructive. A cut flips a timeline clip's `included` flag;
  source media is never modified. Journaled edits go through
  `Editor::apply_edit` and are undoable (snapshot inverses). Not every
  timeline change is journaled: `append_media` extends the timeline and swaps
  the media outside `apply_edit` (it extends the existing history to match, and
  its registry row is not `mutating`), and `import_media` only relinks the same
  video or sets media on a project that has none.
- Times are `f64` seconds (`src/model.rs`), snapped to source frames at the
  edges (`src/time.rs`). `normalize_op` snaps `CutRange`, `RestoreRange`,
  `TrimClip`, and `SplitClip`, and saved-clip ranges are snapped;
  `CutSegments`, `RestoreSegments`, and `RoughCut` use transcript and detector
  times as they are. Exporters convert to frames with `time::to_frame`;
  `RationalTime` is unused outside `src/time.rs`.
- Zero egress by default. Model calls go to the configured inference endpoint
  (local by default); non-UI callers cannot point it at a non-loopback host
  (`Editor::update_preferences`), and the frontier client (`src/mcp/client.rs`)
  is reachable only from the UI's meta tools.
- Externally requested writes need the user's approval. `export`,
  `export_clip`, and `delete_project` from MCP clients
  (`ActionSource::McpClient`) wait on `Editor::request_confirmation`, which
  emits a `ConfirmRequest` and denies after 120 s without an answer.
  `Editor::bootstrap_with_store` enables this unless `ROUGHCUT_NO_CONFIRM=1`;
  `Editor::test_instance` leaves it off.
- The tool contract and the browser mock stay in step with the registry: the
  `doc_parity` and `mock_parity` tests fail otherwise.
- The editing loop runs on fixtures in demo mode (`FABLE_DEMO=1`, or no
  ffmpeg/ffprobe; `capabilities::probe`) and in `Editor::test_instance`:
  video and transcription fall back to `MockVideoEngine` and
  `MockTranscriber`. With ffmpeg present but no whisper engine, transcription
  returns `Unavailable`. Embeddings have no automatic fallback (`MockEmbedder`
  is installed only by `set_embedder_for_tests`); with no embedding server,
  indexing is skipped and search falls back to BM25. With no model server, AI
  features fall back to heuristics, except `story_edit` without a target
  duration, which returns `Unavailable`. Capture and face finding have no
  fallback and need macOS.

## Global Side Effects

- **SQLite**: `library.db` under `<OS data dir>/roughcut` (`store::data_dir()`),
  tables `projects`, `transcripts`, `preferences`, `kv`, `embeddings`.
  `Editor::bootstrap_with_store` purges trash older than 30 days.
- **Files**: media caches under `<data dir>/roughcut/cache/`, downloaded models
  under `<data dir>/roughcut/models`, binaries under `<data dir>/roughcut/bin`,
  the frontier API key at `<data dir>/roughcut/secrets/<key_ref>` when the
  Keychain is unavailable, recordings under `~/Movies/RoughCut`, `mcp.json`
  (URL and bearer token for `mcp-shim`), the Resolve plugin scripts in
  Resolve's `Fusion/Scripts/Utility` folder, `roughcut-*` temp files (the
  transcription WAV, ffmpeg concat lists, the Resolve import script), and
  export files at caller-supplied paths.
- **Subprocesses**: ffmpeg/ffprobe (probe, render, capture, peaks, frames),
  whisper-cli (when not built with `whisper-native`), Ollama and llama-server,
  a login shell for in-app terminals, `python3` for DaVinci Resolve,
  `security` for the Keychain, `tar` (llama-server install), and `which`,
  `sysctl`, and `df` for capability, RAM, and disk-space checks.
- **Network**: the inference endpoint (chat, embeddings, health probes),
  user-triggered downloads (Hugging Face, GitHub releases, `.gguf` URLs), the
  MCP server on `127.0.0.1`, and — only when the user opts in — a frontier
  provider's API.
- **macOS services**: Apple Vision face detection, the Keychain (frontier API
  key), and the camera, microphone, and Screen Recording permissions used by
  capture.

## System Verification

- Run: `cargo test -p roughcut-core` (from the workspace root).
- Changes to exported types also need the bindings regenerated:
  `TS_RS_EXPORT_DIR="$(pwd)/frontend/src/ipc/generated" cargo test -p roughcut-core --features ts-bindings export_bindings`.
