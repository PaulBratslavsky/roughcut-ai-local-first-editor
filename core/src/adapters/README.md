# adapters

## Overview

`crate::adapters` is where `roughcut-core` reaches outside the process: media
toolchain binaries (ffmpeg, ffprobe, whisper-cli), local HTTP model servers,
the macOS camera/mic/screen capture stack, and Apple Vision. Each capability
sits behind a trait (or a small free-function API) so the rest of the core stays
portable. `VideoEngine` and `Transcriber` each have a production
implementation, a demo `Mock*`, and an `Auto*` selector that picks between them
**on every call** using `crate::capabilities::probe()`. Because of that,
installing ffmpeg or a whisper model mid-session takes effect without a
restart. `Embedder` has `OllamaEmbedder` and `MockEmbedder` but no `Auto*`
selector. `InferenceClient` has only `OpenAiCompatClient`, with no mock.

The module does not own project state; callers in `crate::engine`, `crate::agent`,
`crate::export`, `crate::tools`, etc. pass in `Media`/`Timeline` values and
receive results or file paths.

| File | Boundary | Production impl | Fallback / mock |
| --- | --- | --- | --- |
| `mod.rs` | Module root, re-exports, `demo_mode()` | — | — |
| `video.rs` | Probe, render, audio/frame extraction, timeline assets | `FfmpegCli` (ffmpeg/ffprobe CLIs) | `MockVideoEngine`, selected by `AutoVideoEngine` |
| `transcribe.rs` | Speech-to-text | `WhisperRs` (feature `whisper-native`) or `WhisperCli` | `MockTranscriber`, selected by `AutoTranscriber` |
| `inference.rs` | Chat completions / tool calls | `OpenAiCompatClient` (OpenAI-compatible HTTP) | — (no mock here) |
| `embed.rs` | Text embeddings for semantic search | `OllamaEmbedder` (OpenAI-compatible HTTP) | `MockEmbedder` |
| `face.rs` | Face finding and 9:16 crop math | Apple Vision (`#[cfg(target_os = "macos")]`) | Off macOS: `CoreError::Unavailable` |
| `capture.rs` | Native capture of one take: devices, camera mode, AVFoundation sessions, H.264 writers | AVFoundation + AVAssetWriter (`#[cfg(target_os = "macos")]`) | Off macOS: empty device lists, `start` is `CoreError::Unavailable` |
| `record.rs` | Recording sessions (takes, pause/resume, finish) and the recordings library | `capture.rs`; ffmpeg only to concat takes and normalize mic audio | — (macOS only at runtime) |

## Interface

### `mod.rs`

- Declares `pub mod embed; face; inference; record; transcribe; video;`.
- Re-exports `Embedder`, `MockEmbedder`, `OllamaEmbedder`; `ChatMessage`,
  `ChatRequest`, `ChatResponse`, `InferenceClient`, `OpenAiCompatClient`,
  `ToolCall`; `AutoTranscriber`, `MockTranscriber`, `Transcriber`,
  `WhisperCli`; `AutoVideoEngine`, `FfmpegCli`, `MockVideoEngine`,
  `VideoEngine`. Everything else (e.g. `face::*`, `record::*`,
  `embed::cosine`, `inference::FunctionCall`, `transcribe::WhisperRs`,
  `video::RenderJob`) is reached by its module path.
- `pub fn demo_mode() -> bool`: returns `crate::capabilities::probe().demo()`.

### `video.rs`

- `#[async_trait] pub trait VideoEngine: Send + Sync`
  - `probe(&self, file_path: &str) -> Result<Media>`: metadata only, no decode.
  - `render_mp4(&self, job: &RenderJob<'_>, sink: &SharedSink) -> Result<()>`:
    renders the timeline's included clips and emits `ProgressTask::Export` progress.
  - `extract_audio_wav(&self, media: &Media, out_path: &str) -> Result<()>`:
    mono 16 kHz WAV (the whisper input).
  - `extract_frame(&self, media: &Media, at: f64, width: u32, out_path: &str) -> Result<()>`:
    one still at `at` seconds, `width` px wide (height keeps the aspect).
  - `media_assets(&self, media: &Media) -> Result<MediaAssets>`: waveform
    peaks plus a thumbnail filmstrip, cached on disk.
- `pub struct FfmpegCli`: production `VideoEngine`.
- `pub struct MockVideoEngine`: demo `VideoEngine`.
- `pub struct AutoVideoEngine`: per-call selector between the two.
- `pub struct RenderJob<'a> { media, timeline, out_path, audio_offset_s, screen: Option<(&Media, &crate::model::Layout)>, vertical_crop_x: Option<f64> }`:
  `screen` is dual-capture compositing; `vertical_crop_x` renders 9:16 at
  1080×1920 (0 = left, 0.5 = centre, 1 = right).
- `pub struct MediaAssets { peaks_path: Option<String>, peaks_per_second: u32, thumbnails: Vec<ThumbnailRef>, playback_path: Option<String> }`
  and `pub struct ThumbnailRef { time: f64, path: String }`. Both are
  `Serialize`, and `ts_rs::TS`-exported under feature `ts-bindings`. They carry
  **file paths, not payloads**.
- `pub const PEAKS_PER_SECOND: u32 = 50`.
- Binary resolution: `ffmpeg_path()`, `ffprobe_path() -> Option<PathBuf>`,
  `ffmpeg_available() -> bool` (true only when both resolve).
- Playback: `pub async fn ensure_playable_copy(media: &Media) -> Result<Option<String>>`.
- Cache and peaks: `asset_cache_key(&Media) -> String`, `raw_peaks`,
  `normalize_peaks`, `compute_peaks` (pure), `decode_raw_peaks(file_path)`,
  `load_raw_peaks(&Media) -> Option<Vec<u8>>`.

### `transcribe.rs`

- `#[async_trait] pub trait Transcriber: Send + Sync` with
  `transcribe(&self, media: &Media, wav_path: &str, language: &str, project_id: Option<Uuid>, sink: &SharedSink) -> Result<Transcript>`.
  The input is a 16 kHz mono WAV.
- `pub struct WhisperCli`: shells out to `whisper-cli`.
- `pub struct WhisperRs`: `#[cfg(feature = "whisper-native")]`, in-process
  whisper.cpp through `whisper-rs`.
- `pub struct MockTranscriber`: returns `crate::demo::fixture_transcript(media)`.
- `pub struct AutoTranscriber`: per-call selector.
- Model discovery: `pub const MODEL_CANDIDATES` (best first:
  `ggml-large-v3-turbo-q5_0.bin`, `ggml-small-q5_1.bin`, `ggml-base.bin`),
  `models_dir() -> PathBuf`, `resolve_whisper_model() -> Option<PathBuf>`.
- Engine detection: `whisper_native_enabled() -> bool`
  (`cfg!(feature = "whisper-native")`), `whisper_available() -> bool`
  (`whisper-cli` on PATH).

### `inference.rs`

- `#[async_trait] pub trait InferenceClient: Send + Sync` with
  `chat(&self, ChatRequest) -> Result<ChatResponse>` and `healthy(&self) -> bool`.
- `pub struct OpenAiCompatClient { pub endpoint, pub api_key: Option<String>, .. }`
  with `new(endpoint: &str, api_key: Option<String>)`. It works against
  llama-server, Ollama, or any OpenAI-compatible server.
- Wire types:
  - `ChatMessage { role, content, tool_calls, tool_call_id }`, with constructors
    `system`, `user`, `assistant`, and `tool_result(id, content)`.
  - `ToolCall { id, kind (serde "type"), function: FunctionCall }`.
  - `FunctionCall { name, arguments }`, where `arguments` is a JSON-encoded string.
  - `ChatRequest { model, messages, tools: Option<Vec<Value>>, temperature, response_format: Option<Value> }`.
  - `ChatResponse { message }`.

### `embed.rs`

- `#[async_trait] pub trait Embedder: Send + Sync` with
  `embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>` and
  `model_id(&self) -> String`, used to detect stale indexes.
- `pub struct OllamaEmbedder` with `new(endpoint, model)` and
  `async fn reachable(&self) -> bool`.
- `pub struct MockEmbedder`: deterministic hashing embedder,
  `model_id() == "mock-embedder"`.
- `pub fn cosine(a: &[f32], b: &[f32]) -> f32`.

### `face.rs`

- `pub struct FaceBox { cx, cy, w, h: f64, confidence: f32 }`: normalised
  0..1 to the frame, origin top-left.
- `pub fn detect_faces(image: &Path) -> Result<Vec<FaceBox>>`: PNG/JPEG on disk.
  **Blocking**; call it from `spawn_blocking`.
- `pub fn subject(faces: &[FaceBox]) -> Option<&FaceBox>`: the speaker in one frame.
- `pub fn crop_x_for_faces(frames: &[Vec<FaceBox>], frame_w: u32, frame_h: u32) -> Option<f64>`:
  the render's `vertical_crop_x`.
- `pub fn sample_times(kept: &[(f64, f64)], max: usize) -> Vec<f64>`: source
  times to sample across the kept `(source_in, source_out)` spans.
- `pub fn png_size(path: &Path) -> Result<(u32, u32)>`: reads the size from
  the IHDR chunk.

### `record.rs`

- Session lifecycle (one global session):
  - `start_capture_full(sink, video_index, microphone, is_screen, screen_index: Option<u32>) -> Result<String>`.
    Returns the take-1 path. `screen_index = Some` turns on dual camera+screen capture.
  - `start_capture(sink, video_index, microphone, is_screen)` and
    `start_camera(sink, camera, microphone)` are thin wrappers.
  - `pause() -> Result<RecordingStatus>`, `resume(sink) -> Result<RecordingStatus>`,
    `status() -> RecordingStatus`, `stop() -> Result<RecordingOutcome>`.
  - `abort_on_exit()` is the app-exit hook.
- Devices and permission: `list_devices() -> Result<CaptureDevices>`,
  `screen_permission() -> bool`, `request_screen_permission() -> bool`.
- Display preview: `screen_preview(display_index) -> Result<Vec<u8>>`
  (`[width u32 LE][height u32 LE][RGBA…]`, empty when nothing new),
  `stop_screen_preview()`.
- Library: `list_recordings() -> Result<Vec<RecordingFile>>`,
  `combine_recordings(paths: &[String]) -> Result<String>`,
  `delete_recording(path: &str) -> Result<()>`.
- Serializable DTOs (`ts_rs::TS` under `ts-bindings`): `CaptureDevice`,
  `CaptureDevices`, `RecordingStatus`, `RecordingFile`, `RecordingOutcome`.

### `capture.rs`

- Devices: `cameras() -> Vec<Device>`, `microphones() -> Vec<Device>`
  (`id` = AVCaptureDevice uniqueID), `displays() -> Vec<Display>`
  (`id` = CGDirectDisplayID, `name` from `label_displays`).
- One take: `LiveTake::start(TakeSpec) -> Result<LiveTake>` (blocking),
  `failure(&self) -> Option<String>`, `duration_s(&self) -> f64`,
  `finish(self) -> Result<()>` (blocking). `TakeSpec` names the video source
  (`VideoSource::Camera(id)` or `::Display(id)`), the microphone, the optional
  overlay display, and the output paths.
- Display preview: `preview_frame(display_id) -> Result<Option<PreviewFrame>>`
  (newest frame since the last call, RGBA, ≤ `PREVIEW_MAX_WIDTH` = 640 px
  wide), `preview_stop()`.
- Pure helpers (unit-tested): `pick_mode`, `target_fps`, `encode_size`,
  `bitrate`, `label_displays`.

There are no child directories with contracts.

## Invariants

### Selection and demo mode

- `demo_mode()` / `Capabilities::demo()` is `FABLE_DEMO == "1"` **or**
  ffmpeg/ffprobe not resolvable. It is recomputed on every call and never cached.
- `AutoVideoEngine` routes each method to `FfmpegCli` when
  `capabilities::probe().media_ready()`, otherwise to `MockVideoEngine`.
- `AutoTranscriber` order:
  1. `caps.demo()`: `MockTranscriber`.
  2. Built with `whisper-native`: always `WhisperRs` (whisper-cli is never consulted).
  3. Otherwise `WhisperCli` if `caps.whisper_cli`.
  4. Otherwise `CoreError::Unavailable("no transcription engine: ...")`.
- `MockVideoEngine` invents `probe` metadata (300 s, 30 fps, 3840×2160, 48 kHz,
  codec `"h264 (demo)"`). `render_mp4`, `extract_audio_wav` and `extract_frame`
  return `CoreError::Unavailable`. `media_assets` returns no peaks, no
  thumbnails, and no playback path.

### Binary and model resolution

- `video::resolve_binary` order:
  1. `FFMPEG_PATH` / `FFPROBE_PATH`, if it points to a file.
  2. `<data_dir>/bin/<name>[.exe]`.
  3. Each `PATH` entry.
  4. `/opt/homebrew/bin`, `/usr/local/bin`, `/usr/bin`.

  `data_dir` is `crate::store::data_dir()`. Nothing is cached. `record.rs` uses
  the same `video::ffmpeg_path()` / `ffprobe_path()`.
- `WhisperCli` runs the bare name `whisper-cli`, so it depends on `PATH`
  rather than `resolve_binary`.
- `resolve_whisper_model`: `WHISPER_MODEL` wins if it names an existing file.
  Otherwise it takes the first existing `MODEL_CANDIDATES` file under
  `models_dir()` (`<data_dir>/models`). Otherwise it returns `None`, and the
  transcribers fail with `CoreError::Unavailable("no whisper model found ...")`.
  (The `WhisperCli` doc comment mentions only `ggml-base.bin`, but the code
  uses the full candidate list.)

### Probing (`FfmpegCli::probe`)

- Fails with `NotFound` when the file does not exist, and with
  `InvalidArg("no video stream")` when ffprobe reports no video stream, so
  audio-only files are rejected.
- Falls back to 30 fps when `r_frame_rate` is missing or has a zero
  denominator, and to 48 kHz when there is no audio stream or sample rate.

### Rendering (`FfmpegCli::render_mp4`)

- Fails with `InvalidArg("timeline has no included clips")` when
  `timeline.included_clips()` is empty.
- Video cuts are hard trims. Each clip's audio gets fades of
  `min(0.02, dur/4)` s at both edges. Clips are joined with
  `concat=n=N:v=1:a=1`.
- The A/V offset is applied once, before the per-clip trims, and only when
  `|audio_offset_s| > 0.001`: `adelay` when positive, `atrim` when negative
  (`audio_sources`). The shifted stream is then `asplit` into one copy per
  clip, because a filter output label must be read by exactly one filter.
  Reading `[ash]` once per clip made ffmpeg 8 feed every clip after the first
  audio from the wrong position, without an error. Without an offset, every
  clip reads the raw input `[0:a]`, which may be read any number of times.
- ffmpeg runs with `-hide_banner`, and a failed render's error carries the last
  800 characters of stderr, where ffmpeg states the cause.
- Compositing applies only when `job.screen` is `Some` **and**
  `layout.mode != "camera"`. Mode `"screen"` clone-pads the screen to the
  camera's duration. Any other mode overlays a camera bubble: size
  `layout.size.clamp(0.10, 0.45)`, minimum 64 px, `"round"` or rounded-rect
  mask, 3% inset, and the corner is chosen by whether `layout.corner`
  contains `l` / `t`.
- `vertical_crop_x` applies one crop after the concat:
  `w = trunc(ih*9/16/2)*2`, `x = (iw-ow)*clamp(x,0,1)`, then
  `scale=1080:1920`.
- Progress fraction is capped at `0.99` until ffmpeg exits successfully. Then
  `1.0` ("render complete") is emitted.

### Assets and playback

- The asset cache directory is `<data_dir>/cache/<asset_cache_key>`. The key is
  the first 16 hex chars of SHA-256 over `"v4:"` + path + file size + mtime
  seconds, so it follows **file identity, not media id**: projects that share
  footage share assets. Bump the `"v4:"` prefix to invalidate old caches. (The
  `VideoEngine::media_assets` doc comment still says `<media id>`.)
- `media_assets` regenerates peaks only when `peaks.bin` is missing, and
  thumbnails only when `thumb_0001.jpg` is missing. The thumbnail interval is
  `(duration/240).clamp(1, 10)` s. It never builds `play.mp4`; it only reports
  it when it exists.
- Peaks: `raw_peaks` takes the max |sample| per bucket scaled to `0..=255`,
  where `bucket = sample_rate / peaks_per_second`. A trailing partial bucket is
  kept. `decode_raw_peaks` decodes at 8 kHz. `normalize_peaks` rescales to a
  240 ceiling only when `4 <= loudest < 240`; silence and already-loud files
  are left untouched. `peaks_raw.bin` holds raw analysis levels and
  `peaks.bin` holds normalized display levels.
- `ensure_playable_copy` builds a copy only when `moov` comes after `mdat`
  (`needs_faststart`) or the first audio stream has more than 2 channels.
  - It returns `Ok(None)` when the source plays as-is **or** when another task
    already holds the per-key claim (`IN_FLIGHT` + `crate::inflight::try_claim`).
    Callers must retry later.
  - It writes `play.mp4.part` and then renames it, so a crash never leaves a
    `play.mp4` that looks complete.

### Transcription

- `WhisperCli` passes `-l <language>` only when `language` is non-empty and
  not `"auto"`. Segments with empty text are dropped, and tokens that start
  with `[_` are skipped. Timestamps are converted from ms to seconds.
- `WhisperRs` uses greedy decoding (`best_of=1`) with token timestamps, on
  `min(available_parallelism, 8)` threads. Its progress is mapped to the
  `0.25..0.98` band so the bar never moves backwards.
- `MockTranscriber` emits progress 0.25, 0.5, 0.75, 1.0, sleeping 120 ms after each.

### Inference and embeddings

- Both HTTP clients trim trailing `/` from `endpoint`. The endpoint must
  already include the API base (e.g. `.../v1`), because the clients append
  `/chat/completions`, `/embeddings`, or `/models` to it.
- `OpenAiCompatClient` has a 5 s connect timeout and a 300 s request timeout.
  If the builder fails it falls back to `Client::default()`, which has no
  timeouts. `chat` reads only `choices[0].message`; there is no streaming.
  All failures map to `CoreError::Inference`.
- `OllamaEmbedder` uses `reqwest::Client::new()`, so `embed` has no connect or
  request timeout.
- `healthy()` / `reachable()` send `GET {endpoint}/models` with a 2 s timeout
  and return true only on a 2xx. `healthy()` never sends the API key (only
  `chat` does), so a key-protected endpoint reports unhealthy.
- `Embedder::embed` returns an empty output for an empty input, with no HTTP
  call. `OllamaEmbedder` returns vectors in the response's `data` order with
  no re-sort by `index`, and coerces non-numeric elements to `0.0`.
- `MockEmbedder` makes 128-dim vectors: lowercase, alphanumeric words of 3+
  chars, FNV-1a hash mod 128, not normalised.
- `cosine` returns `0.0` for empty input, mismatched lengths, or a zero-norm vector.

### Face finding

- `subject` only considers faces with `confidence >= 0.6` (`MIN_CONFIDENCE`)
  and `h >= 0.04` (`MIN_FACE_H`). The largest `w*h` wins.
- `crop_x_for_faces` returns `None` when no frame has a subject, or when fewer
  than a quarter of the frames do. It uses the **median** subject `cx`, and
  returns `Some(0.5)` when the source is already 9:16 or narrower. Otherwise
  the result is clamped to `0..=1`.
- `sample_times` returns about one sample per 2 s of kept time, with
  `n.clamp(min(6, max), max)`, taken at midpoints and mapped back to source
  time in play order. It is empty when the total kept time is `<= 0` or
  `max == 0`.
- macOS `detect_faces` converts Vision's bottom-left origin to top-left and
  requires a UTF-8 path. Off macOS it always returns
  `CoreError::Unavailable("face detection needs macOS (Apple Vision)")`.

### Capture (`capture.rs`, `record.rs`)

- Capture is native (ADR-0006): one `AVCaptureSession` for the video source +
  microphone, a second for the display in dual sessions, and one
  `AVAssetWriter` per output file (H.264 via the hardware encoder, mono AAC
  48 kHz, fragmented MP4 every 2 s so a crash keeps what was written).
- **Camera mode.** If `lockForConfiguration` succeeds, `pick_mode` chooses the
  format (landscape, then closest to 16:9, then largest area; among equal
  sizes the frame-rate range closest to 30 fps) and the lock is released once
  the session runs. If another app holds the lock (OBS with a custom-format
  source), the camera is shared at its current format. Recording never needs
  the lock.
- **Sync.** All streams carry host-clock timestamps. Writers are created on
  each stream's first video frame and all start at `t0` = the latest first
  frame across video streams; earlier samples (video and audio) are dropped.
- **Start is honest.** `LiveTake::start` checks camera/mic authorization
  (prompting once when undetermined) and Screen Recording, then waits up to
  8 s for every stream to deliver. A source that doesn't fails the start with a
  reason; a take that never started leaves no files.
- **Failure.** `failure()` reports AVCaptureSession runtime errors, writer
  errors, and stalls (no camera video or no microphone audio for 5 s; display
  frames are not watched).
- **Timestamps.** Samples more than 10 s away from the host clock's "now" are
  dropped (a UVC camera shared with OBS delivered a first frame stamped 0).
  Video is capped at ~30 fps (frames closer than 30 ms are skipped), so a
  camera another app runs at 60 fps records at 30.
- `ROUGHCUT_CAPTURE_DEBUG=1` traces sample timestamps and encoder drops to
  stderr.
- **Display preview.** One `AVCaptureScreenInput` session at 8 fps for the
  selected display, BGRA, downsampled in Rust (each output pixel averages four
  samples). No scale factor: with one set, the screen input delivered a single
  frame and stopped. The preview refuses to run while any take is starting or
  running (`LIVE_TAKES`), switches session when the display changes, and stops
  on `preview_stop`, when a take starts, or after 3 s without a poll (a
  watchdog thread).
- Bitrates: camera 0.1 bit/pixel/frame at 30 fps clamped to 4–16 Mbps;
  displays 0.06 clamped to 6–14 Mbps. Encoded sizes are even and fit within
  3840×2160 (or 2160×3840).
- **At most one session**, held in the global `active()` mutex.
  `start_capture_full` refuses with `InvalidArg("a recording is already running")`
  while a session is live **or paused**. A session whose take has failed is
  closed (its file kept) and replaced.
- Start indexes refer to the last `list_devices()` listing (fetched if there is
  none) and are resolved to device IDs once per session, so `resume` reopens
  the same devices even if indexes shift.
- `start_capture_full` checks free disk space with `df -Pk` and needs at least
  2.0 GB. The check is best-effort: parse failures pass. `resume` does not
  re-check.
- File naming: `base = "{presentation|screen|recording}-{Local %Y%m%d-%H%M%S}"`.
  - Takes are `{base}-take{n}.mp4`, plus `{base}-screen-take{n}.mp4` for dual
    sessions, numbered consecutively.
  - `stop` writes `{base}.mp4` and, for dual sessions, `{base}-screen.mp4`.
  - Take files are never deleted.
- `pause`, `resume` and `stop` refuse while `transitioning` (a pause is still
  finalizing). `pause` errors with `"no recording is running"` or
  `"already paused"`. `resume` errors with `"no recording session"` or
  `"not paused"`.
- `pause` closes the take (`finish`) and pushes it when finish succeeded
  **and** the primary file exists. A missing screen file degrades that take to
  camera-only. A take that had already failed is closed the same way; its
  failure becomes `RecordingStatus.error` and its finish error is not raised.
- `status()` is read-only. A failed take reports `paused: true` with `error`
  set; the next `pause`, `resume` or `stop` closes it. `resume` re-checks the
  session after starting the new take and closes it rather than orphaning it.
- `stop` always removes the session. A finalize error does not abort it.
  - With no usable takes: `CoreError::Other("nothing usable was recorded ...")`,
    carrying the finalize note or the last take's failure.
  - The screen stream is concatenated only when **every** take has a screen
    file. A screen concat failure degrades to `screen_path: None`.
- Concatenation:
  - Same-size takes: `-f concat -safe 0`, video stream-copied. The primary
    stream's audio is re-encoded through `dynaudnorm=f=300:g=11` (mono AAC
    192k); screen files and `combine_recordings` copy everything.
  - Takes of different sizes (another app changed the camera format between
    takes) are scaled/padded to the first take's size and re-encoded with
    `h264_videotoolbox`.
  - `combine_recordings` requires at least 2 existing files whose ffprobe
    stream signatures (`codec_type,codec_name,width,height,channels`) match
    exactly. The inputs may be anywhere. The output goes to
    `~/Movies/RoughCut`, which `combine_recordings` does not create.
- `delete_recording` only deletes canonical paths inside `~/Movies/RoughCut`
  (otherwise `InvalidArg("not a RoughCut recording")`). It is idempotent: a
  path that is already gone returns `Ok`.
- `list_recordings` returns up to 50 `*.mp4` files in `~/Movies/RoughCut`,
  newest mtime first.
- Off macOS, device lists are empty and starting fails with
  `CoreError::Unavailable`. `screen_permission()` / `request_screen_permission()`
  return `true` off macOS.

### Known hazards (from reading the code; not covered by tests)

- `render_mp4` uses `[0:a]` without checking that the source has an audio
  stream.
- Recording progress emits one `"recording…"` event per take; the recorder UI
  keeps its own clock.
- Two sessions started in the same second share a `base`; AVAssetWriter
  refuses an existing path, so the second start fails instead of overwriting.
- Display capture uses `AVCaptureScreenInput` (deprecated in favor of
  ScreenCaptureKit, still functional; measured at a steady 30 fps on macOS 26).

## Side Effects

- **Process spawns.** All are `tokio::process` unless noted.
  - `video.rs`: `ffprobe` (probe, audio channel count); `ffmpeg` (render,
    16 kHz WAV extract, frame grab, thumbnails, peaks decode to `pipe:1`,
    playable remux).
  - `record.rs`: `ffmpeg -f concat -safe 0 …` (`stop`, `combine_recordings`;
    re-encoding when take sizes differ); `ffprobe` (durations, video sizes,
    combine signatures); `df -Pk` (blocking `std::process`). Capture itself
    spawns nothing.
  - `transcribe.rs`: `whisper-cli`; `which whisper-cli` (blocking
    `std::process`, also run inside every `capabilities::probe()`).
- **Filesystem writes.**
  - `<data_dir>/cache/<key>/`: `peaks.bin`, `peaks_raw.bin`, `thumb_%04d.jpg`,
    `play.mp4.part` renamed to `play.mp4`.
  - Caller-supplied `out_path` (MP4, WAV, PNG). Existing files are overwritten
    with `-y`.
  - `<wav_path>.transcript.json`, written by `whisper-cli` and never cleaned up.
  - `~/Movies/RoughCut/` (created if missing by `start_capture_full` only):
    take files, concatenated outputs, `combined-<timestamp>.mp4`. Files are
    deleted only through `delete_recording`.
  - A temp concat list in `std::env::temp_dir()`, removed after use. It leaks
    if spawning ffmpeg fails.
- **Network.** HTTP to the configured local or remote endpoint only:
  - `POST {endpoint}/chat/completions`, with an optional `Authorization: Bearer`.
  - `POST {endpoint}/embeddings`.
  - `GET {endpoint}/models`.
- **Environment reads.** `FABLE_DEMO` (via `capabilities`), `FFMPEG_PATH`,
  `FFPROBE_PATH`, `PATH`, `WHISPER_MODEL`, `HOME` (via `dirs`, for
  `~/Movies/RoughCut` and `crate::store::data_dir()`), and `TMPDIR` (via
  `std::env::temp_dir()`, for the concat list).
- **Global state.**
  - `record.rs`: `active()` session (`Mutex<Option<Session>>`, holding the
    `LiveTake`); `device_cache()` (5 s TTL, also the listing start indexes
    refer to).
  - `video.rs`: `ensure_playable_copy`'s `IN_FLIGHT` set.
- **Events.** `crate::events::send` emits `CoreEvent::progress` with
  `ProgressTask::Export` (render), `ProgressTask::Transcribe` (`WhisperRs`,
  `MockTranscriber`) and `ProgressTask::Recording` (capture).
- **OS frameworks (macOS).**
  - Apple Vision through `objc2-vision`: `VNDetectFaceRectanglesRequest`.
  - CoreGraphics TCC FFI: `CGPreflightScreenCaptureAccess`,
    `CGRequestScreenCaptureAccess` (the latter shows a one-time system prompt).
  - AVFoundation through `objc2-av-foundation` (`capture.rs`):
    `AVCaptureDeviceDiscoverySession`, `AVCaptureSession`,
    `AVCaptureDeviceInput`, `AVCaptureScreenInput`, video/audio data outputs,
    `AVAssetWriter`. Delegates run on private serial dispatch queues.
  - CoreGraphics display FFI: `CGGetActiveDisplayList`, `CGDisplayBounds`,
    `CGDisplayCopyDisplayMode` (pixel size), `CGDisplayIsBuiltin`.
  - The first capture asks for camera and microphone access
    (`requestAccessForMediaType`) when macOS hasn't asked yet.
- **Process lifetime.** `abort_on_exit` finishes a live take (its file is
  complete). If the app is killed instead, the take keeps everything up to
  its last 2 s fragment. Dropping a `LiveTake` without `finish` keeps what
  was written, or deletes the files if the take never started.

## Verification

- Run (from the workspace root `/Users/paul/fable`): `cargo test -p roughcut-core`
- This module's unit tests only: `cargo test -p roughcut-core --lib adapters::`
  (pure tests in `embed.rs`, `face.rs`, `video.rs` (`compute_peaks`),
  `capture.rs` (mode choice, encode size, bitrate, display labels) and
  `record.rs` (take numbering, concat arguments)). `tests/e2e.rs` exercises
  `MockVideoEngine`, `MockTranscriber` and `MockEmbedder` offline.
- Capture on real hardware: `cargo run -p roughcut-core --example
  native_check` (camera, pause/resume, dual, display), `preview_check`
  (display preview, switching, off during a take; `PREVIEW_DUMP=<file.ppm>`
  saves a frame); also `record_smoke`,
  `takes_smoke`, `screen_smoke`, `dual_smoke`,
  `crash_smoke`). macOS attributes their camera/mic/screen requests to the
  app that launched the terminal, which needs those permissions.
- `WhisperRs` compiles only with `--features whisper-native`, which the default
  test run leaves out.
