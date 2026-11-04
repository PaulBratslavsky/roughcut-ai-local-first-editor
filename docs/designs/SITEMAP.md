# Design sitemap

The index of RoughCut's design artifacts, for the Elephant-Goldfish Model
(EGM) workflow. New designs from `/egm:egm-draft` are saved in this directory
(`docs/designs/<FEATURE>.md`) and registered in the first table. Designs and
decisions written before EGM was set up live elsewhere under `docs/` and are
indexed in the second table, so a new design can build on them instead of
repeating them.

Start with [`CONTEXT.md`](../../CONTEXT.md) (the domain glossary) and
[`07-architecture.md`](../07-architecture.md) (how the pieces fit together).

## EGM designs

| Link | Date | Author | Components | Description |
| ---- | ---- | ------ | ---------- | ----------- |
| [bulk-shorts.md](bulk-shorts.md) | 2026-10-30 | Paul Bratslavsky | core/clip_finder, core/engine/clips, core/export, frontend/shorts, frontend/clips | Phase 1 implemented (Phases 2–3 to do): scan the whole video for shorts, review them as cards on a Shorts screen, make and export them in a batch; pieces of a clip in any play order; multi-moment shorts by condensing a stretch. |
| [hosted-models.md](hosted-models.md) | 2026-10-31 | Paul Bratslavsky | core/adapters, core/engine, core/tools, app/src-tauri, frontend/setup, frontend/topbar | Implemented (live check pending): a Local / Hosted switch for every AI step; Hosted uses Claude's Messages API with the user's key in the OS key store, one model per run, visible errors instead of silent fallbacks, and only the user can turn it on; removes the hidden escalate_to_frontier path. |
| [logging.md](logging.md) | 2026-11-03 | Paul Bratslavsky | core/applog, core/llm, core/engine/route, core/story, core/clip_finder, core/agent, core/adapters/video, core/engine/media, app, frontend/setup | Implemented on `feat/logging` and checked in the app: a small local app log — one JSON line per AI step (model that answered, time, tokens, fallback and why), Hosted → Local switch, ffmpeg/ffprobe failure (stderr tail) and transcription start/end/failure; typed events plus one scrubber keep transcript text, prompts, replies and keys out; 5 MB × 3 rotation; never leaves the Mac. |
| [remove-recording.md](remove-recording.md) | 2026-11-03 | Paul Bratslavsky | core/adapters/record, core/adapters/video, core/engine/media, app, frontend/recorder, frontend/footage, frontend/preview | Implemented and merged into `feat/shorts` (c6d6010): remove in-app recording; record in OBS as one file and import it; OBS `.mkv` files get a playable copy for the preview; adding footage no longer needs the recorder's folder; old dual-capture projects keep working. |
| [shorts-finder-review](../reviews/2026-11-03-shorts-finder-review.md) | 2026-11-03 | Paul Bratslavsky | core/clip_finder | Implemented on `feat/shorts-sentences` (not merged): shorts are cut on whole sentences (whisper's segments re-cut), the model picks each short's first and last sentence topic by topic with a four-part rubric, and heuristic slices of one idea count as one moment. Review with evidence, then as built and measured. |

## Existing design artifacts

| Link | Date | Author | Components | Description |
| ---- | ---- | ------ | ---------- | ----------- |
| [CONTEXT.md](../../CONTEXT.md) | 2026-06-16 | Paul Bratslavsky | all | Domain glossary: Project, Timeline/Clip, EditOp, SavedClip, ClipCandidate, Shorts screen, Beat, Caller, adapters, ModelMode. The shared vocabulary for designs and reviews. |
| [07-architecture.md](../07-architecture.md) | 2026-11-03 | Paul Bratslavsky | core, app, frontend, mcp-shim | Architecture and implementation guide: one tool registry with three callers, adapters, AI layer, MCP, persistence, the Tauri shell, the frontend. Rewritten to match `feat/shorts` (bulk shorts, Local / Hosted, OBS import, the in-app terminal). |
| [tool-api.md](../tool-api.md) | 2026-06-10 | Paul Bratslavsky | core/tools, frontend/ipc | The tool contract every caller (UI, local agent, MCP clients) uses. Kept in sync with the registry by `core/tests/doc_parity.rs`. |
| [01-product.md](../01-product.md) | 2026-06-10 | Paul Bratslavsky | product | Mission and product: a local-first, zero-egress AI video editor. |
| [02-users.md](../02-users.md) | 2026-06-10 | Paul Bratslavsky | product | Target users and how they work. |
| [03-requirements.md](../03-requirements.md) | 2026-06-10 | Paul Bratslavsky | product | Functional requirements. |
| [04-tech-decisions.md](../04-tech-decisions.md) | 2026-06-10 | Paul Bratslavsky | all | Stack decisions: Tauri, Rust core, React, whisper.cpp, local LLMs. |
| [05-tech-requirements.md](../05-tech-requirements.md) | 2026-06-10 | Paul Bratslavsky | core | Data model and technical requirements. |
| [06-build-spec.md](../06-build-spec.md) | 2026-06-10 | Paul Bratslavsky | all | The original milestone build spec. |
| [08-recording-plan.md](../08-recording-plan.md) | 2026-06-11 | Paul Bratslavsky | core/adapters/record, frontend/recorder | Superseded by ADR-0008. Camera + screen capture, v1 plan. |
| [ADR-0001](../adr/0001-hand-rolled-mcp-jsonrpc.md) | 2026-06-10 | Paul Bratslavsky | core/mcp | Hand-rolled MCP JSON-RPC over axum instead of `rmcp`. |
| [ADR-0002](../adr/0002-snapshot-inverses-in-the-edit-journal.md) | 2026-06-10 | Paul Bratslavsky | core/engine | Undo/redo stores snapshot inverses, not analytic inverses. |
| [ADR-0003](../adr/0003-generated-frontend-types.md) | 2026-06-10 | Paul Bratslavsky | core/model, frontend/ipc | Frontend model types are generated from Rust (ts-rs). |
| [ADR-0004](../adr/0004-provisioning-outside-the-tool-registry.md) | 2026-06-10 | Paul Bratslavsky | core/tools, app | Model/toolchain provisioning stays outside the tool registry. |
| [ADR-0005](../adr/0005-online-editor-architecture.md) | 2026-10-03 | Paul Bratslavsky | all | Proposed: the path to an online (browser-hosted) editor. |
| [ADR-0007](../adr/0007-hosted-model-mode.md) | 2026-10-31 | Paul Bratslavsky | core/engine/route, core/adapters/anthropic, core/secrets, app | Hosted model mode: the user can switch the AI steps to Claude with their own key; only the user enables egress, in the app; one model per run; the hidden frontier tools removed. |
| [ADR-0008](../adr/0008-remove-in-app-recording.md) | 2026-11-03 | Paul Bratslavsky | core/adapters/video, core/engine/media, app, frontend | In-app recording removed: record in OBS as one file and import it; non-MP4 files get a playable copy for the preview; adding footage joins into the data dir; old camera-bubble projects keep working. |
| [architecture-audit.md](../architecture-audit.md) | 2026-09-30 | Paul Bratslavsky | all | Full architecture audit across every tier. |
| [refactor-checklist.md](../refactor-checklist.md) | 2026-06-16 | Paul Bratslavsky | core | Deepening `core/src`: the refactor checklist and its status. |
| [export-validation.md](../export-validation.md) | 2026-06-10 | Paul Bratslavsky | core/export | NLE export validation against real editors. |
| [zero-egress-verification.md](../zero-egress-verification.md) | 2026-06-10 | Paul Bratslavsky | all | How the zero-egress guarantee is verified. |

There is no ADR-0006 here: that number was taken by the unmerged
`feat/native-capture` branch (see ADR-0008).

Features built before EGM without a design document of their own: **Clips**
(saved clips, the clip finder, vertical export, face-framed crop) — see
`SavedClip` / `ClipCandidate` in `CONTEXT.md` and the Clips section of
`tool-api.md`. Also without one: the **in-app terminal** (the editor's
Terminal tab and the Shorts screen's terminal drawer, one shared shell) and
**open over MCP** (an MCP client's `open_project` switches the app, via
`project-opened`) — see [`07-architecture.md`](../07-architecture.md).
