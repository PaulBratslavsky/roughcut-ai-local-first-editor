# Architecture audit — 2026-09

A full pass over every tier: `core/`, the Tauri shell (`app/src-tauri`), the
MCP shim, the Resolve plugin, and the frontend. It builds on
[`refactor-checklist.md`](refactor-checklist.md) (items 1–7 cover `core/src`
depth) and uses the vocabulary in [`CONTEXT.md`](../CONTEXT.md) and the
deepening glossary (module, interface, seam, adapter, depth, leverage,
locality).

**Legend.** ✔ = re-verified against the code while writing this;
unmarked = reported by the audit with file:line evidence, not independently
re-run. Line numbers are as of commit `961b767`.

Baseline: `cargo test -p roughcut-core` — 39 tests green (23 unit, 14 e2e,
1 doc-parity, 1 mock-parity). The frontend has no tests.

---

## 1. Recommended fix — caller policy has no single home ✔

All three audit passes (core, shell, frontend) independently landed here.

The registry's **interface** is "one tool set, three callers", but the rules
for *which caller may do what* are smeared across five places:

| Rule | Where it lives today |
|---|---|
| What MCP clients see | `tools::mcp_defs()` filters `meta` — **listing only** |
| What the agent sees | `tools::agent_defs()` filters `agent` — **listing only** |
| What the agent may call | `dispatch_basic` filters `meta` (not `agent`) |
| What MCP / UI may call | `dispatch` — **everything** |
| Schema validation | `dispatch_basic` only (UI + MCP unchecked) |
| Confirmation | inline `if s == McpClient` in two handlers |

Each caller has to pick the right `*_defs` + `dispatch*` pair, and the MCP
server picked the wrong one (`mcp/server.rs:128` → `dispatch`). Consequences:

- **MCP clients can call the hidden meta tools** (`connect_external`,
  `escalate_to_frontier`, `apply_instruction`) by name. ADR-0004's escape
  hatch ("add it as a meta tool, hidden from MCP") doesn't actually hide.
- **The agent loop can call any non-meta tool** — including `export`,
  `delete_project`, `set_preferences`, `import_media`, none of which it is
  shown — and its unknown-tool error advertises all of them. Confirmation is
  keyed on `McpClient`, so a local-model `export` writes without asking.
  Transcript text (whatever was said on camera) is model input, so this is
  reachable by prompt injection, not just hallucination.
- **`set_preferences` can silently break zero-egress.** It is MCP-visible,
  unconfirmed, and replaces `inference_endpoint` with no loopback check; the
  automatic post-transcribe cleanup and embedding index then send transcript
  text to whatever host it names.
- **`set_preferences` is broken from the UI.** It deserializes a *full*
  `Preferences` (`tools.rs:929`); the Inspector sends a partial
  (`InspectorPanel.tsx:308, 316`), so choosing "Aggressive" or saving custom
  filler words fails with a serde error nobody renders. The browser mock
  merges partials (`mockApi.ts:539`), which hid it.

**Direction:** a `Caller` (Ui | Mcp | Agent) passed to one `dispatch` and one
`defs`, deciding visibility, validation and attribution from the `ToolSpec`
row — so listing and dispatch cannot disagree. `set_preferences` becomes a
merge-patch, and only the user can point inference off-machine. Tracked as
checklist item 8.

---

## 2. Correctness bugs

### High

| # | Finding | Evidence |
|---|---|---|
| B1 | ✔ `trim_clip` can **panic** (`f64::clamp` with min > max) when adjacent included clips total < 50 ms (reachable at ≥ ~41 fps). It runs under the global `state` lock → mutex poisoned → every later `.lock().unwrap()` panics, for every project, until restart. | `model.rs:280`, `engine/edits.rs:45` |
| B2 | ✔ `llm_outline` **panics** on the weak-model overlap it says it handles: `b.last.clamp(first, chunk_end-1)` with `first == chunk_end`. | `story.rs:187-188` |
| B3 | ✔ `reorder_clip` is a **journaled no-op**: `normalize()` re-sorts by `source_in` and resets `order`. The mock really reorders. | `edits.rs:672-684`, `model.rs:167, 192` |
| B4 | `append_media` / `import_media` (existing project) change `duration` outside `apply_edit`; every older undo snapshot and checkpoint stops at the old duration, so undo strands the appended span. | `engine/media.rs:26, 176-188` |
| B5 | `rough_cut_peaks` is one global slot any `apply_edit` consumes — concurrent rough cuts on two projects refine against each other's audio. | `engine/mod.rs:72`, `edits.rs:74` |
| B6 | In-memory state and disk can diverge: three separate autocommit writes after mutating memory; a failed journal write leaves an undo entry that restores two edits back. | `edits.rs:94-98`, `store.rs` |
| B7 | ✔ Schema drift wipes data: `load_preferences` falls back to defaults on any decode error (adding a field resets every user's prefs, then persists); history decode does the same. | `store.rs:218`, `engine/mod.rs:196` |
| B8 | MCP shim uses reqwest's 30 s default timeout and **re-sends** the same `tools/call` up to 5× — long `transcribe` / `export` run concurrently, share one temp WAV, and stack confirmation dialogs. | `mcp-shim/src/main.rs:73, 121-132` |
| F1 | Switching tabs kills the in-app terminal's shell (and the chat history; a second agent run can start while the first is still going). | `App.tsx:125-138`, `TerminalPanel.tsx:83-89`, `ChatPanel.tsx:173` |

### Medium

| # | Finding | Evidence |
|---|---|---|
| B9 | EDL timecodes are wrong at NTSC rates (29.97 rounded to 30 → ~2.4 s off at 40 min). No `FrameRate` home: four writers derive the rate four ways; `time.rs`'s `RationalTime` has no production caller. | `export/edl.rs:7, 18` |
| B10 | Clips aren't reliably frame-snapped (`CutSegments`, `RoughCut`, padding write raw times); FCPXML/OTIO round start and duration separately → 1-frame gaps. | `edits.rs:645-714`, `fcpxml.rs:29-30` |
| B11 | Export paths not escaped (`Q&A.mp4` → invalid FCPXML; `#`/`%` break `file://`). | `fcpxml.rs:23`, `xmeml.rs:17` |
| B12 | Embedding index goes stale after re-transcribe; `plan_duration_cut` then scores every segment 0.5 and cuts from the start. | `semantic.rs:34-37, 159` |
| B13 | Filler/take flags only ever accumulate; LLM cleanup updates `segment.text` but not `words` (token counts differ), so the two disagree. | `detect.rs:138-156`, `media.rs:126` |
| B14 | `undo_actions` checks and acts under separate locks (a concurrent edit can be undone). | `edits.rs:314-334` |
| B15 | llama-server outlives a crash on a fixed port; next launch's health check passes against the orphan. | `llm_runtime.rs:17, 262, 271-285` |
| B16 | `pty_write` is a sync Tauri command (main thread) doing a blocking write under the global sessions lock — a >1 KB paste into a busy shell freezes the UI. | `lib.rs:259-262`, `pty.rs:109-114` |
| B17 | `cargo test` overwrites the live `mcp.json` (e2e starts the real server; `data_dir()` can't be overridden). | `tests/e2e.rs:140`, `mcp/server.rs:60-64` |
| B18 | Recording races: stop-during-pause drops the last take; concurrent starts orphan an ffmpeg holding the camera. **Resolved: the recorder was removed (ADR-0008).** | `record.rs:464-508, 677-688` |
| F2 | Errors render as `[object Object]` on desktop: Tauri rejects `{error:{code,message}}`, the mock `{code,message}`; 11 sites unwrap the wrong one. | `HistoryPanel.tsx:34` et al. |
| F3 | Panel-local copies of core state survive a project switch (`<Editor>` not keyed) — Apply writes project A's padding into B. | `App.tsx:232`, `InspectorPanel.tsx:273-280` |
| F4 | Transcript "segment is cut" is inferred from `linked_segment_ids`, which are provenance, not current state — wrong after partial restores. | `TranscriptPanel.tsx:251-257` |
| F5 | "Screen only" recording opens the webcam anyway. **Resolved: the recorder was removed (ADR-0008).** | `RecorderPanel.tsx:339` |
| F6 | `set_audio_offset`, `set_preferences`, `attach_screen_media` emit no event → MCP-driven changes never reach the UI; re-transcribe leaves a stale suggestions banner. | `projects.rs:185-218`, `queries.ts:109-114` |

### Packaging / security posture

- `csp: null` + asset scope `**` + all 32 app commands open to the webview: no
  active injection vector today (no HTML sinks, no remote resources), but any
  script in the page is a shell. Set a real CSP, scope assets per imported
  file, restrict commands with an `AppManifest`. (`tauri.conf.json:24-32`)
- No `Info.plist` camera/mic usage strings or entitlements — a signed build
  likely can't record. (`app/src-tauri`) **Resolved: the recorder was removed
  (ADR-0008), so the app needs no camera or mic access.**
- `install_llama_server` runs an unpinned, unverified `releases/latest`
  binary. (`llm_runtime.rs:127-157`)
- `mcp.json` is 0644, non-atomic, never removed; the token never rotates and
  is injected into every in-app shell's environment. (`server.rs:31-64`,
  `pty.rs:70-73`)

---

## 3. Depth findings (not bugs — friction)

Corrections to existing checklist items are applied in
`refactor-checklist.md`; new candidates:

1. **Typed tool client (frontend).** `callTool(name: string, args:
   Record<string, unknown>)` carries none of the contract; a Rust rename fails
   at runtime. Generate `ToolName`/args/result types; type the mock table so
   parity is compile-checked in both directions.
2. **The mock is a second implementation, not an adapter.** ~450 of 1204
   lines re-implement edit semantics and have drifted ~10 ways (and
   `jump_to`, `undo_actions`, `accept_*` are wrong in the mock only).
   ADR-0003's "WASM timeline" next step, or shared golden vectors.
3. **Host commands seam.** 31 bespoke Tauri commands (recording, PTY,
   Resolve, permissions; the recording and permission ones were removed in
   ADR-0008) outside any written trust category; each costs a Rust + TS
   wrapper + mock stub. `isTauri` appears 45× outside `api.ts`.
4. **Inference has no injection point** (`Editor::inference()` always builds
   a live client), so the 330-line online agent loop and every LLM path have
   0 tests. The LLM-or-heuristic fork is copied 7×; `llm::ask_json` is dead.
5. **"Is this segment still in the cut?" has four answers** (SRT midpoint,
   flow >50%, planner ≥0.2 s, agent context ignores inclusion). One
   `Timeline::coverage` policy.
6. **Child-process lifecycle has no owner** — PTY, recording, llama-server
   are three globals the shell must remember to stop (recording was removed
   in ADR-0008); ffmpeg renders aren't tracked at all. A `Supervisor`.
7. **Per-project persisted state has no single owner** — tables + `journal:`
   / `outline:` kv + file cache; `Store` has no unit of work.
8. **Events aren't generated** — `CoreEvent` payloads are hand-typed in TS;
   the shell emits a raw `"mcp-ready"` past the "one choke point".

## 4. Test gaps

- `engine/` (the mutation funnel): 0 unit tests.
- No test covers: history surviving restart, checkpoints, `reorder_clip`,
  `append_media`, any online LLM path, any export writer's frame accuracy,
  meta tools over MCP, the shim, the shell.
- `mock_parity.rs` only checks that each tool *name* appears in
  `mockApi.ts`; its "semantics are covered by the Playwright flows" comment is
  not backed by anything in the repo.
- CI never regenerates the TS bindings, so ADR-0003's "forgetting fails the
  build" doesn't hold. Add `export_bindings` + `git diff --exit-code`.

## 5. Doc drift

- `CONTEXT.md`: Transcript and Media are **not** immutable
  (`media.rs:112-139, 187`); clips are **not** reliably frame-snapped (B10).
- `07-architecture.md`: "4 IPC commands" (32), "five events" (10), "api.ts is
  the only file that knows about Tauri" (6 others), `types.ts` hand-mirrors
  models (superseded by ADR-0003).
- ADR-0001: "the shim test does the same over stdio" — no such test exists.
