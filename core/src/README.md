# src (`roughcut-core`)

## Overview

The source of `roughcut-core`, the Rust crate that owns all of RoughCut's state:
the project and timeline model, the tool registry every caller goes through,
the AI layer (chat agent loop, story outline and flow review, clip finder,
deterministic detection), persistence, and the event stream the UI listens to.
Platform- and binary-dependent work sits behind the traits in
[`adapters`](./adapters/README.md); the stateful `Editor` that implements every
operation lives in [`engine`](./engine/README.md).

`lib.rs` declares 24 public modules and re-exports `Editor`
(`pub use engine::Editor`) and the error types (`pub use error::{CoreError,
Result}`). The crate is consumed by the Tauri shell (`app/src-tauri`), the MCP
server ([`mcp`](./mcp/README.md)), the tests in `core/tests/`, and the
examples in [`core/examples/`](../examples/README.md).

## Interface

### Child packages

- [adapters](./adapters/README.md) — the boundary to everything outside the
  process: `VideoEngine` (ffmpeg/ffprobe), `Transcriber` (whisper),
  `InferenceClient` (OpenAI-compatible chat), `Embedder`, and Apple Vision
  face finding. Video and transcription have production impls, `Mock*`
  fixtures, and `Auto*` selectors that re-check `capabilities::probe()` per
  call.
- [engine](./engine/README.md) — `Editor`: the in-memory project cache, the
  journaled edit path (`apply_edit`, undo/redo, checkpoints), media and
  transcription, saved clips, semantic search, and UI confirmations.
- [export](./export/README.md) — NLE interchange writers (xmeml, FCPXML, EDL,
  OTIO), SRT, plain-text transcript, and MP4 via the video adapter;
  `Editor::export` / `Editor::export_clip`.
- [mcp](./mcp/README.md) — the localhost MCP server (JSON-RPC over axum,
  bearer token).

### Modules in this directory

| Module | Role | Key public items |
| --- | --- | --- |
| `lib.rs` | Crate root | 25 `pub mod`s; `pub use engine::Editor`; `pub use error::{CoreError, Result}` |
| `model.rs` | The data model (serde; `ts_rs::TS` exports behind feature `ts-bindings`) | `Project`, `Media`, `Transcript`, `TranscriptSegment`, `Word`, `Timeline`, `Clip`, `ClipOrigin`, `Padding`, `EditOp`, `EditAction`, `ActionSource`, `Preferences`, `Aggressiveness`, `ModelTier`, `ProjectSummary`, `SavedClip`, `SavedClipSummary`, `ClipCandidate`, `ClipFormat`, `SourceRange`, `PlaySpan`, `ClipBatchError`, `ClipExportResult`, `pieces_overlap`, `Checkpoint`, `CheckpointSummary`, `CutSuggestion`, `Layout`, `History`, `HistoryEntry`, `ModelMode`, `ModelStatus`, `RouteReason`, `KeyCheck`, `HostedKeyResult`, `HOSTED_MODELS`, `hosted_model_name`, `EPS` |
| `tools.rs` | The tool registry: one `ToolSpec` row per tool, three callers | `ToolSpec`, `ToolDef`, `Caller`, `defs`, `dispatch`, `registry`, `is_mutating`, `Handler`, `HandlerFuture` |
| `agent.rs` | The chat agent loop and LLM-with-fallback helpers | `run_instruction`, `run_instruction_with`, `InstructionOutcome`, `HistoryTurn`, `clean_transcript`, `generate_chapters`, `Chapter`, `generate_title_description` |
| `story.rs` | Story beats, cut snapping, flow review, the `story_edit` pipeline | `Outline`, `Beat`, `outline`, `outline_with`, `snap_cut_range`, `FlowReview`, `FlowIssue`, `review_flow`, `review_flow_mode`, `story_edit`, `StoryEditOutcome`, `outline_to_value` |
| `clip_finder.rs` | Short-form clip candidates from the current cut | `find_clips`, `FindOptions` |
| `detect.rs` | Non-LLM analysis: silences, fillers, takes, rough cut, suggestions, BM25 | `detect_silences`, `silence_candidates`, `refine_silences`, `RefinedSilence`, `detect_fillers`, `detect_takes`, `annotate`, `apply_annotations`, `generate_rough_cut`, `RoughCutOutcome`, `suggest_cuts`, `bm25_rank`, `find_segments`, `DEFAULT_FILLERS`, `TimeRange`, `TakeGroup`, `FillerFindings` |
| `llm.rs` | The strict-JSON LLM call contract | `ask_json`, `ask_json_with`, `ask_json_schema`, `ask_json_schema_served` (also says which model answered), `extract_json` |
| `store.rs` | Persistence: documents in SQLite | `Store` (trait), `SqliteStore` (`open`, `open_in_memory`, `default_path`), `data_dir` |
| `events.rs` | The core → UI event contract | `CoreEvent`, `ProgressTask`, `EventSink`, `SharedSink`, `NullSink`, `send` |
| `error.rs` | The crate error type | `CoreError` (`code`, `to_json`, `hosted`, `is_fatal_model_error`), `HostedErrorKind`, `Result` |
| `time.rs` | Frame-exact time and timecode | `RationalTime`, `snap_to_frame`, `to_frame`, `fps_tick_rate`, `timecode`, `timecode_frames` |
| `capabilities.rs` | What this machine can do right now; decides demo mode | `Capabilities`, `probe`, `system_ram_gb` |
| `setup.rs` | First-run status and whisper model downloads from Hugging Face | `SetupStatus`, `TierStatus`, `RuntimeStatus`, `status`, `embeddings_reachable`, `MODEL_TIERS`, `ModelTier` (a whisper download tier, distinct from `model::ModelTier`), `download_whisper_model` |
| `llm_runtime.rs` | Ollama pulls and the managed llama-server sidecar | `ollama_cli_available`, `ollama_pull`, `install_llama_server`, `llama_server_installed`, `download_gguf`, `list_ggufs`, `start_managed`, `stop_managed`, `managed_running` |
| `download.rs` | Generic streaming HTTP download with progress and an optional sha256 check | `download_to`, `DownloadOpts` |
| `progress.rs` | Child-process output → progress events | `stream_progress`, `Delimiter` |
| `inflight.rs` | One background job per key (RAII) | `try_claim`, `InFlight` |
| `pty.rs` | In-app terminal sessions | `spawn`, `write`, `resize`, `kill`, `kill_all` |
| `resolve.rs` | DaVinci Resolve plugin install and timeline push | `status`, `ResolveStatus`, `install_plugin`, `send_timeline`, `SendToResolveOutcome` |
| `demo.rs` | Fixture transcript for demo mode and tests | `fixture_transcript` |
| `secrets.rs` | The Hosted API key in the OS key store (Keychain / Credential Manager / kernel keyring) | `SecretStore`, `KeyringStore`, `MemoryStore`, `remove_legacy_plaintext` |

The registry (`tools.rs:REGISTRY`) has 65 rows. `Caller::can_use` shows the UI
all 65, MCP clients 64 (every row except the one `meta` row,
`apply_instruction`), and the agent loop the 28 rows marked `agent`. 19 rows
are `mutating`. The LOCAL / HOSTED switch and its key are not registry rows:
they are Tauri commands (ADR-0004, ADR-0007). The names and contracts are documented
in `docs/tool-api.md`.

## Invariants

### Tool registry (`tools.rs`)

- `Caller` is the only place per-caller policy lives. `defs` and `dispatch`
  both filter through `Caller::can_use`, so a caller can call exactly the tools
  it is shown: `Ui` → all, `Mcp` → `!meta`, `Agent` → `agent && !meta`.
- Arguments are schema-checked (`validate_args`) for every caller except `Ui`.
  Only `string`, `number`, `boolean`, `array`, and `object` types are checked.
- Handlers receive `caller.source()`, so edits are attributed to the caller;
  `Agent { attribute_to }` separates attribution from permission.
- `export`, `export_clip`, `export_clips` (once per batch), and `delete_project` ask the user
  (`Editor::request_confirmation`) when the source is `ActionSource::McpClient`.
- Adding a tool means adding a row. `core/tests/doc_parity.rs` requires every
  row to be documented in `docs/tool-api.md` and every tool documented there to
  be a row, and `core/tests/mock_parity.rs`
  requires every row to be implemented in `frontend/src/ipc/mockApi.ts`.

### Data model (`model.rs`)

- A `Timeline` partitions the source media into included and excluded `Clip`s.
  Cuts flip `included` and never delete source. `Timeline::normalize` drops
  clips shorter than `EPS`, sorts by `source_in`, merges touching clips with
  the same `included` flag (never two included clips where either has origin
  `Split`), and refreshes `order` and `cut_count`.
- `EditOp` serializes internally tagged (`"type"`, snake_case). The edit
  journal stores `SetClips` snapshots as inverses (see ADR-0002).
- Saved files stay loadable through serde defaults and aliases on `Project`
  (`saved_clips` accepts `shorts`, `clip_candidates` accepts
  `short_candidates`) and `SavedClip` (`format` defaults to `Vertical`,
  `crop_x` to 0.5). `Project::upgrade`, called by `SqliteStore::load_project`,
  turns a legacy single-range clip into one piece.
- A `SavedClip` is not a timeline `Clip`: it is a standalone edit with its own
  `Timeline` (`Timeline::restricted_to` over its `ranges`).

### AI layer (`agent.rs`, `story.rs`, `clip_finder.rs`, `llm.rs`)

- The agent loop runs at most `MAX_STEPS` = 12 model rounds and sends only the
  last 12 history turns, on `Editor::inference_conversation()` (one model per
  run). When the model server is unhealthy (or the first chat call fails with
  anything but a `CoreError::Hosted` error) it falls back to the offline
  router, which checks silence intent, then clip intent (`wants_shorts`, which
  never cuts the main video), then a cut intent that cuts only the single best
  keyword match. A Hosted error (no key, rejected key or request) is returned
  instead, with nothing edited.
- Identical repeated tool calls are answered from a cache instead of being
  re-dispatched. The cache is cleared after any round in which a mutating tool
  succeeded. After 2 read-only rounds, one steering message is sent;
  `find_clips` and `create_clip` do not count as read-only.
- When the model ends with a plain-text answer, the summary is that text plus
  a bracketed receipt derived from the actions that actually landed
  (`[Applied N change(s) — undoable.]`, or a no-change note), not from the
  model's text. If the turn's successful calls include `cut_by_transcript`,
  `cut_range`, or `apply_edits` and actions landed, one model-backed
  `story::review_flow` runs and its flow note (when it checked any boundary)
  is appended after the receipt.
- A successful `story_edit` call ends the turn with its own summary; the
  receipt is added only when actions landed. The offline router, the step
  limit, and a failed model call after edits write their own summaries.
- `llm::ask_json_schema` makes at most 2 attempts, the second with a repair
  message quoting the parse error. With a schema it asks the server for
  constrained decoding (`response_format`). If the first attempt's request is
  rejected by a local server, it retries once without `response_format` (never
  for a `CoreError::Hosted` error); the repair attempt sends `response_format`
  again and returns its error without retrying, so a server that rejects
  schemas can cost three requests and then fail.
- `story::outline_with` caches only LLM outlines, under KV key
  `outline:{project_id}`, as `(transcript hash, outline, model)`; a cached
  outline counts only when that model is the one the current mode would use
  (an old two-part entry counts as the local model's), and an outline made
  partly by each model (a Hosted run that fell back) isn't cached. LLM outlines
  are chunked at 120 segments; beats under 60 s are merged into a neighbour.
- Every AI step re-raises `CoreError::is_fatal_model_error()` (no key, no
  consent, rejected key or request) instead of substituting its rule-based
  result: `find_clips` and each scoring batch, the outline, flow review,
  story edit, cleanup, chapters and title/description.
- `story::story_edit` allows one run per project at a time
  (`inflight::try_claim`). Without a healthy model server (Local mode only —
  in Hosted mode the client is always "healthy" and problems come back as
  errors) it cuts with the duration planner (`Editor::plan_duration_cut`) when
  given a target, and returns `Unavailable` otherwise. With a model it
  outlines and asks the model which whole beats to cut, all on its own
  conversation client, and changes nothing before the plan passes its guards;
  then it cuts audio-confirmed silences (`Editor::cut_confirmed_silences`) and
  the planned beats.
- `story_edit` refuses a plan (`Inference` error) that cuts every beat, all
  but one of more than 2 beats, or over 80% of the duration unless a target is
  set and what remains is at least half of it. It drops isolated mid-video
  beats with `cut_priority <= 3` from the plan (contiguity guard). If the cut
  is still more than 8% over a target, the duration planner trims it toward
  the target.
- `story_edit` then reviews and repairs the flow in a bounded loop (at most 3
  reviews and 2 repairs), stopping early when the flow is coherent, the issue
  count does not fall, nothing is repairable, or the timeline changed. Repairs
  only restore segments the same run cut.
- `clip_finder::find_clips` holds the project's one scan slot
  (`Editor::begin_clip_scan`; a second run errors), clamps `count` to 1–60
  (`MAX_COUNT`) and `min_s` to at least 3 s (`max_s` to at least `min_s` + 1).
  It builds windows that start and end on a usable (kept, non-filler,
  non-throwaway) segment within one beat, and falls back to windows that span
  beats when none fit. It scores them heuristically, keeps a diverse pool of
  `max(24, count)` (`POOL_MAX`), and has the model score it in batches of 6
  (`BATCH`) by index. With `scan` (`build_pool`), the pool is up to 90
  (`SCAN_POOL`) when a model will score it and uncapped when none is
  reachable, filled round-robin across beats (every beat's best before any
  beat's second; a round cut off by the cap keeps its best-scoring windows;
  `diversify` when the windows span beats). A scan drops windows overlapping
  a saved clip before scoring and again at the write
  (`Editor::replace_clip_candidates`); when that leaves nothing, it writes an
  empty list. It returns `Found { candidates, method }`.
  Two candidates sharing more than 30% of the shorter one's source seconds
  (`MAX_OVERLAP`, `model::pieces_overlap`) are the same moment. With a theme
  and model scoring, judged candidates under 30 (`MIN_THEMED_SCORE`) are
  dropped, but only when at least one judged candidate scores 30 or more.
  `min_score` drops judged candidates under it.

### Persistence, events, errors, time

- `SqliteStore` stores JSON documents in tables `projects`, `transcripts`,
  `preferences` (a single row, `CHECK (id = 1)`), `kv`, and `embeddings`. All
  access goes through one `Mutex<Connection>`. Saves are upserts, except
  `save_embeddings`, which deletes a project's vectors and re-inserts them in
  one transaction. `list_projects` returns trashed projects too (with
  `deleted_at` set).
- `SqliteStore::open` (not `open_in_memory`) migrates older databases: it runs
  `ALTER TABLE projects ADD COLUMN deleted_at` (ignoring the error once the
  column exists) and sets `PRAGMA user_version = 1` when it is lower.
- `events::send` is the one place a `CoreEvent` becomes a wire event
  (`event.name()`, `event.payload()`). The 11 wire names are `progress`,
  `agent-step`, `timeline-changed`, `media-assets-changed`, `projects-changed`,
  `transcript-changed`, `clips-changed`, `mcp-ready`, `confirm-request`,
  `terminal-output`, and `terminal-exit`.
- `CoreError::code` maps variants to stable strings (`not_found`,
  `invalid_argument`, `unavailable`, `io`, `storage`, `inference`, `export`,
  `internal`); `to_json` is always `{"error": {"code", "message"}}`.
- `time::snap_to_frame` rounds to the nearest frame (identity when
  `fps <= 0`); timecodes are `HH:MM:SS:FF`, non-drop.
- `capabilities::probe` re-checks the machine on every call; demo mode is on
  when `FABLE_DEMO=1` or ffmpeg/ffprobe can't be found.
- `inflight::try_claim` gives at most one live claim per key; the key is
  released when the guard drops, including during a panic.

## Side Effects

- **SQLite** (`store.rs`): creates or opens `library.db` under
  `store::data_dir()` (`<OS data dir>/roughcut`) and reads and writes the five
  tables above. `SqliteStore::delete_project` hard-deletes a project's rows
  from `projects`, `transcripts`, and `embeddings` and its `journal:` and
  `outline:` KV keys, as five separate statements without a transaction.
  `llm_runtime::start_managed` saves the sidecar endpoint as
  `inference_endpoint` in preferences once the sidecar answers.
- **HTTP**: LLM calls (`llm.rs`, `agent.rs`, `story.rs`, `clip_finder.rs`) and
  health probes (`setup::status` `GET {endpoint}/models`) go to the configured
  inference endpoint. Embedding calls (`setup::embeddings_reachable`, and the
  themed `clip_finder` search through `Editor::hybrid_find`) go to the
  embedding model on the same endpoint. `setup::download_whisper_model`
  fetches whisper models from Hugging Face (pinned sha256) through the generic
  `download::download_to`. `llm_runtime::install_llama_server` looks up the
  latest llama.cpp release through the GitHub API and downloads its macOS
  asset; `download_gguf` downloads user-supplied `.gguf` URLs; neither is
  checksummed. `start_managed` polls the sidecar's `GET /models` every 2 s for
  up to 4 minutes.
- **Subprocesses**: `llm_runtime.rs` runs `which ollama`, `ollama pull`,
  `tar`, and the llama-server sidecar on `127.0.0.1:47861`; `pty.rs` spawns a
  login shell; `resolve.rs` runs `python3` against the running Resolve;
  `capabilities.rs` runs `sysctl` on macOS.
- **Filesystem**: `download_to` writes `<dest>.part` and renames it into
  place, after a sha256 check only when one is pinned (whisper models).
  `install_llama_server` downloads `llama-server.tar.gz` into
  `<data dir>/bin/`, extracts it into `bin/llama-extract/`, moves the files
  beside `llama-server` into `bin/`, deletes the archive and extract folder, and
  sets `llama-server` to mode `0755`. `resolve::install_plugin` writes into
  Resolve's `Scripts/Utility` folder; `resolve::send_timeline` writes
  `<temp dir>/roughcut-resolve-<pid>.py` and removes it after the run.
- **Events**: progress, agent steps, and change notifications through the
  `EventSink` passed to the `Editor`.
- **Environment**: `FABLE_DEMO`, `INFERENCE_ENDPOINT`, `INFERENCE_MODEL`,
  `EMBEDDING_MODEL` (read by `Preferences::default`, and by the serde default
  when a saved preferences doc has no `embedding_model`), and `SHELL`
  (`pty.rs`, falling back to `/bin/zsh`). `pty::spawn` sets
  `TERM=xterm-256color` in the shell and, when given MCP details,
  `ROUGHCUT_MCP_URL` and `ROUGHCUT_MCP_TOKEN`.
- **Process-global state**: the managed llama-server child
  (`llm_runtime.rs`), PTY sessions (`pty.rs`), and story-edit claims
  (`story.rs`).

## Verification

- Run: `cargo test -p roughcut-core` (unit tests, `core/tests/e2e.rs` through
  the registry, and the doc and mock parity guards).
- After changing a type exported under `ts-bindings` (in `model.rs`,
  `events.rs`, `setup.rs`, `resolve.rs`, or
  [`adapters`](./adapters/README.md)), regenerate the frontend bindings:
  `TS_RS_EXPORT_DIR="$(pwd)/frontend/src/ipc/generated" cargo test -p roughcut-core --features ts-bindings export_bindings`
  (from the workspace root).
