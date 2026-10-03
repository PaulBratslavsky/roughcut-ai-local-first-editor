# ADR-0005: Path to an online (browser-hosted) editor

**Status:** proposed (2026-10)

## Context

RoughCut is a Tauri desktop app built on "local-first, zero egress, no
account" (docs/01-product.md). We are evaluating an online version that runs
in a browser with no install. That conflicts with the mission, so this ADR
records both the technical path and the product trade-off before any code is
written.

What already transfers:

- **One IPC seam.** Every UI→core call goes through `callTool(name, args)`
  and `onAppEvent` in `frontend/src/ipc/api.ts`, which already branch between
  Tauri and the browser mock. An HTTP branch is a third, symmetric case.
- **The core owns all state.** The frontend is a pure view; it doesn't care
  where the core runs.
- **Adapters are traits.** `VideoEngine`, `Transcriber`, `InferenceClient`,
  and `Store` (`core/src/adapters/`, `core/src/store.rs`) each take a cloud
  implementation without touching the engine or tool registry.
- **Inference speaks the OpenAI protocol.** Ollama → hosted model is a config
  change.
- **The MCP server is already HTTP + Bearer** (ADR-0001). Going remote means
  replacing the localhost bind and per-install token with real auth.

What doesn't transfer:

- **Single-user, single-process state.** One `Editor` holds every project in
  memory with one event sink; there is no notion of a user.
- **Local file paths everywhere.** Media is referenced by path and played back
  via `convertFileSrc`; exports write to `out_path` on disk.
- **Heavy compute runs in-process.** whisper.cpp (Metal), ffmpeg renders, and
  embeddings run on the user's machine.
- **~30 bespoke shell commands** outside the registry (ADR-0004): recording,
  PTY, Resolve integration, model/toolchain provisioning, `reveal_path`,
  privacy settings.

## Decision

If we build an online editor, it will be a **new server shell around the
unchanged `roughcut-core`**, not a fork. The desktop app stays the primary
product. The tool registry remains the single API: the web UI calls the same
tools over HTTP that it calls over Tauri IPC today.

Target shape:

```
Browser (React SPA) ──HTTPS/WS──► API server (axum + roughcut-core) ──► Postgres
        │                               │                           ──► Redis (jobs + events)
        └──direct upload/stream────► Object storage + CDN ◄── GPU workers (whisper, ffmpeg, embeddings)
                                                              LLM: hosted API or vLLM
```

| Concern | Desktop (today) | Online |
|---|---|---|
| Transport | Tauri `invoke` + events | `POST /tools/:name`; WebSocket/SSE for `CoreEvent`s |
| Identity | none | Auth + per-user/per-project authorization in front of dispatch; `Caller` gains an identity |
| Project store | `SqliteStore` | `PostgresStore` behind the same `Store` trait |
| Media | local paths, `convertFileSrc` | Object storage. Presigned direct uploads; proxy (low-res H.264/HLS), waveforms and thumbnails generated at ingest |
| Transcribe / render / embed | in-process | Job queue + GPU workers. Progress via Redis pub/sub → the user's socket |
| LLM | Ollama on localhost | Hosted API or self-hosted vLLM via `OpenAiCompatClient` |
| Exports | file in `~/Downloads` | Object in the bucket + signed URL. MP4 is a job |
| MCP | localhost + stdio shim | Remote MCP (streamable HTTP) with OAuth |
| Recording | native capture | `getDisplayMedia` + `MediaRecorder`, uploaded as media |
| PTY, Resolve, provisioning, `reveal_path` | shell commands | Dropped from the web build |

Deployment:

- **Frontend:** static Vite build on a CDN (e.g. Vercel).
- **API server:** long-running container (Fly.io, Railway, Cloud Run, ECS).
  It holds live editor sessions, WebSockets and ffmpeg calls, which suits a
  container better than short-lived functions.
- **GPU workers:** scale-to-zero pool (Modal, RunPod, or AWS GPU instances).
- **Data:** managed Postgres, S3/R2 for media and exports, Redis for jobs and
  events.

Phasing, each step shippable on its own:

1. **`server/` crate:** axum wrapper around `roughcut-core` exposing the
   registry over HTTP + WebSocket, plus the HTTP branch in `callTool` /
   `onAppEvent`. Single user, self-hosted, local disk. Cheap, and proves the
   seam.
2. **Storage:** `PostgresStore`; a media-storage adapter (new trait) for
   object storage; ingest pipeline for proxies and waveforms.
3. **Jobs:** move transcribe / render / embed behind a queue; workers emit
   `CoreEvent`s over pub/sub.
4. **Multi-tenancy:** auth, users, project ownership, per-project editor
   sessions; remote MCP with OAuth.

## Alternatives considered

- **In-browser compute** (whisper.cpp WASM/WebGPU, ffmpeg.wasm, File System
  Access API). Keeps zero egress and costs almost no GPU spend, but browser
  API support is uneven and WebView/canvas performance is already the
  project's top technical risk (docs/07-architecture.md, "Known gaps").
  Worth a spike before committing to step 3.
- **Desktop + optional cloud sync.** Sync project JSON only; media stays
  local. Preserves the mission and is far cheaper, but needs an install, so
  it doesn't meet the "no install" goal.
- **Fork the core for the web.** Rejected: the single tool registry is the
  architecture's main invariant; two cores would drift.

## Consequences

- The online edition gives up "zero egress" and "no account". It must be
  positioned as a separate edition, and the desktop app's guarantees
  (docs/zero-egress-verification.md) must not regress.
- Running cost is dominated by GPU minutes and video bandwidth; pricing must
  be modelled before step 3.
- `Editor` must become safe for many concurrent users. The likely shape is
  one session/actor per open project rather than one global in-memory
  editor.
- The registry's trust model (ADR-0004) carries over: anything exposed over
  HTTP is reachable by remote MCP clients, so authorization lives at
  dispatch, not in the UI.
- Revisit this ADR after the step-1 prototype and the in-browser compute
  spike.
