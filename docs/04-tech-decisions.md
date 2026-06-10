# Tech Decisions

For each decision: what was chosen, what else was considered, and why — traced back to a requirement (Stage 3) or the mission/users (Stages 1-2).

> **Note on process:** the tech exploration for this project was done up front (it's how the project started). These decisions synthesize that research. The architecture went through one deliberate pivot: an initial Mac-native (SwiftUI) plan was replaced by **one Tauri + web codebase shipped to all platforms** once "one codebase, ship everywhere" became the priority. The constant throughout: a **portable Rust core** holding all the expensive logic, with platform-specific pieces (video, model runtime, transcription) swappable behind traits.

## Overall architecture — Hybrid: portable Rust core + Tauri/web UI + swappable adapters
- **Choice:** A **portable Rust core** holds everything that's expensive to rewrite (agent loop, tool schemas, MCP client+server, project/timeline model, inference client). A **single Tauri v2 + React/TanStack web UI** runs on all three platforms. Platform-specific work (video, inference runtime, transcription) sits behind **Rust traits** as swappable adapters.
- **Considered:** (1) Pure native Swift (AVFoundation + MLX-Swift + WhisperKit) — best Mac app, but 60-80% throwaway on port and rewritten UI per platform. (2) **SwiftUI Mac shell now + native shells later** — rejected in favor of one shared web UI (see UI shell decision) once the direction became "one codebase, ship everywhere." (3) C++/Qt — proven for NLEs but no first-party MCP SDK and weakest AI/agent tooling. (4) Odin — not viable.
- **Why:** Satisfies "one codebase, ship to Mac/Windows/Linux" while keeping the expensive logic in a portable Rust core. Tauri *is* Rust, so the core links directly into the app with no FFI bridge. The same Rust tool definitions are driven by the local orchestrator, external MCP clients, and the UI — the Stage 1 "open editing engine" identity. The concentrated risk is the web-based timeline/preview on three different system WebViews (addressed in the UI shell decision).

## UI shell (the "frontend")
- **Choice:** **Tauri v2 + a React/TanStack web frontend**, one codebase shipped to Mac, Windows, and Linux. The frontend renders all panels (transcript/script, chat, inspector, padding, export, settings) as ordinary web UI. The **timeline and video preview** — the one performance-critical surface — are rendered on a **canvas (WebGL/WebGPU, e.g., via PixiJS or equivalent)**, not DOM elements, with **virtualization** (TanStack Virtual) for long projects and **WebCodecs** for frame-accurate preview/seeking.
- **Considered & rejected:** (1) **SwiftUI native Mac shell + native per-platform shells** — best Mac feel and best timeline performance, but the entire UI is rewritten per platform; rejected because the mission favors one codebase, more contributors (React >> SwiftUI in the open-source pool), and shipping to all platforms sooner. (2) **Hybrid: web chrome + native timeline surface** — kept as a *fallback* if the canvas timeline hits a wall on long-form 4K (see risk below), not the starting point.
- **Why:** Matches the decided direction — "one codebase, ship everywhere." Pairs more cleanly with the Rust core than SwiftUI did: **Tauri is itself Rust**, so the frontend talks to the core via Tauri commands/events/channels — **no UniFFI bridge needed** (this removes the earlier UniFFI pre-1.0 async risk entirely). Easier for a community to maintain and contribute to.
- **TanStack usage:** **TanStack Virtual** for timeline + transcript virtualization (essential for long footage); **TanStack Query** for calling/caching Rust-core state; **TanStack Router** for app navigation; **TanStack Store** for view state.
- **KEY RISK (validate with an early spike):** Tauri uses the *system* WebView per OS — **WebView2/Chromium (Windows), WKWebView (macOS), WebKitGTK (Linux)** — and **WebCodecs/WebGPU support has been uneven across these three.** This cross-platform WebView capability gap is now the project's biggest technical risk (bigger than the framework choice itself). Mitigations: (a) spike the canvas timeline + WebCodecs preview on all three WebViews *before* building out features; (b) fall back to piping decoded frames from the Rust/FFmpeg side to the canvas if WebCodecs is missing/weak in a given WebView; (c) keep the timeline renderer behind an interface so a native surface can replace it on a platform where the WebView can't keep up (the hybrid fallback). WKWebView's ~60fps rAF cap and canvas-perf limits are real but generally acceptable for a 2D timeline; the preview-decode path is the riskier piece.

## Core / "backend" logic
- **Choice:** **Rust**, linked directly into the Tauri application. Houses the agentic tool-calling loop, the tool schema definitions, the project/timeline model, undo/redo, and the inference + MCP layers.
- **Considered:** writing core logic in TypeScript in the frontend (loses the MCP/`rmcp` integration, performance, and the clean "core owns state" boundary), or C++ (weaker MCP/agent ecosystem).
- **Why:** Rust gives memory safety for buffer-heavy video work, a production-grade MCP SDK, and a single implementation of the tool registry shared by the local orchestrator, MCP clients, and the UI — on every platform.

## Frontend ↔ Rust core boundary
- **Choice:** **Tauri v2 IPC** — `#[tauri::command]` functions for request/response, **events** for core→UI push (transcription progress, agent-loop steps, timeline changes), and **channels** for streaming. For **large binary data** (video frames, waveform peaks, thumbnails) use Tauri's **custom/asset protocol** or a local stream rather than JSON IPC, which is too slow for per-frame data.
- **Considered & dropped:** **UniFFI** (Rust↔Swift) — no longer needed now that the frontend is web/Tauri rather than SwiftUI. This **removes the earlier UniFFI pre-1.0 async/`Sendable` risk** entirely.
- **Why:** Tauri's native Rust integration means the core links directly into the Tauri app; the web frontend calls it over IPC. The only care needed is keeping large binary transfers off the JSON IPC path.

## Local LLM inference (the orchestrator)
- **Choice:** **Gemma 4** run via a bundled **llama.cpp `llama-server`** (with **Ollama** as an alternative/fallback), exposed over an **OpenAI-compatible HTTP API** that the Rust core calls. Model size tiers to hardware (E4B for ~16GB machines, 26B-A4B MoE as the sweet spot for ~32GB, 31B for high-end).
- **Considered:** **MLX-Swift** (fastest on Apple Silicon) — rejected as the primary path because it's Apple-only and would be 100% throwaway on port, *and* its Gemma 4 tool-call parser was still unfit as of mid-2026. Direct in-process bindings — more coupling, less portability.
- **Why:** llama.cpp/Ollama run identically on Mac (Metal), Windows/Linux (CUDA/Vulkan/ROCm), giving portable inference + portable tool-calling — directly serving the cross-platform constraint and the local-first requirement (runs offline). The HTTP boundary also makes the local model and an optional remote frontier model interchangeable behind one interface.
- **Landmines to pin (from research):** Gemma 4 tool-calling needs specific runtime versions/templates (Ollama ≥ the fix release; llama.cpp `llama-server` with `--jinja` + correct chat template + direct GGUF path); pin exact versions and budget integration time.

## Video engine
- **Choice:** **FFmpeg** as the portable backbone, accessed from Rust via **`ffmpeg-the-third`** (the actively maintained fork). Hardware accel selected per platform through FFmpeg: VideoToolbox (Mac), NVENC/QuickSync (Windows), VAAPI/Vulkan (Linux).
- **Considered:** **AVFoundation** — best Mac video framework but Mac-only (throwaway on port); **MLT** — higher-level cross-platform editing engine (powers Shotcut/Kdenlive), a strong option if we want more NLE features sooner; `ffmpeg-next` — maintenance-only, so the fork is preferred.
- **Why:** FFmpeg works everywhere with one consistent HW-accel interface, satisfying the cross-platform constraint. *Note:* FFmpeg's API is low-level/unsafe — budget a safe Rust wrapper layer, or adopt MLT later if the feature set grows.

## Transcription
- **Choice:** **whisper.cpp**, on-device, everywhere (Core ML/ANE acceleration on Mac, CUDA/Vulkan elsewhere).
- **Considered:** **WhisperKit** (Mac/ANE-only, throwaway on port); cloud ASR (violates local-first).
- **Why:** Portable, runs fully offline, multilingual — serves the on-device transcription and multi-language requirements (Stage 3) and survives the port.

## MCP (server + client)
- **Choice:** **`rmcp`** (the official Rust MCP SDK) in the core, implementing **both** roles: a **server** exposing editing tools to Claude Desktop / other clients, and an optional **client** to reach out to a frontier model.
- **Considered:** TypeScript/Python SDKs (most mature, but would split the core out of Rust); Swift MCP SDK (Mac-only).
- **Why:** Keeps the MCP layer in the portable core (one implementation, ships everywhere) and uses the same tool definitions the local orchestrator uses — exactly the Stage 1 "open editing engine" identity. *Note:* a Mac MCP client that spawns subprocess servers may require disabling the App Sandbox, which affects Mac App Store distribution — fine here since distribution is via direct download / open-source releases.

## Timeline interchange / export
- **Choice:** **OpenTimelineIO (OTIO)** for portable timeline interchange, plus direct **XML/EDL** export targets for Premiere / Final Cut / DaVinci Resolve; **SRT** for captions; **FFmpeg** for direct **MP4** render.
- **Why:** Serves the NLE hand-off requirement (Stage 3) with a portable, industry-standard core.

## Local persistence ("database")
- **Choice:** **Local files** for projects (a project file referencing the source media + the non-destructive edit state), plus a lightweight embedded store (e.g., SQLite or simple serialized files) for preferences and project library. All on-device.
- **Considered:** a cloud/synced DB — rejected outright (violates local-first/mission).
- **Why:** No accounts, no cloud, everything the user owns sits on their disk (Stage 3 data + local-first guarantees).

## Auth
- **Choice:** **None.** No accounts, no login. The only credentials in the system are an optional user-supplied API key for a frontier service (if they opt into the MCP client path), stored locally.
- **Why:** Directly from Stage 3 — a local, single-user, owned tool needs no identity system.

## Distribution & deployment
- **Choice:** Direct download of signed/notarized app bundles (.dmg on Mac; later .exe/MSI on Windows, AppImage/Flatpak/deb on Linux), plus source + releases on a public repo. First run downloads the model assets sized to the machine.
- **Considered:** App stores — possible later, but MCP subprocess/sandbox constraints and the open-source ethos favor direct distribution first.
- **Why:** Fits open-source norms and avoids store sandbox limits on the MCP/local-model features.

## CI/CD
- **Choice:** **GitHub Actions** (or equivalent) building the Rust core + the Tauri app for Mac, Windows, and Linux from the single codebase, running tests, and producing signed release artifacts per platform.
- **Why:** Standard for open-source; the build matrix covers all three platforms from day one.

## Licensing — DECIDED: GPL-3.0
- **Choice:** **GPL-3.0** (copyleft).
- **Why:** Matches the mission's "must stay free and open forever" stance — anyone who builds on or distributes a derivative must also keep it open under GPL. This prevents the exact failure mode the project is reacting against: someone taking the local-first work, closing it, and reselling it as a proprietary or cloud service. Chosen over permissive (Apache-2.0) deliberately: the goal is to protect and propagate an open commons, not to maximize unrestricted (including closed/commercial) reuse.
- **Dependency compatibility (verify during setup):**
  - **FFmpeg** — builds as LGPL by default, but enabling certain codecs/components (e.g., GPL-licensed parts like x264/x265) makes the FFmpeg build GPL. GPL-3.0 for our app is compatible with a GPL FFmpeg build; just ensure the combination is GPL-consistent and document which FFmpeg build is bundled. (LGPL FFmpeg is also fine under GPL-3.0.)
  - **whisper.cpp** — MIT; compatible.
  - **llama.cpp** — MIT; compatible. **Ollama** — MIT; compatible.
  - **Rust crates** (`rmcp`, `ffmpeg-the-third`, Tauri, etc.) — predominantly MIT/Apache-2.0; compatible with GPL-3.0 (permissive → GPL is fine).
  - **Gemma 4 model weights** — governed by Google's Gemma terms (the research noted Apache-2.0 for Gemma 4; **verify the exact model license** at integration time). Model *weights* are downloaded by the user at first run, not redistributed in the repo, which keeps the licensing clean — but confirm the Gemma terms permit your distribution/use pattern.
  - **Qt is NOT in the stack** (we chose Tauri + React + Rust), so Qt's licensing is not a concern. Tauri, React, TanStack, and PixiJS are all MIT/Apache — compatible.
- **Practical note:** the main thing to watch is that *all bundled native components* are GPL-compatible and that you document the bundled FFmpeg build. Pure permissive deps (MIT/Apache) flowing into a GPL-3.0 project are fine; the reverse (GPL deps in a permissive project) would not be — but that's not our situation.
- **AGPL consideration (parked):** if you ever offer a *hosted* version (contrary to the current mission), AGPL-3.0 would close the "SaaS loophole." Since hosting is explicitly out of scope, plain GPL-3.0 is the right pick now. Revisit only if the hosting stance changes.

---

### Open assumptions to confirm (Stage 4)
1. **License — DECIDED: GPL-3.0.** Copyleft, to guarantee the project and derivatives stay open. Verify the bundled FFmpeg build is GPL-consistent and confirm Gemma 4's model-weight terms at integration time (see Licensing section).
2. **llama.cpp as primary, Ollama as fallback** — agree, or prefer Ollama primary (simpler model management) at some performance/control cost?
3. **UI: DECIDED — Tauri v2 + React/TanStack, one codebase for all platforms.** The deferred "native shells vs shared UI" question is resolved in favor of one shared web UI. The remaining work is to **de-risk the timeline/preview early** (canvas rendering + WebCodecs across the three system WebViews) — this is now the top technical risk and should be the first spike in the build plan.
4. **MLT vs raw FFmpeg** — start with raw FFmpeg (lighter, less NLE feature surface) and adopt MLT only if features grow? Or start on MLT for a richer engine sooner?
5. **SQLite vs flat files** for persistence — either is fine; any preference?
