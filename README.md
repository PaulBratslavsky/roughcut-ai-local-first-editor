# RoughCut *(working name)*

**An open-source, local-first, AI-powered video editor.** It turns raw
talking-head footage into a clean rough cut **entirely on your machine** — no
upload, no subscription, no account. Transcribe on-device, let a local LLM
remove silences / filler words / bad takes, refine by editing the transcript
or chatting with your footage, drag clip boundaries frame-by-frame, then
export to Premiere, Final Cut, or DaVinci Resolve.

Uniquely, the editor exposes its tools over **MCP**, so external clients like
**Claude Desktop** can drive it — and the same registry powers the local agent
loop and the UI. One tool set, three callers.

> Mission: prove that a paid cloud AI service can be replaced by owned,
> local-first software. See [`docs/01-product.md`](docs/01-product.md).

## Status

Working skeleton through the build plan's M1–M6 (see
[`docs/06-build-spec.md`](docs/06-build-spec.md)):

- ✅ Rust core: project/timeline model (non-destructive, frame-snapped),
  tool registry, undo/redo, SQLite persistence
- ✅ Ingest (ffprobe) + on-device transcription — in-process whisper.cpp
  (whisper-rs, GPU/Metal where available) with real progress events; the
  `whisper-cli` adapter remains as a fallback for core-only builds
- ✅ First-run setup: whisper model download with progress (large-v3-turbo
  quantized by default), ffmpeg detection across PATH/Homebrew, and an honest
  demo-mode banner when running on fixtures
- ✅ AI first pass: silence / filler / repeated-take detection, one-click
  rough cut with cut count
- ✅ Transcript-based editing, trim handles, split, global padding
- ✅ Local agent loop (Gemma via Ollama / llama-server, OpenAI-compatible),
  with a deterministic offline fallback when no model server is running
- ✅ Export: Premiere/Resolve XML (xmeml), Final Cut (fcpxml), EDL, OTIO,
  SRT, MP4 (ffmpeg)
- ✅ MCP server on a localhost endpoint (per-install auth token) + the
  `roughcut-mcp-shim` stdio proxy for Claude Desktop
- ✅ React + TanStack frontend (canvas timeline, transcript panel, chat,
  padding inspector) in the Gling-style layout
- ⬜ M0 WebView spike on Windows/Linux, WebCodecs preview, waveform peaks
  from real audio, bundled llama.cpp sidecar, bundled ffmpeg sidecar,
  signed bundles (M7)

**Demo mode:** without `ffmpeg`/`whisper-cli` on PATH (or with `FABLE_DEMO=1`)
the adapters switch to fixture footage so the entire edit loop — rough cut,
transcript editing, chat, undo, export — is drivable with nothing installed.

## Layout

```
core/        Rust core crate — the portable asset (models, tools, agent, MCP, export)
app/         Tauri v2 shell (src-tauri) wiring core ↔ frontend
frontend/    React + TanStack UI (Vite); runs standalone in a browser with a mock API
mcp-shim/    stdio ⇄ localhost proxy that Claude Desktop spawns
docs/        the six planning documents + the tool API contract + ADRs (docs/adr/)
```

New to the codebase? Start with
[`docs/07-architecture.md`](docs/07-architecture.md) — a full walkthrough of
how every feature works and where it lives in the code.

## Architecture notes

- **Edits are data.** Every mutation is an `EditOp` applied through one Editor
  entry point; `EditAction`s carry the op plus exact inverse/redo snapshots,
  and the per-project journal persists — undo survives restarts (ADR-0002).
- **One tool table.** Each tool is a single `ToolSpec` row in
  `core/src/tools.rs`; schemas, the agent subset, and dispatch all derive from it.
- **Generated frontend types.** The TS model types in
  `frontend/src/ipc/generated/` are exported from the Rust structs (ADR-0003):
  `TS_RS_EXPORT_DIR="$(pwd)/frontend/src/ipc/generated" cargo test -p roughcut-core --features ts-bindings export_bindings`
- **Capabilities resolved at call time** (`core/src/capabilities.rs`): install
  ffmpeg or download a whisper model mid-session and the adapters pick it up
  on the next operation — no restart.

## Build & run

Prereqs: Rust (1.85+), Node 20+, and `cmake` for the app build (it compiles
whisper.cpp in-process via the core's `whisper-native` feature). Optional but
recommended: `ffmpeg`/`ffprobe` (PATH or Homebrew — the app finds either), and
[Ollama](https://ollama.com) with a Gemma model (`ollama pull gemma4:26b`) for
conversational editing. The whisper speech model is downloaded from the
first-run setup screen — no `whisper-cli` needed.

```sh
# Tests (core: unit + end-to-end through the tool registry + MCP over HTTP)
cargo test -p roughcut-core

# Frontend alone, in a browser with fixture data
cd frontend && npm install && npm run dev

# The desktop app
cd frontend && npm install && cd ..
cargo install tauri-cli --version '^2' --locked   # once
cargo tauri dev      # from app/src-tauri, or: npx @tauri-apps/cli dev

# Live agent-loop smoke test against your local model server
cargo run -p roughcut-core --example agent_live
```

## Claude Desktop (MCP)

1. Run the app once (it writes `<data dir>/roughcut/mcp.json` with the
   localhost endpoint + per-install token; macOS:
   `~/Library/Application Support/roughcut/mcp.json`).
2. Build the shim: `cargo build --release -p roughcut-mcp-shim`.
3. Add to Claude Desktop's config:

```json
{
  "mcpServers": {
    "roughcut": { "command": "/path/to/target/release/roughcut-mcp-shim" }
  }
}
```

Claude Desktop can then list projects, read the transcript, cut by transcript,
apply padding, and export — through the exact tool registry the UI uses. All
externally-driven edits are tagged `mcp_client` and are undoable.

## Configuration (no secrets in the repo)

| Env var | Default | Purpose |
|---|---|---|
| `FABLE_DEMO` | auto | `1` forces fixture adapters |
| `INFERENCE_ENDPOINT` | `http://localhost:11434/v1` | OpenAI-compatible local server (Ollama / llama-server) |
| `INFERENCE_MODEL` | `gemma4:26b` | model tag for the agent loop |
| `WHISPER_MODEL` | auto (best model in `<data dir>/roughcut/models/`) | override the whisper model path |
| `FFMPEG_PATH` / `FFPROBE_PATH` | auto (PATH, Homebrew, `<data dir>/bin`) | override the ffmpeg/ffprobe binaries |
| `FRONTIER_MODEL` | per provider | model for the opt-in frontier path |

Optional frontier escalation (`connect_external` / `escalate_to_frontier`) is
**opt-in only**, uses your own API key stored in the OS keychain, and is never
auto-invoked. Core editing makes zero network calls; verify with any network
monitor.

## Principles (non-negotiable)

1. Local-first / zero egress by default — full function with networking disabled.
2. Non-destructive editing — source media is never modified; everything is undoable.
3. The Rust core owns all state — the frontend is a view layer.
4. One tool registry, three callers — local LLM, MCP clients, UI.
5. Frame-exact time internally, seconds at the API edge.
6. No accounts, no secrets in the repo.

## License

[GPL-3.0](LICENSE). Dependency licensing notes (FFmpeg build flavor, Gemma
weight terms) are tracked in [`docs/04-tech-decisions.md`](docs/04-tech-decisions.md).
Model weights are downloaded by the user at runtime, never redistributed here.
