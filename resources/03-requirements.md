# Functional Requirements

Requirements are capability statements — what the product must *do*, not how it's built. Tech choices are deferred to Stage 4. Each requirement traces back to the product (Stage 1) and the user journey / core loop (Stage 2).

## Core features (MVP)

### Ingest & transcription
- The app must let a user import a local video file (common formats: MP4, MOV, MKV, AVI) by drag-and-drop or file picker.
- The app must transcribe the video's audio to time-aligned text **entirely on-device**, with no network upload.
- The app must support multiple spoken languages (not English-only), within the limits of the local transcription model.
- The app must produce a transcript where each word/segment is linked to its timestamp in the video.

### AI first-pass rough cut
- The app must automatically detect and remove **silences / dead air**.
- The app must automatically detect and remove **filler words** ("um," "uh," etc.).
- The app must automatically detect **repeated/bad takes** and keep the best one, removing the others.
- The app must show how many cuts were made (a cut count).
- The app must let the user control cut **aggressiveness** (e.g., a "natural" vs "aggressive" sensitivity, and a custom filler-word list).
- All AI cut decisions must be **non-destructive** — the original footage is never modified; cuts are an editable layer over it.

### Transcript-based editing
- The user must be able to edit the cut by **editing the transcript text**: deleting words/sentences removes the corresponding video, and the mapping is bidirectional (selecting text highlights the video range and vice versa).
- The user must be able to **restore** removed sections (undo a cut) and toggle a "show cuts / skip cuts" view of the transcript.

### Timeline & clip control
- The app must display a **timeline** with a thumbnail filmstrip, audio waveform, and a playhead.
- Each cut must produce **clips with adjustable start/end handles** the user can drag to fine-tune where a cut lands, with frame-level precision.
- The app must support **splitting** a clip at the playhead.
- The app must provide **global padding**: add or remove a buffer of time (e.g., 0.15s) to the start and end of *all* talking clips at once, with start and end adjustable independently or linked together, applied on demand.
- The app must provide **playback** with variable speed (e.g., 1x/2x) and a preview that reflects the current cut.

### Conversational / agentic editing (local)
- The user must be able to issue **natural-language editing instructions** ("cut the tangent about X," "tighten the intro," "remove the section where I fumble the demo") that a **local model** interprets and executes against the editing tools.
- The local model must act as an **orchestrator** that calls the app's editing tools (transcribe, find segments, cut, pad, split, etc.) in a loop to fulfill a request.
- Conversational edits must be **reviewable and reversible** — the user sees what changed and can undo it (consistent with non-destructive editing).

### AI assists (metadata)
- The app must be able to generate **chapters** from the transcript.
- The app should be able to generate **title and description suggestions** from the content. *(Should-have for MVP; can be trimmed if needed.)*

### Export / hand-off
- The app must **export to professional NLEs** via interchange formats (XML for Final Cut, XML/EDL for Premiere and DaVinci Resolve) so the rough cut continues in the user's finishing editor.
- The app must be able to **export captions/subtitles** (e.g., SRT).
- The app must be able to **export a rendered video file** (e.g., MP4) of the current cut.

### MCP — open & extensible
- The app must act as an **MCP server**, exposing its editing tools (transcribe, cut, pad, split, find-segments, generate-chapters, export, etc.) so external MCP clients — notably **Claude Desktop** — can drive the editor.
- The app must act as an **MCP client**, so the user can **optionally** connect out to a frontier model (e.g., Claude) or external tools, bringing their local data/context to that model on demand.
- The frontier/cloud path must always be **opt-in and explicit** — never required for core editing, never invoked without the user's deliberate choice. Local-by-default, frontier-by-choice.
- The MCP tool surface must be the **same set of tools** the app's own local orchestrator uses (one tool definition, driven by either the local model or an external client).

### Local-first guarantees
- The app must perform all core editing (ingest, transcription, cutting, refinement, export) **fully offline**, with no account and no network connection required.
- The app must make **no network requests** during core editing unless the user explicitly invokes the optional MCP/frontier path.
- First-run setup may download model(s) once; after that the app must function with networking disabled.

### Preferences & projects
- The app must **remember user preferences** across sessions (default padding, cut aggressiveness, export target, language).
- The app must let the user **save and reopen projects** (the editing state over a source video), so work persists between sessions. All project data is stored locally.

## Account & auth
- **No account is required** to use the app. There is no sign-up, no login, no cloud identity.
- If the user opts into a frontier-model path via MCP, they may need to supply their **own credentials/API key** for that external service; these are stored locally and used only for the user-initiated connection.

## Data the product handles
- **Source video/audio files** — the user's raw footage (local only).
- **Transcript** — time-aligned text derived from the audio.
- **Project / edit state** — the non-destructive set of cuts, clip boundaries, padding, splits, and metadata over a source file.
- **Cut/segment objects** — individual clips with start/end times, included/excluded status, source references.
- **Generated metadata** — chapters, title/description suggestions, captions.
- **User preferences** — defaults for padding, aggressiveness, language, export target.
- **Model assets** — the locally downloaded transcription and LLM model files.
- **(Optional) external credentials** — API keys for a user-chosen frontier service, stored locally.

## Integrations
- **Local transcription** — speech-to-text running on-device.
- **Local LLM** — an on-device model acting as the editing orchestrator.
- **NLE interchange** — produce files Premiere / Final Cut / DaVinci Resolve can import (XML/EDL), plus SRT and MP4.
- **MCP (server)** — expose editing tools to external MCP clients (Claude Desktop, others).
- **MCP (client, optional)** — connect out to a frontier model / external tools at the user's choice.

## Non-functional requirements
- **Privacy:** zero footage egress by default; verifiable (open source). Any network activity must be user-initiated and transparent.
- **Performance:** on-device processing of a typical recording must be fast enough to feel like a time-saver vs manual editing; the app should scale model size to the user's hardware (smaller models on lower-spec machines). Concrete targets to be set in Stage 5, but the bar is "noticeably faster than rough-cutting by hand."
- **Hardware:** must run on consumer hardware; degrade gracefully on lower-RAM machines (smaller models, or a faster non-LLM cut mode) rather than failing.
- **Offline:** full core functionality with networking disabled.
- **Transparency / trust:** because the privacy claim is the core value, the app should make it easy to confirm no data leaves the device (open source; optionally, visible network status).
- **Extensibility:** the MCP tool surface should be stable and documented enough for third parties to build on.
- **Cross-platform:** Mac, Windows, and Linux from **one codebase**; development and first release lead on Mac, but the architecture targets all three from day one. Platform-specific pieces (video hardware-accel, model runtime backends, transcription accel) stay swappable behind interfaces. *(Detailed in Stage 4.)*
- **Licensing:** open source (specific license chosen in Stage 4).

## Out of scope for MVP
- **Full NLE features** — color grading, motion graphics, advanced audio mixing, multi-track compositing. The app hands off to a real editor; it is not one.
- **Short-form / "viral clip" generation** — auto-generating Shorts/Reels from long-form. *Assumed deferred to v2* — flag if you want this in MVP, as it expands scope meaningfully.
- **Multicam editing** — deferred.
- **AI b-roll insertion, background replacement, auto-reframe/zoom** — deferred (these are Gling extras, not core to the private rough-cut job).
- **Cloud collaboration / multi-user projects** — out of scope; this is a local, single-user tool.
- **Mobile apps (iOS/Android)** — out of scope.
- **AI voice / dubbing / overdub** — out of scope.
- **Hosted/cloud version of the app itself** — contrary to the mission; out of scope.

---

### Assumptions — CONFIRMED (user approved)
1. **Shorts/clip generation deferred to v2.**
2. **Chapters must-have; title/description should-have.**
3. **MP4 render included in MVP** alongside NLE interchange.
4. **Bad-take detection in MVP as a target** — may slip to v1.1 if it proves too hard to match early.
5. **"Faster than manual" as the qualitative performance bar**, with concrete numbers set during the build (M2/M4).
