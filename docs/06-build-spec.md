# Build Spec — Local-First AI Video Editor

> **This is the handoff document for Claude Code.** It is self-contained: you should be able to build from this file alone. It synthesizes the product, users, requirements, and tech decisions from stages 1–5. Where it says "see Stage N," that's optional background, not required reading.

---

## 1. What we're building (one paragraph)

An **open-source (GPL-3.0), local-first, AI-powered video editor** for YouTubers and creators who want to own their data. It turns raw talking-head footage into a clean rough cut **entirely on-device** — no upload, no subscription, no account. A local LLM (Gemma 4) transcribes the footage, removes silences/filler/bad takes, and lets the user refine by editing the transcript, chatting, dragging clip boundaries, and applying global padding. It exports to Premiere/Final Cut/DaVinci Resolve. Uniquely, it exposes its editing tools over **MCP**, so external clients like **Claude Desktop** can drive it — and it can *optionally* reach out to a frontier model via MCP when the user chooses (local by default, frontier by choice). The mission is bigger than one app: prove that paid cloud AI services can be replaced by owned, local software.

## 2. Stack (decided — see Stage 4)

| Layer | Choice |
|-------|--------|
| Core logic | **Rust** (portable; owns all state) |
| App shell | **Tauri v2** |
| Frontend | **React + TanStack** (Query, Router, Virtual, Store) |
| Timeline/preview render | **Canvas: WebGL/WebGPU** (e.g. PixiJS) + **WebCodecs**, fallback = frames piped from Rust/FFmpeg |
| Local LLM | **Gemma 4** via bundled **llama.cpp `llama-server`** (Ollama fallback), OpenAI-compatible API |
| Transcription | **whisper.cpp** (on-device) |
| Video engine | **FFmpeg** via **`ffmpeg-the-third`** (Rust) |
| MCP | **`rmcp`** — server (expose tools) + client (optional frontier escalation) |
| Timeline interchange | **OpenTimelineIO** + XML/EDL/SRT writers |
| Persistence | **SQLite + local files** (no cloud, no account) |
| Frontend↔core | **Tauri IPC** (commands/events/channels); binary over asset/stream protocol |
| License | **GPL-3.0** (verify bundled FFmpeg build is GPL-consistent; confirm Gemma 4 weight terms) |
| Platforms | **Mac, Windows, Linux** from one codebase |

## 3. Architecture & principles

```
Tauri v2 app (Mac / Windows / Linux — one codebase)
├── Frontend (React + TanStack)
│   ├── Transcript/Script panel (TanStack Virtual)
│   ├── Chat panel (agent-loop steps + diffs)
│   ├── Video preview (WebCodecs / piped frames)
│   ├── Timeline (Canvas WebGL/WebGPU, virtualized)
│   ├── Inspector / Padding panel
│   └── Export control + First-run setup
│        │  Tauri IPC (commands/events/channels; binary via asset protocol)
├── Rust core  ◄── the portable asset; survives every platform
│   ├── Tool registry  ◄── KEYSTONE: one tool set, 3 callers (local LLM / MCP client / UI)
│   ├── Agent loop (drives Gemma 4)
│   ├── Project / Timeline model (non-destructive, frame-exact)
│   ├── Undo/redo (EditAction stack)
│   └── MCP server + client (rmcp)
└── Adapters (behind Rust traits — swappable per platform)
    ├── VideoEngine      → FFmpeg (VideoToolbox / NVENC / QuickSync / VAAPI)
    ├── InferenceClient  → llama.cpp / Ollama (Metal / CUDA / Vulkan / ROCm)
    ├── Transcriber      → whisper.cpp
    └── Store            → SQLite / files
```

**Non-negotiable principles:**
1. **Local-first / zero egress by default.** No network calls during core editing. Any outbound (MCP frontier) is explicit, user-initiated, opt-in. Full function with networking disabled.
2. **Non-destructive editing.** Source media is never modified. Cuts are an editable layer; everything is undoable.
3. **The Rust core owns all state.** The frontend is a view layer. This is what keeps the app portable.
4. **One tool registry, three callers.** The local orchestrator, external MCP clients, and the UI all call the same core functions.
5. **Frame-exact internal time** (rational, OTIO-style), surfaced as seconds at the API edge.
6. **No accounts, no secrets in the repo.** Optional external API keys live in the OS keychain.

## 4. Repository structure (proposed)

```
/                       GPL-3.0 LICENSE, README, CONTRIBUTING
/core/                  Rust core crate (the portable asset)
  /src/
    project/            Project, Timeline, Clip, Transcript models
    tools/              tool registry (the shared command surface)
    agent/              agent loop + Gemma 4 orchestration
    mcp/                rmcp server + client
    adapters/
      video/            VideoEngine trait + FFmpeg impl
      inference/        InferenceClient trait + llama.cpp/Ollama impl
      transcribe/       Transcriber trait + whisper.cpp impl
      store/            Store trait + SQLite/file impl
    time/               frame-exact rational time
/app/                   Tauri v2 application (wires core + frontend)
  /src-tauri/           Tauri Rust side; registers commands/events; links core
/mcp-shim/              tiny stdio⇄local-endpoint proxy binary (spawned by Claude Desktop)
/frontend/              React + TanStack
  /src/
    panels/             transcript, chat, preview, inspector, export
    timeline/           Canvas (WebGL/WebGPU) renderer + interaction
    ipc/                Tauri command/event wrappers (TanStack Query)
    state/              TanStack Store view state
/models/                (gitignored) downloaded model assets at runtime
/docs/                  the six planning files + tool API docs
```

## 5. Data model (see Stage 5 for full fields)

Core entities: **Project** (name, source media, timeline, settings) · **Media** (path, duration, fps, dims) · **Transcript** (segments → words, each with start/end/confidence; flags: is_filler, is_silence, take_group_id, is_best_take) · **Timeline** (ordered clips + global_padding) · **Clip** (source_in/out, included bool, order, origin, linked_segment_ids) · **ProjectSettings/Preferences** (padding defaults, aggressiveness, filler stoplist, export target, language, model_tier) · **EditAction** (type, inverse payload, source: ui/local_ai/mcp_client — for undo + audit) · **ExternalConnection** (optional MCP-client provider + keychain ref).

Implement time as frame-exact rational internally. Cuts set `included=false` or adjust `source_in/out` — never delete source.

## 6. The tool registry (the core API — see Stage 5 for I/O detail)

Defined once in Rust; surfaced to LLMs as JSON-schema tools, to the UI as commands, to MCP as server tools.

- **Ingest/transcribe:** `import_media`, `transcribe`
- **Analysis (AI first pass):** `detect_silences`, `detect_fillers`, `detect_takes`, `generate_rough_cut`
- **Editing:** `cut_range`, `restore_range`, `cut_by_transcript`, `trim_clip`, `split_clip`, `reorder_clip`, `set_global_padding`
- **Semantic/LLM:** `find_segments(query)`, `apply_instruction(instruction)`
- **Metadata:** `generate_chapters` (must), `generate_title_description` (should), `generate_captions`
- **Export:** `export(target ∈ {premiere_xml, fcp_xml, resolve_xml, edl, mp4, srt})`
- **Project/state:** `create/open/save/list_project`, `undo`, `redo`, `get_timeline`, `get/set_preferences`
- **MCP client (opt-in):** `connect_external`, `escalate_to_frontier` (never auto-invoked)

Agent loop: instruction + project context + tool schemas → Gemma 4 → tool call(s) → execute (record EditActions) → loop until done → UI shows diff, all undoable. Constrain tool output with JSON-schema/GBNF; cap iterations; surface each step in chat.

## 7. Build plan (milestones)

### M0 — De-risk the WebView (DO THIS FIRST)
The biggest risk is web-based timeline/preview across three system WebViews (WebView2/Chromium, WKWebView, WebKitGTK). Build a throwaway spike:
- A Tauri v2 app on **all three platforms**.
- A **canvas timeline** (WebGL/WebGPU) drawing a thumbnail filmstrip + waveform + playhead + draggable handle over a long (e.g., 45-min 4K) clip, scrolling/zooming smoothly.
- A **frame-accurate preview** via WebCodecs; if WebCodecs is missing/weak on a WebView, prove the **Rust/FFmpeg frame-pipe fallback** over Tauri's asset protocol.
- **Exit criteria:** smooth scrub + accurate seek on long 4K on Mac, Windows, and Linux. If a WebView can't, adopt the native-timeline-surface fallback for that platform before proceeding.

### M1 — Core skeleton + ingest + transcription
- Rust core crate; trait definitions (VideoEngine, InferenceClient, Transcriber, Store).
- `import_media` (FFmpeg probe) + `transcribe` (whisper.cpp) with progress events.
- SQLite store; create/open/save project.
- Frontend: file import, transcript panel rendering time-linked text (TanStack Virtual).

### M2 — AI first-pass rough cut
- `detect_silences`, `detect_fillers`, `detect_takes`, `generate_rough_cut`.
- Non-destructive Clip/Timeline model + undo/redo.
- Frontend: cut counter, show-cuts/skip-cuts, basic timeline render from M0.

### M3 — Manual refinement
- `cut_by_transcript`, `trim_clip` (drag handles), `split_clip`, `set_global_padding`.
- Padding inspector (Start/End sliders + link + apply), aggressiveness, filler stoplist.
- Preferences persistence.

### M4 — Local agent loop (chat editing)
- Bundle/manage llama.cpp `llama-server` (Gemma 4); model-tier-to-hardware; pin runtime + chat template (`--jinja`).
- Agent loop + `find_segments` + `apply_instruction`; chat panel with step/diff display.

### M5 — Export + metadata
- `export` to premiere_xml/fcp_xml/resolve_xml/edl (OTIO), mp4 (FFmpeg), srt.
- `generate_chapters` (must), `generate_title_description` (should), `generate_captions`.

### M6 — MCP server (+ optional client)
- `rmcp` server exposing the tool registry on a **localhost MCP endpoint** secured by a **per-install auth token**.
- A tiny **stdio shim binary** (Claude Desktop spawns it; it proxies stdio ⇄ the running app's endpoint) — verify **Claude Desktop can drive the editor** end-to-end.
- Optional client path: `connect_external` (keychain), `escalate_to_frontier`, with explicit opt-in + confirmation on destructive external-driven ops.

### M7 — Packaging & cross-platform
- First-run setup (hardware detect, model download, offline confirmation).
- Signed/notarized bundles: .dmg, then Windows (.msi/.exe) and Linux (AppImage/Flatpak/deb).
- GitHub Actions build matrix; document the bundled GPL-consistent FFmpeg build.

## 8. POC / MVP acceptance criteria

**Proof-of-concept (prove the core thesis):**
- [ ] Import a local 4K video; transcribe it on-device with **no network calls**.
- [ ] One-click rough cut removes silences + fillers; shows a cut count.
- [ ] Edit by deleting transcript text → video cuts to match.
- [ ] Drag a clip boundary to adjust a cut, frame-accurately.
- [ ] Apply global padding (e.g., 0.15s start/end) to all clips.
- [ ] Type a chat instruction ("cut the part where I ramble about X") → local Gemma 4 finds + cuts it → change is shown and undoable.
- [ ] Export an XML that imports cleanly into a real NLE.
- [ ] Verify (e.g., network monitor) that nothing left the machine.

**MVP (shippable v1):**
- [ ] All POC criteria, polished, on **Mac, Windows, and Linux**.
- [ ] Bad-take detection working (target; may slip to v1.1 — see Stage 3).
- [ ] Chapter generation; title/description (should-have).
- [ ] MP4 + SRT export in addition to NLE interchange.
- [ ] MCP server verified: **Claude Desktop can run a full edit** through the exposed tools.
- [ ] First-run setup downloads models sized to hardware; degrades gracefully on low-RAM machines.
- [ ] Faster than manual rough-cutting (qualitative bar; set concrete numbers during M2/M4).
- [ ] GPL-3.0 license in repo; FFmpeg build documented; Gemma terms confirmed.

## 9. Out of scope for MVP (see Stage 3)
Full NLE features (color, motion graphics, audio mixing); Shorts/clip generation (v2); multicam; AI b-roll/background-replacement/auto-reframe; cloud collaboration; mobile apps; AI voice/dubbing; any hosted version of the app (contrary to mission).

## 10. Top risks & mitigations
1. **WebView inconsistency (WebCodecs/WebGPU across WKWebView / WebView2 / WebKitGTK)** — highest risk. *Mitigation:* M0 spike before features; Rust/FFmpeg frame-pipe fallback; timeline renderer behind an interface so a native surface can replace it per-platform.
2. **Gemma 4 tool-calling fragility** — needs specific runtime versions/templates. *Mitigation:* pin versions, `--jinja` + correct template + direct GGUF path; constrain output with JSON-schema/GBNF; test the exact (system-prompt + tools) combo.
3. **On-device performance on low-end machines** — *Mitigation:* model tiers (E4B → 26B MoE → 31B); offer a fast non-LLM cut mode; publish min-spec guidance.
4. **FFmpeg licensing** — *Mitigation:* document the bundled GPL-consistent build; user downloads model weights at runtime (not redistributed).
5. **IPC throughput for binary data** — *Mitigation:* asset/stream protocol for frames/waveforms/thumbnails, never JSON IPC.
6. **Local MCP endpoint abuse** (any local process could drive the editor) — *Mitigation:* localhost-only bind + per-install auth token; confirmation prompts on destructive externally-driven ops.
7. **Bad-take detection difficulty** (Gling's hardest feature) — *Mitigation:* treat as a target; acceptable to slip to v1.1.

---

*Companion files in `/docs`: 01-product, 02-users, 03-requirements, 04-tech-decisions, 05-tech-requirements, plus the Gling UI reference image. This build spec is the entry point.*
