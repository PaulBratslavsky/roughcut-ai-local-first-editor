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

## Existing design artifacts

| Link | Date | Author | Components | Description |
| ---- | ---- | ------ | ---------- | ----------- |
| [CONTEXT.md](../../CONTEXT.md) | 2026-06-16 | Paul Bratslavsky | all | Domain glossary: Project, Timeline/Clip, EditOp, SavedClip, ClipCandidate, Beat, Caller, adapters. The shared vocabulary for designs and reviews. |
| [07-architecture.md](../07-architecture.md) | 2026-06-10 | Paul Bratslavsky | core, app, frontend, mcp-shim | Architecture and implementation guide: one tool registry with three callers, adapters, AI layer, MCP, persistence, the Tauri shell, the frontend. |
| [tool-api.md](../tool-api.md) | 2026-06-10 | Paul Bratslavsky | core/tools, frontend/ipc | The tool contract every caller (UI, local agent, MCP clients) uses. Kept in sync with the registry by `core/tests/doc_parity.rs`. |
| [01-product.md](../01-product.md) | 2026-06-10 | Paul Bratslavsky | product | Mission and product: a local-first, zero-egress AI video editor. |
| [02-users.md](../02-users.md) | 2026-06-10 | Paul Bratslavsky | product | Target users and how they work. |
| [03-requirements.md](../03-requirements.md) | 2026-06-10 | Paul Bratslavsky | product | Functional requirements. |
| [04-tech-decisions.md](../04-tech-decisions.md) | 2026-06-10 | Paul Bratslavsky | all | Stack decisions: Tauri, Rust core, React, whisper.cpp, local LLMs. |
| [05-tech-requirements.md](../05-tech-requirements.md) | 2026-06-10 | Paul Bratslavsky | core | Data model and technical requirements. |
| [06-build-spec.md](../06-build-spec.md) | 2026-06-10 | Paul Bratslavsky | all | The original milestone build spec. |
| [08-recording-plan.md](../08-recording-plan.md) | 2026-06-11 | Paul Bratslavsky | core/adapters/record, frontend/recorder | Camera + screen capture, v1 plan. |
| [ADR-0001](../adr/0001-hand-rolled-mcp-jsonrpc.md) | 2026-06-10 | Paul Bratslavsky | core/mcp | Hand-rolled MCP JSON-RPC over axum instead of `rmcp`. |
| [ADR-0002](../adr/0002-snapshot-inverses-in-the-edit-journal.md) | 2026-06-10 | Paul Bratslavsky | core/engine | Undo/redo stores snapshot inverses, not analytic inverses. |
| [ADR-0003](../adr/0003-generated-frontend-types.md) | 2026-06-10 | Paul Bratslavsky | core/model, frontend/ipc | Frontend model types are generated from Rust (ts-rs). |
| [ADR-0004](../adr/0004-provisioning-outside-the-tool-registry.md) | 2026-06-10 | Paul Bratslavsky | core/tools, app | Model/toolchain provisioning stays outside the tool registry. |
| [ADR-0005](../adr/0005-online-editor-architecture.md) | 2026-10-03 | Paul Bratslavsky | all | Proposed: the path to an online (browser-hosted) editor. |
| [architecture-audit.md](../architecture-audit.md) | 2026-09-30 | Paul Bratslavsky | all | Full architecture audit across every tier. |
| [refactor-checklist.md](../refactor-checklist.md) | 2026-06-16 | Paul Bratslavsky | core | Deepening `core/src`: the refactor checklist and its status. |
| [export-validation.md](../export-validation.md) | 2026-06-10 | Paul Bratslavsky | core/export | NLE export validation against real editors. |
| [zero-egress-verification.md](../zero-egress-verification.md) | 2026-06-10 | Paul Bratslavsky | all | How the zero-egress guarantee is verified. |

Features built before EGM without a design document of their own: **Clips**
(saved clips, the clip finder, vertical export, face-framed crop) — see
`SavedClip` / `ClipCandidate` in `CONTEXT.md` and the Clips section of
`tool-api.md`.
