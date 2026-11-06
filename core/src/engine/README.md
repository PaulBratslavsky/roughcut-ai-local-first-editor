# engine (`roughcut_core::engine`)

## Overview

The `Editor` owns all in-memory project state (projects, transcripts, and
undo/redo journals). It wires the storage, video, transcription, inference, and
embedding adapters, and implements the project, media, edit, clip, search, and
confirmation operations that the tool registry calls. Export is not part of
this package: `export`, `export_clip`, and `snapshot` are `impl Editor` methods
in `core/src/export/mod.rs`. The Tauri app, MCP clients, and the local agent
loop all call the same methods. The crate root re-exports it as
`roughcut_core::Editor` (`core/src/lib.rs`).

Each submodule covers one concern and adds methods to `Editor`. All of them
share the `Inner` state defined in `mod.rs`.

| File          | Concern                                                                                                 |
| ------------- | ------------------------------------------------------------------------------------------------------- |
| `mod.rs`      | `Editor`, `Inner`, the per-project state cache, constructors, and adapter accessors                     |
| `projects.rs` | Project lifecycle, trash, read accessors, preferences, and layout/audio offset                          |
| `media.rs`    | Media import/append/screen media, transcription, and the background cleanup and index pass             |
| `edits.rs`    | The mutation seam: `apply_edit`, journal, undo/redo/history, checkpoints, rough cut, and suggestions    |
| `clips.rs`    | Saved clips, clip candidates, and the 9:16 face-centred crop                                            |
| `semantic.rs` | Embedding index, semantic and hybrid search, and the duration-cut planner                               |
| `confirm.rs`  | User approval of destructive ops that an external MCP client requests                                   |
| `route.rs`    | The LOCAL / HOSTED switch: `RouteState` (key, chip status), the per-run `HostedRouter`, the app commands' Editor methods |

This package depends on the following crate modules but does not own them:
`core/src/error.rs` (`CoreError`, `Result`),
`core/src/model.rs` (`Project`, `Timeline`, `EditOp`, `EditAction`,
`Transcript`, `Preferences`, ...), `core/src/store.rs` (`Store`, `SqliteStore`,
`data_dir`), `core/src/adapters/` (`VideoEngine`, `Transcriber`, `Embedder`,
`InferenceClient`, `face`, `video`), `core/src/events.rs`
(`CoreEvent`, `send`), `core/src/detect.rs`, `core/src/time.rs`
(`snap_to_frame`), `core/src/agent.rs` (`clean_transcript`),
`core/src/inflight.rs` (`try_claim`), and `core/src/capabilities.rs`
(`probe`).

## Interface

### Types

- `Editor` (`mod.rs`): a `Clone` handle around `Arc<Inner>`. It is cheap to
  clone and every clone shares the same state.
- `Inner` (`mod.rs`):
  - Public fields: `store: Box<dyn Store>`, `video: Box<dyn VideoEngine>`,
    `transcriber: Box<dyn Transcriber>`, `sink: SharedSink`, `demo: bool`.
  - Crate-visible field: `pub(crate) rough_cut_peaks: Mutex<Option<Vec<u8>>>`.
  - Private fields: `embedder_override`, `confirms` (pending oneshot senders),
    `require_confirm: AtomicBool`, and
    `state: Mutex<HashMap<Uuid, ProjectState>>`.
- `ProjectState` (`mod.rs`): one cache entry, holding
  `{ project: Option<Project>, transcript: Option<Transcript>, undo: Vec<JournalEntry>, redo: Vec<JournalEntry> }`.
- `JournalEntry` (`mod.rs`): a persisted undo/redo record
  `{ action: EditAction, inverse: EditOp, redo: EditOp }`.
- `EditOutcome` (`mod.rs`): `{ action, timeline, split_clips }`. For
  `EditOp::SplitClip`, `split_clips` holds the two resulting clips.
- `ClipSource` (`clips.rs`, re-exported): `Candidate(Uuid)` or
  `Ranges(Vec<SourceRange>)`.
- `ClipChanges` (`clips.rs`, re-exported, `Default`): a partial update with
  fields `title`, `ranges`, `format`, and `crop_x`. A `None` field is left
  unchanged.
- `FaceCrop` (`clips.rs`, re-exported, `Serialize`): `{ clip, crop_x: Option<f64>, frames_sampled, frames_with_face }`.
- `DurationCutPlan` (`semantic.rs`, `Serialize`): `{ segment_ids, before_s, projected_after_s, target_s, method: "centrality" | "heuristic", notes }`.
  The type is `pub` but its module is private and it is not re-exported, so
  callers only get it as the return value of `plan_duration_cut`.

### Construction and accessors (`mod.rs`)

| Method                                                | Notes                                                                                                                                                                  |
| ----------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `new(store, video, transcriber, sink, demo)`          | Empty state. Confirmations off. No embedder override.                                                                                                                  |
| `bootstrap(sink) -> Result<Self>`                     | Opens `SqliteStore` at `SqliteStore::default_path()`, then calls `bootstrap_with_store`.                                                                               |
| `bootstrap_with_store(store, sink) -> Result<Self>`   | Production wiring with `AutoVideoEngine` and `AutoTranscriber`. Confirmations follow `ROUGHCUT_NO_CONFIRM`. Runs `purge_trash(30)` and `secrets::remove_legacy_plaintext()` at startup.                         |
| `test_instance() -> Self`                             | In-memory SQLite with `inference_endpoint = http://127.0.0.1:9/v1`, `MockVideoEngine`, `MockTranscriber`, `NullSink`, `demo = true`, and an in-memory key store (`MemoryStore`).                                 |
| `demo_mode() -> bool`                                 | `inner.demo \|\| capabilities::probe().demo()`, re-evaluated on every call.                                                                                             |
| `sink()`, `store()`, `video()`                        | Return the shared sink (cloned), the store, and the video engine.                                                                                                      |
| `inference() -> Result<Box<dyn InferenceClient>>`     | One run's client: Local mode, a new `OpenAiCompatClient` from `prefs.inference_endpoint`; Hosted mode, a `HostedRouter` (batch policy). `inference_conversation()` is the same with the conversation policy (chat, story edit). See `route.rs`. |
| `embedder() -> Result<Arc<dyn Embedder>>`             | Returns the test override if one is set. Otherwise builds a new `OllamaEmbedder` from `prefs.inference_endpoint` and `prefs.embedding_model`.                          |
| `set_embedder_for_tests(embedder)`                    | Installs the embedder override.                                                                                                                                        |
| `with_project(id, f(&Project, Option<&Transcript>))`  | Read access. Loads the project on demand, then runs `f` with the state lock held.                                                                                      |

### Projects and preferences (`projects.rs`)

| Method                                                                   | Notes                                                                                                              |
| ------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------ |
| `async create_project(name, file_path: Option<&str>) -> Result<Project>` | Optionally probes media to seed `Timeline::new(duration)`.                                                         |
| `duplicate_project(id, name) -> Result<Project>`                         | Save-as: new id and timestamps, copies the project and transcript, starts with empty undo/redo.                    |
| `open_project(id) -> Result<Project>`                                    | Loads the project and calls `backfill_index_if_missing`.                                                           |
| `save_project(id) -> Result<Project>`                                    | Persists the cached project and its transcript, if one is loaded.                                                  |
| `list_projects() -> Result<Vec<ProjectSummary>>`                         | Passes through to `Store::list_projects`.                                                                          |
| `delete_project(id)` / `restore_project(id)`                             | Soft delete (sets `deleted_at`) and undelete.                                                                      |
| `purge_trash(days) -> Result<u32>`                                       | Hard-deletes projects trashed more than `days` days ago.                                                           |
| `get_timeline(id)`, `get_transcript(id)`                                 | Read accessors.                                                                                                    |
| `async media_assets(id) -> Result<adapters::video::MediaAssets>`         | Waveform peaks and thumbnail paths, plus the playable copy (`playback_path`) or why it failed (`playback_error`). Starts building a missing copy in the background. |
| `get_preferences()`, `set_preferences(prefs)`                            | Full load and full replace. `set_preferences` does no validation.                                                  |
| `update_preferences(patch: &serde_json::Value, source: ActionSource)`    | Partial JSON merge with an unknown-key check, a loopback guard, and a guard on the model-switch fields.            |
| `set_layout(id, Layout)`, `set_audio_offset(id, offset_s)`               | Project metadata: persisted, not journaled.                                                                        |

### Media and transcription (`media.rs`)

| Method                                                                    | Notes                                                                                                              |
| ------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| `async import_media(project_id: Option<Uuid>, file_path) -> Result<Media>` | Probes the file. Given a project: no media yet → sets it with `Timeline::new(media.duration)`; same video moved → relinks; a different video → `InvalidArg`. |
| `async transcribe(id, language: Option<&str>) -> Result<Transcript>`      | On-device transcription, then a background LLM cleanup and semantic-index pass.                                    |
| `update_segment_texts(id, &[(Uuid, String)]) -> Result<u32>`              | Replaces segment text and keeps every timestamp. Used by the LLM cleanup pass.                                     |
| `async append_media(id, file_path) -> Result<Media>`                      | Lossless concat onto the source (a new file in `<data dir>/media/`), swaps the project media, appends an included `Manual` clip over the new span, and extends the undo history and checkpoints to match. |
| `async attach_screen_media(id, file_path) -> Result<()>`                  | Sets `project.screen_media` to a screen file that started with the camera file (probed; offset 0).                 |

### Edits, history, checkpoints, and suggestions (`edits.rs`)

| Method                                                                                                  | Notes                                                                                                      |
| ------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------- |
| `apply_edit(id, op: EditOp, source: ActionSource) -> Result<EditOutcome>`                               | The journaled mutation path for the main cut. It calls `apply_edit_labeled(.., None)`.                     |
| `apply_edit_labeled(id, op, source, label: Option<String>)`                                             | Runs normalize, validate, apply, journal, persist, notify. `label` overrides the history description.      |
| `annotate_transcript(id) -> Result<(detect::FillerFindings, Vec<detect::TakeGroup>)>`                   | Persists filler and repeated-take flags on the transcript. Not journaled.                                  |
| `async cut_confirmed_silences(id, source) -> Result<Vec<EditAction>>`                                   | Cuts only the silences that the audio waveform confirms, with one `CutRange` edit per silence.            |
| `async generate_rough_cut(id, Option<Aggressiveness>, source) -> Result<(EditAction, Timeline, u32, u32)>` | Returns `(action, timeline, cut_count, suggestion_count)`. Transcribes first if needed.                  |
| `get_suggestions`, `accept_suggestion`, `accept_all_suggestions`, `dismiss_suggestion`                  | Review the Tier-2 `CutSuggestion`s. Accepting applies them as `CutSegments`.                               |
| `undo(id)`, `redo(id)` -> `Result<(Option<EditAction>, Timeline)>`                                      | Step through the journal one entry at a time.                                                              |
| `undo_actions(id, ids) -> Result<(usize, Timeline)>`                                                    | Undoes one chat turn's actions, but only if they are still the newest entries.                             |
| `history(id) -> Result<History>`, `jump_to(id, target: Option<Uuid>) -> Result<Timeline>`               | Full timeline of edits, and target-driven time travel. `None` returns to the original state.               |
| `create_checkpoint(id, name, auto)`, `get_checkpoints`, `restore_checkpoint`, `delete_checkpoint`       | Named full snapshots of the clips. A restore is a normal undoable `SetClips` edit.                          |

### Saved clips and face crop (`clips.rs`)

| Method                                                                              | Notes                                                                                  |
| ----------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------- |
| `set_clip_candidates(id, Vec<ClipCandidate>)`, `get_clip_candidates(id)`            | Replace or read the candidates from one `find_clips` run.                              |
| `replace_clip_candidates(id, Vec<ClipCandidate>, drop_saved: Option<f64>)`          | A run's final write: with `drop_saved`, drops candidates overlapping a saved clip now. |
| `begin_clip_scan(id) -> Result<ClipScan>`                                           | Claims the project's one `find_clips` slot; dropping the guard frees it.               |
| `create_clip(id, ClipSource, title: Option<&str>, format: Option<ClipFormat>)`      | Snapshots the main cut, restricted to the clip's ranges. Returns `SavedClipSummary`.   |
| `async create_clips(id, &[Uuid], format: Option<ClipFormat>)`                       | Makes each candidate that validates, then frames each vertical one in turn (`MakeClips` progress). Returns `(clips, errors)`. |
| `list_clips(id)`, `get_clip(id, clip_id)`                                           | Summaries in creation order, or the full `SavedClip` including its timeline.           |
| `update_clip(id, clip_id, ClipChanges)`, `delete_clip(id, clip_id)`                 | New ranges re-snapshot the current main cut.                                           |
| `async center_clip_on_face(id, clip_id) -> Result<FaceCrop>`                        | Face pass that always applies the crop it finds.                                       |
| `auto_center_clip(id, clip_id)`                                                     | Fire-and-forget face pass that uses compare-and-set on `crop_x`.                       |

### Semantic index and search (`semantic.rs`)

| Method                                                                                 | Notes                                                                    |
| -------------------------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| `backfill_index_if_missing(id)`                                                        | Starts a deduplicated background index build when the index is missing. |
| `async index_transcript(id) -> Result<u32>`                                            | Embeds the speech segments and persists the vectors. Best-effort.       |
| `plan_duration_cut(id, target_s) -> Result<DurationCutPlan>`                           | Proposes cuts to reach a target duration. It does not apply them.       |
| `async hybrid_find(id, query, k) -> Result<(Vec<(Uuid, f64)>, &'static str)>`          | BM25 and cosine results fused with RRF. Returns `"hybrid"` or `"bm25"`. |
| `async semantic_find(id, query, k) -> Result<Option<Vec<(Uuid, f32)>>>`                | Cosine top-k. `None` means no usable index.                             |

### Confirmation (`confirm.rs`)

| Method                                               | Notes                                                                                               |
| ---------------------------------------------------- | --------------------------------------------------------------------------------------------------- |
| `async request_confirmation(summary) -> bool`        | Asks the UI to approve a destructive op (export, delete) that an external MCP client requested.     |
| `resolve_confirmation(id, approved)`                 | The answer path, called from a Tauri command.                                                       |
| `require_confirmations_for_tests(on)`                | Turns the confirmation requirement on or off.                                                       |

### The model switch (`route.rs`)

| Method | Notes |
| --- | --- |
| `inference()`, `inference_conversation()` | One run's client (batch / conversation policy). Local mode: the local client. Hosted mode: a `HostedRouter`. |
| `model_label() -> String` | "Claude Opus 5.5", or "the on-device model" in Local mode or after a Hosted fallback. |
| `model_status() -> Result<ModelStatus>` | The chip's state; no network, the key store read only once. |
| `set_model_mode(mode, consent)` | Hosted needs a stored key and, the first time, `consent: true` (stores `hosted_consent_at`). Sends `model-route`. |
| `set_hosted_model(model)` | One of `HOSTED_MODELS`. |
| `async set_hosted_key(Option<&str>) -> HostedKeyResult` | `Some`: printable ASCII, checked with `GET /v1/models/{model}`; saved if it works or Anthropic is unreachable. `None`: deletes it and switches to Local. |
| `async check_hosted_key() -> KeyCheck` | Setup's Test. |
| `set_inference_for_tests(local, hosted)`, `set_secret_store_for_tests(store)`, `set_hosted_key_for_tests(key)` | Test seams: fake clients, an in-memory key store, a key stored without a network check. |

### Subdirectories

- None. `engine/` is a leaf package.

## Invariants

### State and locking

- `ensure_loaded` loads a project once. It reads `load_project`,
  `load_transcript`, and the `journal:{project_id}` KV entry only while
  `entry.project` is `None`, and never refreshes from the store afterwards. If
  the journal JSON fails to parse, the journal is silently treated as empty;
  an undo or redo entry that doesn't parse (an op this build no longer has,
  such as the removed `reorder_clip`) is dropped on its own
  (`mod.rs:journal_entries`).
- A single `std::sync::Mutex` (`Inner.state`) guards every project, transcript,
  and journal. Async methods read through `with_project` and release the lock
  before they `.await`. Store writes do happen while `state` is held.
- When the two locks are nested, the order is `state` then `rough_cut_peaks`
  (`edits.rs:apply_edit_labeled`).
- A missing project returns `CoreError::NotFound("project")` or
  `NotFound("project {id}")`.

### Edit pipeline and journal (`edits.rs`)

- Every journaled (undoable) change to the main cut goes through `apply_edit`
  or `apply_edit_labeled`. Undo, redo, and checkpoint restore replay ops with
  the same `apply_op` function.
- Two methods change `project.timeline` directly without journaling:
  `import_media` (sets it, only for a project with no media yet) and
  `append_media` (extends it). Neither leaves the history describing other
  media: see Media and transcription.
- `normalize_op` frame-snaps the time fields of `CutRange`, `RestoreRange`,
  `TrimClip`, and `SplitClip` with `snap_to_frame(x, project.fps())`. With
  `SetGlobalPadding { linked: true }` it forces `end_s = start_s`.
- `CutRange` and `RestoreRange` are rejected before any mutation with
  `InvalidArg` when `start >= end`, `start < 0`, or `start >= timeline.duration`.
- Journal entries store full clip and padding snapshots
  (`inverse` and `redo` are both `EditOp::SetClips`), not inverse ops. The
  transcript is not part of the snapshot: transcript changes made by
  `RoughCut` are persisted but are not reverted by undo.
- `UNDO_CAP = 100`. When the undo stack grows past it, the oldest entry is
  evicted. Every successful `apply_edit` clears the redo stack. Redo pushes
  back onto the undo stack without re-checking the cap.
- `undo` and `redo` on an empty stack return `Ok((None, timeline))`, not an
  error.
- `undo_actions` rejects an empty `ids` list. It also returns `InvalidArg`
  unless the top `ids.len()` undo entries are exactly the given ids.
- `jump_to` re-derives its direction from the live stacks on every step. It
  gives up after `2 * UNDO_CAP + 8` steps with `CoreError::Other`. A target
  that is in neither stack gives `NotFound`. If it moved at least one step, it
  clears `project.suggestions`.
- `apply_op`: `CutSegments` and `RestoreSegments` need a transcript (otherwise
  `InvalidArg`) and fail with `NotFound("segment {id}")` for an unknown
  segment.
- `apply_op`: `RoughCut` first applies `prefs.default_padding` (if non-zero)
  to the cuts already there, then adds its own exclusions, tagged
  `ClipOrigin::AiCut`, on top. Its cuts carry their own margins, so padding
  them would shrink each into the word it removes; cuts shorter than
  `detect::MIN_CUT_S` (0.2 s) are dropped.
- `transcribe` trims every word to the audio (`detect::trim_words_to_audio`)
  when raw peaks load, before saving.
- `rough_cut_peaks` belongs to the whole Editor, not to one project. Any
  `apply_edit` takes it, whatever the op. Journal replays pass `None`.
- `cut_confirmed_silences` returns `Ok(vec![])` when there is no transcript,
  no media, or no raw peaks. It cuts only `refine_silences` results with
  `source == "confirmed"`, using `min = max(prefs.silence_min_duration_s, 0.6)`.
- `generate_rough_cut` first takes an automatic `"Before rough cut"`
  checkpoint and ignores any failure to do so. Suggestions are stored and are
  never applied automatically.

### Checkpoints and suggestions (`edits.rs`)

- `CHECKPOINT_CAP = 40`. When it is exceeded, the oldest `auto` checkpoint is
  evicted first, otherwise the oldest overall. A blank name becomes
  `"Checkpoint"`.
- `accept_suggestion` cuts only the segment ids that are still live. If none
  are live, it removes the suggestion and returns
  `InvalidArg("that suggestion was stale ...")`.
- `accept_all_suggestions` applies one `CutSegments` edit (one undo step) and
  removes every suggestion, including stale ones.
- `dismiss_suggestion` and `delete_checkpoint` do not call `ensure_loaded`.
  For a project that is not in the cache they change nothing and return
  `Ok(())`, but `dismiss_suggestion` still sends `TimelineChanged`. An unknown
  suggestion or checkpoint id also returns `Ok(())`, not `NotFound`.

### Saved clips (`clips.rs`)

- Saved clips and candidates are project metadata: persisted but never
  journaled, so undo and redo do not touch them.
- Every clip mutation goes through `update_project_meta`. On error nothing is
  saved and no event is sent.
- `set_ranges` rejects an empty range list.
- `set_ranges` clamps each range to `[0, duration]` and frame-snaps it, then
  rejects it with `InvalidArg` if `!(start < end) || start >= duration`.
- `set_ranges` sorts the ranges and merges any that touch or overlap
  (`start <= last.end + EPS`).
- `set_ranges` rejects ranges that leave under 1.0 s of kept footage in the
  current cut.
- `create_clip` defaults: `crop_x = 0.5`, `Landscape`, and title
  `"Clip {n}"`.
- `create_clip` from a `Candidate` defaults to `Vertical`, copies the
  candidate's title, score, reason, and hook, builds the clip from its
  `pieces`, and removes the candidate only after the clip validates. An
  explicit `format` or non-blank `title` argument overrides these defaults.
- `create_clips` does the same per id inside one `update_project_meta`. An id
  that isn't listed becomes a `ClipBatchError`; one that fails validation
  stays listed and becomes one too. It then awaits the face pass for each
  vertical clip, with compare-and-set on `crop_x`; a failed pass leaves the
  crop at 0.5. A clip deleted during framing is left out of the result.
- `update_clip` clamps `crop_x` to `[0, 1]` and ignores a blank title.
- `set_crop_if` is compare-and-set: the background face pass writes `crop_x`
  only if the clip's crop still equals the value captured when the pass
  started (within `1e-9`). A crop the user set by hand is never overwritten.
  The test `background_face_pass_never_overrides_a_hand_set_crop` covers this.
- `face_crop` refuses with `InvalidArg` when `project.screen_media` is set
  (camera-bubble composites) or the project has no media.
- `face_crop` samples at most `FACE_SAMPLES = 12` frames, each grabbed
  `FACE_FRAME_WIDTH = 640` px wide. If it cannot see a face, it leaves the crop
  unchanged and returns `crop_x: None`.

### Media and transcription (`media.rs`)

- `transcribe` requires project media. It clears `project.suggestions`,
  because a new transcript gets new segment ids.
- In demo mode, `transcribe` skips audio extraction and the LLM cleanup. The
  background index pass still runs.
- `update_segment_texts` skips unknown segment ids and never changes
  timestamps. It replaces word texts only when the new word count equals
  `seg.words.len()`. It adds the `+llm-cleanup` suffix to `model_used` at most
  once.
- `import_media` with a project id never discards an edit. A project with no
  media gets the media and a fresh timeline. If the project already has media
  and the new file has the same duration (within 0.05 s), frame size, and
  frame rate (`same_source`), it is a relink: the media is swapped and keeps
  its id, and the cuts, journal, checkpoints, transcript, and saved clips stay.
  Any other video is refused with `InvalidArg` (create a new project, or use
  `append_media`).
- `append_media` requires existing media and that the file exists
  (`NotFound("file {path}")`). The guard (same video codec and resolution,
  same audio format) lives in `adapters::video::concat_media`.
- `append_media` keeps the history valid (`extend_history`): every undo/redo
  `SetClips` snapshot and every checkpoint gets the same included clip over the
  appended span, and the journal is persisted. An undo or restore afterwards
  therefore still covers the whole, longer source. The append itself is not
  journaled, so it cannot be undone (its registry row is not `mutating`).

### Projects and preferences (`projects.rs`)

- `delete_project` is a soft delete. It never touches the source video, and
  it evicts the project from the cache.
- `purge_trash` hard-deletes only projects whose `deleted_at` is older than
  `now - days`.
- `update_preferences` requires the patch to be a JSON object.
- `update_preferences` rejects keys that `Preferences` does not have.
- `update_preferences` merges nested objects key by key (`merge_into`). Any
  other value, arrays included, replaces the old one.
- Zero-egress guard: when `source != ActionSource::Ui`, `update_preferences`
  refuses to change `inference_endpoint` to a URL that fails
  `is_loopback_url`. That check accepts `localhost`, `*.localhost`,
  `127.0.0.0/8`, and `::1`; unparseable URLs and `0.0.0.0` fail it.
  `set_preferences` bypasses both preference checks.
- Model-switch guard: `update_preferences` rejects a change to `model_mode`,
  `hosted_model` or `hosted_consent_at` from every source (only the
  `route.rs` methods behind the app's shell commands change them). Every
  preferences load-modify-save holds `Inner::prefs_write`.

### The model switch (`route.rs`)

- In Hosted mode each run gets its own `HostedRouter`. It re-reads the mode
  before every request (a switch to Local takes effect at once), sends the
  hosted model to Anthropic and the local model's name to the local server,
  and never goes from the local model back to Claude within a run. Only
  "unreachable" falls back; a conversation run that already used Claude
  stops instead, and stays stopped. No key, no consent, a rejected key or
  request is returned as `CoreError::Hosted`.
- `healthy()` is always true in Hosted mode, so key and consent problems come
  back from `chat` as errors instead of quietly selecting rule-based paths.
- The key is read from the `SecretStore` once and cached in `RouteState`
  (a failed read is cached too, until the key is set or forgotten); one
  `AnthropicClient` is kept per key.
- `RouteState` sends `model-route` only when the chip's text would change.
- `media_assets` starts the playable copy (a background
  `ensure_playable_copy`) **before** it awaits peaks and thumbnails, and only
  when demo mode is off, a tokio runtime is present, and there is neither a
  copy (`playable_copy_path`) nor a failure recorded in this run
  (`playable_copy_failure`); otherwise the event below would bring the
  refetch straight back here. When the copy lands **or** fails it sends
  `MediaAssetsChanged`, and the refetch returns `playback_path` or
  `playback_error`. "Plays as-is" and "another call is building it" send
  nothing.

### Semantic (`semantic.rs`)

- `backfill_index_if_missing` does nothing when:
  - there is no tokio runtime,
  - there is no transcript or it has no segments, or
  - an index already exists.
- `backfill_index_if_missing` runs at most one build per project, enforced by
  a static set and `inflight::try_claim`. The claim is released when the build
  completes or panics.
- `index_transcript` embeds only non-silence, non-filler, non-empty segments,
  64 per request. Any embedding error returns `Ok(0)` and saves nothing.
- `semantic_find` returns `Ok(None)` when there is no index, when the index was
  built with a different `embedder.model_id()`, or when the query embedding
  fails.
- `plan_duration_cut` rejects `target_s <= 0` and a missing transcript. It
  protects the first 2 and last 2 speech segments.
- `plan_duration_cut` ranks segments by cosine to the mean embedding. Without
  an index it puts alt takes first, then segments with few words per second.
- `plan_duration_cut` selects segments greedily until the target is met. It
  never applies the cuts.
- `hybrid_find` uses reciprocal-rank fusion with score `1/(60 + rank)` over
  `k * 2` results from each source, then truncates to `k`. It requires a
  transcript.

### Confirmation (`confirm.rs`)

- The engine does not decide when to ask. In `core/src/tools.rs`, only the
  `export`, `export_clip`, and `delete_project` handlers call
  `request_confirmation`, and only when the source is
  `ActionSource::McpClient`. Agent loops cannot call those tools
  (`agent: false`), so the local agent is never asked.
- `request_confirmation` returns `true` right away when `require_confirm` is
  off, which is the default for `Editor::new`, `test_instance`, and
  `ROUGHCUT_NO_CONFIRM=1`.
- When confirmations are on, it waits up to 120 s. A timeout or dropped
  sender returns `false`.
- `bootstrap_with_store` turns confirmations on unless `ROUGHCUT_NO_CONFIRM`
  is `"1"`.
- `resolve_confirmation` ignores unknown or expired ids.

### Known caveats (current code behavior)

- `set_crop_if` saves, bumps `updated_at`, and emits `ClipsChanged` even when
  the compare fails and the crop is unchanged.
- `auto_center_clip` calls `tokio::spawn` without checking for a runtime, so
  it panics if called outside one.
- `cut_confirmed_silences` ignores the error from each per-silence
  `apply_edit`. It returns `Ok` with only the actions that succeeded.
- If `extract_audio_wav` fails, `transcribe` returns early and does not remove
  the temp WAV.

## Side Effects

- **Store (SQLite through `Store`):**
  - `save_project`: lifecycle, edits, clips, checkpoints, suggestions, media,
    layout, and audio offset.
  - `save_transcript`: transcription, annotation, cleanup, edits, and
    duplicates.
  - `set_kv("journal:{id}")` / `get_kv`: the undo journal.
  - `load_preferences` / `save_preferences`.
  - `load_embeddings` / `save_embeddings`.
  - `list_projects` / `delete_project`: the trash purge.
  - `bootstrap` opens or creates the SQLite DB at `SqliteStore::default_path()`.
- **OS key store** (`route.rs` through `SecretStore`): reads, writes and
  deletes the Hosted API key; `bootstrap_with_store` deletes plaintext keys
  left under `<data dir>/secrets/` by the removed frontier path.
- **Network** (`route.rs`): in Hosted mode, AI requests to
  `api.anthropic.com`; in either mode, one `GET /v1/models/{model}` when the
  user saves or tests a key.
- **Events**: `model-route` from `set_model_mode`, `set_hosted_key(None)` and
  the routers.
- **Events (`events::send`):**
  - `TimelineChanged`: edits, undo/redo, suggestion removal
    (`accept_suggestion`, `accept_all_suggestions`, `dismiss_suggestion`),
    `import_media`, `append_media`, and `set_layout`.
  - Other suggestion changes send no event of their own:
    `generate_rough_cut` stores suggestions after its edit's
    `TimelineChanged`, `jump_to` clears them after its last step's
    `TimelineChanged`, and `transcribe` clears them and sends only
    `TranscriptChanged`.
  - `TranscriptChanged`: transcribe, annotate, and segment text updates.
  - `ProjectsChanged`: create, duplicate, delete, and restore.
  - `ClipsChanged`: clip and candidate mutations.
  - `MediaAssetsChanged`: `append_media` and the background playable copy
    (when it lands or fails).
  - `ConfirmRequest`: `request_confirmation`.
  - `progress(ProgressTask::Transcribe, ..)`: transcription stages.
  - `RoughCut` mutates the transcript but sends only `TimelineChanged`.
  - `set_audio_offset`, `attach_screen_media`, `purge_trash`,
    `create_checkpoint`, and `delete_checkpoint` send no event.
    `restore_checkpoint` emits `TimelineChanged` through `apply_edit`.
- **Background tasks:**
  - `transcribe` spawns the LLM cleanup (`agent::clean_transcript`) followed
    by `index_transcript`.
  - `backfill_index_if_missing` spawns `index_transcript`.
  - `auto_center_clip` spawns the face pass.
  - `media_assets` spawns `ensure_playable_copy` when there is neither a copy
    nor a recorded failure.
  - `find_faces` runs face detection in `spawn_blocking`.
- **Filesystem:**
  - `transcribe` writes `temp_dir()/roughcut-<media.id>.wav` and removes it
    once the transcriber returns. A failed audio extraction skips the removal
    (see Known caveats).
  - The face pass writes PNG frames under `data_dir()/cache/faces/<uuid>/` and
    removes the directory afterwards.
  - `load_raw_peaks` reads or writes `data_dir()/cache/<key>/peaks_raw.bin`
    for the rough cut and confirmed-silence cuts.
  - `append_media` writes `data_dir()/media/joined-<timestamp>-<8 hex>.mp4`
    (`.mkv` unless every input is MP4 / MOV; the folder is created on demand)
    through `adapters::video::concat_media`. The original files are left
    untouched.
- **Network:**
  - `embedder().embed(..)` (indexing and `semantic_find` queries) sends
    transcript text to `prefs.inference_endpoint`.
  - The background LLM cleanup also sends transcript text to that endpoint.
  - `update_preferences` limits non-UI callers to loopback endpoints.
- **Subprocesses:** media probing, audio extraction, peak decoding, and frame
  grabs go through the video adapter. `concat_media` runs `ffprobe` and then
  concatenates the inputs. In demo mode the auto adapters use fixtures
  instead.
- **Environment:**
  - `bootstrap_with_store` reads `ROUGHCUT_NO_CONFIRM`.
  - `demo_mode()` reads `FABLE_DEMO` (`"1"` forces demo mode) through
    `capabilities::probe()` on every call.
- **Startup:** `bootstrap_with_store` runs `purge_trash(30)`, which
  hard-deletes projects that have been in the trash for more than 30 days.

## Verification

- Run `cargo test -p roughcut-core` from the workspace root
  (`/Users/paul/fable`).
- Engine unit tests only: `cargo test -p roughcut-core --lib engine::`
  - `engine::clips::tests::background_face_pass_never_overrides_a_hand_set_crop`
  - `engine::projects::tests::loopback_urls`
  - `engine::projects::tests::merge_is_deep_for_objects_and_replaces_otherwise`
- End-to-end through `Editor::test_instance`:
  `cargo test -p roughcut-core --test e2e`
