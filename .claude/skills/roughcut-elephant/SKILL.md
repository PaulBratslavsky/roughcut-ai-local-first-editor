---
name: roughcut-elephant
description: Load RoughCut's project context — what it is, where the design docs and key code live, and the invariants a change must keep. Use before designing, drafting, or reviewing a RoughCut feature with EGM (egm-brainstorm, egm-draft, egm-review, egm-drift-report), and before any non-trivial change to this codebase.
---

# RoughCut project context

RoughCut is a local-first, AI-powered video editor (Tauri v2 desktop app):
talking-head footage in, a clean rough cut out, transcribed and edited on the
user's machine, handed off to Premiere / Final Cut / Resolve or rendered to MP4.
Zero egress by default; no accounts.

## Read first

- `CONTEXT.md` — the domain glossary. Use its terms exactly (e.g. a timeline
  `Clip` is NOT a `SavedClip`).
- `docs/07-architecture.md` — the big picture and every subsystem.
- `docs/designs/SITEMAP.md` — every design artifact and ADR, with what it covers.
- `docs/tool-api.md` — the tool contract all callers share.

## Where the code is

- `core/` — `roughcut-core`, the Rust crate that owns all state.
  - `src/tools.rs` — the tool registry (one row per tool) and `Caller` policy.
  - `src/engine/` — the `Editor`: projects, edits + undo journal, media, semantic
    search, saved clips.
  - `src/model.rs` — the data model (ts-rs bindings behind `ts-bindings`).
  - `src/adapters/` — swappable seams: video (ffmpeg), transcribe (whisper),
    inference (OpenAI-compatible), embed, record, face (Apple Vision).
  - `src/agent.rs` (chat agent loop), `src/story.rs` (outline/flow),
    `src/clip_finder.rs` (short-form candidates), `src/export/` (NLE writers).
  - `tests/` — `e2e.rs` (through the registry), `doc_parity.rs`, `mock_parity.rs`.
- `app/src-tauri/` — the Tauri shell: `call_tool` + bespoke shell commands.
- `frontend/` — React UI. `src/ipc/api.ts` is the single IPC seam (Tauri or the
  in-browser mock `mockApi.ts`); `src/ipc/generated/` holds the generated types;
  `src/panels/` the UI; `src/styles.css` the one stylesheet.
- `mcp-shim/` — stdio ↔ HTTP proxy so Claude Desktop/Code can drive the app.

## Invariants a design must keep

1. One tool registry, three callers (UI, local agent, MCP). New capability = a
   registry row; who may call it is decided by `Caller`, nowhere else.
2. The core owns all state; the frontend is a view that reads via tools.
3. Non-destructive editing; frame-exact time inside, seconds at the API edge.
4. Zero egress by default — only user-triggered downloads leave the machine.
5. Model changes regenerate the frontend types:
   `TS_RS_EXPORT_DIR="$(pwd)/frontend/src/ipc/generated" cargo test -p roughcut-core --features ts-bindings export_bindings`
6. Every registry tool is documented in `docs/tool-api.md` and implemented in
   the browser mock — the parity tests fail otherwise.
7. UI follows the settled design language: monospace, square, monochrome,
   terminal-style; `styles.css` is one sheet built on the `:root` tokens.

## Verify a change

- `cargo test -p roughcut-core` (unit + e2e + parity guards)
- `cd frontend && npx tsc -b && npm run build`
- `cargo tauri dev` from `app/src-tauri` for the live app;
  `npm --prefix frontend run dev` for the browser build on the mock.
- `scripts/build-macos-app.sh` for a signed release `RoughCut.app`.
