# adapters

## Overview

`crate::adapters` is where `roughcut-core` reaches outside the process: media
toolchain binaries (ffmpeg, ffprobe, whisper-cli), local HTTP model servers,
Claude's Messages API (Hosted mode only), and Apple Vision. Each capability
sits behind a trait (or a small free-function API) so the rest of the core stays
portable. `VideoEngine` and `Transcriber` each have a production
implementation, a demo `Mock*`, and an `Auto*` selector that picks between them
**on every call** using `crate::capabilities::probe()`. Because of that,
installing ffmpeg or a whisper model mid-session takes effect without a
restart. `Embedder` has `OllamaEmbedder` and `MockEmbedder` but no `Auto*`
selector. `InferenceClient` has `OpenAiCompatClient` (local servers) and
`AnthropicClient` (Hosted mode, `anthropic.rs`), with no mock; the choice
between them is made per run by `crate::engine::route`, not here.

The module does not own project state; callers in `crate::engine`, `crate::agent`,
`crate::export`, `crate::tools`, etc. pass in `Media`/`Timeline` values and
receive results or file paths.

| File | Boundary | Production impl | Fallback / mock |
| --- | --- | --- | --- |
| `mod.rs` | Module root, re-exports, `demo_mode()` | — | — |
| `video.rs` | Probe, render, audio/frame extraction, timeline assets, the preview's playable copy, joining footage | `FfmpegCli` (ffmpeg/ffprobe CLIs) | `MockVideoEngine`, selected by `AutoVideoEngine` |
| `transcribe.rs` | Speech-to-text | `WhisperRs` (feature `whisper-native`) or `WhisperCli` | `MockTranscriber`, selected by `AutoTranscriber` |
| `inference.rs` | Chat completions / tool calls | `OpenAiCompatClient` (OpenAI-compatible HTTP) | — (no mock here) |
| `anthropic.rs` | Hosted mode: Claude through the Messages API | `AnthropicClient` (`HostedBackend`) | tests use a local axum stub |
| `embed.rs` | Text embeddings for semantic search | `OllamaEmbedder` (OpenAI-compatible HTTP) | `MockEmbedder` |
| `face.rs` | Face finding and 9:16 crop math | Apple Vision (`#[cfg(target_os = "macos")]`) | Off macOS: `CoreError::Unavailable` |

## Interface

### `mod.rs`

- Declares `pub mod anthropic; embed; face; inference; transcribe; video;`.
- Re-exports `Embedder`, `MockEmbedder`, `OllamaEmbedder`; `ChatMessage`,
  `ChatRequest`, `ChatResponse`, `InferenceClient`, `OpenAiCompatClient`,
  `ToolCall`; `AutoTranscriber`, `MockTranscriber`, `Transcriber`,
  `WhisperCli`; `AutoVideoEngine`, `FfmpegCli`, `MockVideoEngine`,
  `VideoEngine`. Everything else (e.g. `face::*`, `embed::cosine`,
  `inference::FunctionCall`, `transcribe::WhisperRs`, `video::RenderJob`) is
  reached by its module path.
- `pub fn demo_mode() -> bool`: returns `crate::capabilities::probe().demo()`.

### `video.rs`

- `#[async_trait] pub trait VideoEngine: Send + Sync`
  - `probe(&self, file_path: &str) -> Result<Media>`: metadata only, no decode.
  - `render_mp4(&self, job: &RenderJob<'_>, sink: &SharedSink) -> Result<()>`:
    renders the job's spans in order and emits `ProgressTask::Export` progress.
  - `extract_audio_wav(&self, media: &Media, out_path: &str) -> Result<()>`:
    mono 16 kHz WAV (the whisper input).
  - `extract_frame(&self, media: &Media, at: f64, width: u32, out_path: &str) -> Result<()>`:
    one still at `at` seconds, `width` px wide (height keeps the aspect).
  - `media_assets(&self, media: &Media) -> Result<MediaAssets>`: waveform
    peaks plus a thumbnail filmstrip, cached on disk.
- `pub struct FfmpegCli`: production `VideoEngine`.
- `pub struct MockVideoEngine`: demo `VideoEngine`.
- `pub struct AutoVideoEngine`: per-call selector between the two.
- `pub struct RenderJob<'a> { media, pieces: &[Vec<PlaySpan>], out_path, audio_offset_s, screen: Option<(&Media, &crate::model::Layout)>, vertical_crop_x: Option<f64> }`:
  `pieces` is what plays, grouped by piece, in output order (the main cut is
  one piece: `Timeline::play_spans()`; a saved clip is
  `SavedClip::piece_spans()`); `screen` is the project's screen file (one that
  starts with the camera file, `project.screen_media`) and its layout, for
  compositing;
  `vertical_crop_x` renders 9:16 at 1080×1920 (0 = left, 0.5 = centre, 1 =
  right).
- `pub struct MediaAssets { peaks_path: Option<String>, peaks_per_second: u32, thumbnails: Vec<ThumbnailRef>, playback_path: Option<String>, playback_error: Option<String> }`
  and `pub struct ThumbnailRef { time: f64, path: String }`. `playback_error`
  is why building the playable copy failed in this run; it is set only when
  there is no copy. Both are
  `Serialize`, and `ts_rs::TS`-exported under feature `ts-bindings`. They carry
  **file paths, not payloads**.
- `pub const PEAKS_PER_SECOND: u32 = 50`.
- Binary resolution: `ffmpeg_path()`, `ffprobe_path() -> Option<PathBuf>`,
  `ffmpeg_available() -> bool` (true only when both resolve).
- Playback: `pub async fn ensure_playable_copy(media: &Media) -> Result<Option<String>>`
  (builds the copy), `pub fn playable_copy_path(&Media) -> Option<String>`
  (the copy, if it is already on disk) and
  `pub fn playable_copy_failure(&Media) -> Option<String>` (why it failed in
  this run).
- Joining footage: `pub async fn concat_media(paths: &[String]) -> Result<String>`
  (used by `Editor::append_media`).
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
  - `ChatMessage { role, content, tool_calls, tool_call_id, provider_blocks }`,
    with constructors `system`, `user`, `assistant`, and `tool_result(id, content)`.
    `provider_blocks` (`#[serde(skip)]`) carries a Claude reply's original
    content blocks back on the next request of the same run.
  - `ToolCall { id, kind (serde "type"), function: FunctionCall }`.
  - `FunctionCall { name, arguments }`, where `arguments` is a JSON-encoded string.
  - `ChatRequest { model, messages, tools: Option<Vec<Value>>, temperature, response_format: Option<Value> }`.
  - `ChatResponse { message, served_by: Option<ServedBy { model, hosted }>, usage: Option<Usage> }`
    (`ChatResponse::new(message)` for doubles). `OpenAiCompatClient` fills
    `served_by` with the request's model.

### `anthropic.rs`

- `AnthropicClient::new(key)`; `send(&ChatRequest) -> Result<ChatResponse, HostedFailure>`;
  `check_model(model)` (`GET /v1/models/{model}`, no tokens). Implements
  `InferenceClient` and `HostedBackend` (the router's seam, with test doubles).
- Request: `POST /v1/messages`, `anthropic-version: 2026-10-21`, beta
  `server-side-fallback-2026-07-01`; `system` messages joined into `system`;
  same-role turns merged (tool results first); a leading assistant turn gets a
  user turn before it; replies sent back with `provider_blocks` unchanged;
  OpenAI tools → `{name, description, input_schema}`; `response_format` →
  `output_config.format` after dropping unsupported keywords and closing every
  object (a root array is wrapped as `{items}` and unwrapped from the reply);
  `cache_control: ephemeral`, `fallbacks: "default"`, `max_tokens` 16000. Never
  `temperature`, `tool_choice`, `thinking` or `effort`.
- Errors: connect error / 5 s connect / 120 s request timeout → `Unreachable(Offline)`;
  429 and 500/502/503/504/529 retried twice (1 s, 3 s; `retry-after` up to 10 s)
  then `Unreachable(RateLimited | ServerError)`; 401/403 → `Hosted{KeyRejected}`;
  other 4xx → `Hosted{BadRequest}` with the API's message; `stop_reason`
  `refusal` / `max_tokens` → `Hosted{Refused | Truncated}`.

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

  `data_dir` is `crate::store::data_dir()`. Nothing is cached.
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

- The inputs and filter graph are built by the pure `render_plan` (unit
  tested); `render_mp4` only runs ffmpeg on them.
- Fails with `InvalidArg("timeline has no included clips")` when no piece has
  a span.
- **One input per group of pieces, seeked.** `input_groups` puts
  consecutive pieces in one group when each starts at or after the previous
  one's end and at most 30 s later (`SHARED_INPUT_GAP_S`); past 12 groups
  (`MAX_INPUTS`) the in-order neighbours with the smallest gaps merge.
  Pieces played out of source order never share. Group `k` is input `k`,
  opened with `-ss (start − preroll)` rounded down to the millisecond (not
  below 0), where `preroll = 1 s + max(audio_offset_s, 0)`; its spans' trims
  are measured from that same rounded value. A render therefore decodes about
  what plays, not the source from 0:00. When compositing, the screen file
  gets one input per group too (`n_groups + k`), seeked the same but never
  into its last 3 s (`SCREEN_TAIL_S`); the difference comes off its
  timestamps (`setpts`), and its last frame is clone-padded (`tpad`) in both
  pip and screen modes.
- Video trims sit half a frame early (`0.5 / frame_rate`), so a span edge on
  a frame boundary keeps the frame at its start and drops the one at its end
  regardless of float rounding. Audio trims are exact.
- `render_mp4` sets `kill_on_drop`: a render whose caller went away stops.
- Video cuts are hard trims. Each span's audio gets fades of
  `min(0.02, dur/4)` s at both edges. Spans are joined, in order, with
  `concat=n=N:v=1:a=1`.
- The A/V offset is applied once per input, before that input's trims, and
  only when `|audio_offset_s| > 0.001`: `adelay` when positive, `atrim` when
  negative (`audio_sources`). The shifted stream is then `asplit` into one
  copy per span, because a filter output label must be read by exactly one
  filter. Reading `[ash]` once per clip made ffmpeg 8 feed every clip after the
  first audio from the wrong position, without an error. Without an offset,
  every span reads its raw input `[k:a]`, which may be read any number of
  times. The preroll keeps a delayed track's first second real audio rather
  than `adelay` silence.
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
  `(duration/240).clamp(1, 10)` s. It never builds `play.mp4`: it reports it
  when it exists (`playable_copy_path`), and otherwise reports the failure
  recorded in this run, if any (`playable_copy_failure`).
- Peaks: `raw_peaks` takes the max |sample| per bucket scaled to `0..=255`,
  where `bucket = sample_rate / peaks_per_second`. A trailing partial bucket is
  kept. `decode_raw_peaks` decodes at 8 kHz. `normalize_peaks` rescales to a
  240 ceiling only when `4 <= loudest < 240`; silence and already-loud files
  are left untouched. `peaks_raw.bin` holds raw analysis levels and
  `peaks.bin` holds normalized display levels.
- `ensure_playable_copy` makes the preview's copy, `play.mp4`. Only the
  preview uses the copy: transcription, render and export read the original
  through ffmpeg. In order:
  1. A `play.mp4` already on disk is returned at once, before any probing.
  2. A failure recorded for this source in this run is returned again as
     `CoreError::Other(<reason>)`, without re-running ffmpeg.
  3. Otherwise `build_playable_copy` runs, and any error it returns (probe or
     ffmpeg) is recorded under the source's `asset_cache_key`. The record is
     in memory, not on disk, so a restart (say, after fixing ffmpeg) tries
     again.
- `build_playable_copy`: one ffprobe call (`container_info`: `format_name`,
  the first video and audio streams' codecs, the audio channel count) and the
  pure `playable_fix(&ContainerInfo)` decide whether a copy is
  needed and how (unit tests in `playable_tests`). ffprobe output that
  `parse_container_info` can't read (no JSON, no format name) is an error,
  not "not an MP4, no audio".
  - **MP4 / MOV** (`format_name` lists `mp4` or `mov`): a copy only when the
    first audio stream has more than 2 channels, or the video is HEVC not
    tagged `hvc1` (ffprobe's `codec_tag_string`, e.g. `hev1`). Where the
    `moov` index sits doesn't matter: the preview reads the file through the
    shell's `rcmedia` protocol, which serves every byte range WebKit asks for
    (OBS writes `moov` last; a full-size copy only to move it cost as much
    disk as the recording). Video is copied; audio is copied, or converted to
    stereo AAC when it has more than 2 channels.
  - **Any other container** (OBS's `.mkv`, `.webm`, `.avi`, …): always a copy.
    H.264 or HEVC video is copied; any other video codec (VP9, AV1, …) is
    converted to H.264 (`libx264 -preset fast -crf 18 -pix_fmt yuv420p`),
    which takes minutes rather than seconds. AAC audio with at most 2 channels
    is copied; anything else becomes stereo AAC (192k); no audio stream
    gives `-an`.
  - HEVC copied into the MP4 is tagged `hvc1` (`-tag:v hvc1`); Apple players
    refuse it otherwise.
  - Only the first video track and the first audio track are mapped
    (`-map 0:v:0?`, plus `-map 0:a:0?` when there is audio): the tracks
    `ContainerInfo` describes, the render plays (`[0:a]`), and transcription
    (`extract_audio_wav`) and the waveform (`decode_raw_peaks`) read. Left to itself,
    ffmpeg would pick the audio track with the most channels (OBS's track 2,
    say). Subtitle and data tracks are dropped (`-sn -dn`, e.g. OBS
    chapters), and the copy is `+faststart`.
  - The H.264 conversion also scales to even dimensions
    (`-vf scale=trunc(iw/2)*2:trunc(ih/2)*2`), because `yuv420p` needs them
    (a browser screen recording can be 1365×767).
  - ffmpeg runs with `-hide_banner -v error`, so a failure's message carries
    the real reason.
  - When ffprobe fails, an MP4 whose index is at the end is still copied
    (video and audio copied as-is); otherwise the probe failure is the
    error.
  - It returns `Ok(None)` when the source plays as-is **or** when another task
    already holds the per-key claim (`IN_FLIGHT` + `crate::inflight::try_claim`).
    Callers must retry later.
  - It writes `play.mp4.part` and then renames it, so a crash never leaves a
    `play.mp4` that looks complete. A failed build deletes the `.part` (a
    re-encode's partial output can be gigabytes).

### Joining footage (`concat_media`)

- Needs at least 2 files (`InvalidArg("pick at least two files to join")`),
  each existing (`NotFound`), whose signatures (`join_signature`, one ffprobe
  call per file) match exactly. The signature covers only the first video
  track (codec, width×height) and the first audio track (codec, channels,
  sample rate), e.g. `h264 1920x1080 + aac 2ch 48000Hz` (`no audio` when
  there is none). A mismatch is `InvalidArg("the files don't match (… vs …):
  adding footage needs the same codec, resolution and audio format")`,
  because a stream copy of mismatched files plays broken. Unreadable ffprobe
  output is an error.
- The join is lossless (`-f concat -safe 0 … -c copy`) and keeps only the
  tracks the signature compared (`-map 0:v:0 -map 0:a:0?`). The inputs may be
  anywhere and are not changed. ffmpeg is resolved before anything is
  written.
- The output is `<data_dir>/media/joined-<Local %Y%m%d-%H%M%S>-<8 hex>.<ext>`
  (the folder is created on demand). `<ext>` is `mp4` (with `+faststart`)
  only when every input is MP4 / MOV; otherwise it is `mkv`, so the preview
  copy still fixes what an MP4 couldn't play (HEVC's `hvc1` tag, Opus or PCM
  audio). The random suffix keeps two joins in the same second apart. A
  failed join removes its partial output.

### Transcription

- `WhisperCli` passes `-l <language>` only when `language` is non-empty and
  not `"auto"`. Segments with empty text are dropped, and tokens that start
  with `[_` are skipped. Timestamps are converted from ms to seconds.
- `WhisperRs` uses greedy decoding (`best_of=1`) with token timestamps, on
  `min(available_parallelism, 8)` threads. Its progress is mapped to the
  `0.25..0.98` band so the bar never moves backwards.
- `WhisperRs` word timing: DTW alignment is on for known models
  (`dtw_preset`, by file name: large-v3-turbo, large-v3, small, base, …), and
  `tokens_to_words` joins tokens into words: a token with a leading space
  starts a word, others (word pieces, contractions, punctuation) join it.
  A word starts at its first token's DTW time (raw `t0` without DTW) and ends
  where the next word starts; `Editor::transcribe` then trims words to the
  audio (`detect::trim_words_to_audio`). For English it sets
  `FILLER_PROMPT` as the initial prompt so whisper writes fillers down.
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

### Known hazards (from reading the code; not covered by tests)

- `render_mp4` uses `[k:a]` without checking that the source has an audio
  stream.

## Side Effects

- **Process spawns.** All are `tokio::process` unless noted.
  - `video.rs`: `ffprobe` (probe, container and codecs for the playable copy,
    stream signatures for joins); `ffmpeg` (render, 16 kHz WAV extract, frame
    grab, thumbnails, peaks decode to `pipe:1`, playable copy,
    `-f concat -safe 0 … -c copy` joins).
  - `transcribe.rs`: `whisper-cli`; `which whisper-cli` (blocking
    `std::process`, also run inside every `capabilities::probe()`).
- **Filesystem writes.**
  - `<data_dir>/cache/<key>/`: `peaks.bin`, `peaks_raw.bin`, `thumb_%04d.jpg`,
    `play.mp4.part` renamed to `play.mp4`.
  - Caller-supplied `out_path` (MP4, WAV, PNG). Existing files are overwritten
    with `-y`.
  - `<wav_path>.transcript.json`, written by `whisper-cli` and never cleaned up.
  - `<data_dir>/media/joined-<timestamp>-<8 hex>.mp4` or `.mkv`
    (`concat_media`; the folder is created on demand). Nothing in the core
    deletes them.
  - A temp concat list (`roughcut-concat-<uuid>.txt`) in
    `std::env::temp_dir()`, removed after the join.
- **Network.** HTTP to the configured local or remote endpoint, and to
  Anthropic in Hosted mode only:
  - `POST {endpoint}/chat/completions`, with an optional `Authorization: Bearer`.
  - `POST {endpoint}/embeddings`.
  - `GET {endpoint}/models`.
  - `anthropic.rs`, only when the user turns on Hosted mode:
    `POST https://api.anthropic.com/v1/messages` and
    `GET https://api.anthropic.com/v1/models/{model}`. Embeddings never go
    there; they always use `{endpoint}/embeddings`.
- **Environment reads.** `FABLE_DEMO` (via `capabilities`), `FFMPEG_PATH`,
  `FFPROBE_PATH`, `PATH`, `WHISPER_MODEL`, `HOME` (via `dirs`, for
  `crate::store::data_dir()`), and `TMPDIR` (via
  `std::env::temp_dir()`, for the concat list).
- **Global state.** `video.rs`: the playable copy's `IN_FLIGHT` set (one
  build per cache key at a time) and `playable_failures()` (cache key → the
  reason the copy failed in this run; memory only, never cleared during a
  run).
- **Events.** `crate::events::send` emits `CoreEvent::progress` with
  `ProgressTask::Export` (render) and `ProgressTask::Transcribe` (`WhisperRs`,
  `MockTranscriber`).
- **OS frameworks (macOS).** Apple Vision through `objc2-vision`:
  `VNDetectFaceRectanglesRequest`.

## Verification

- Run (from the workspace root `/Users/paul/fable`): `cargo test -p roughcut-core`
- This module's unit tests only: `cargo test -p roughcut-core --lib adapters::`
  (pure tests in `embed.rs`, `face.rs`, and `video.rs` (`compute_peaks`,
  `render_plan`, and `playable_tests`: the playable-copy decision, its
  ffmpeg arguments, the ffprobe parsing and `join_signature`)). `tests/e2e.rs` exercises `MockVideoEngine`,
  `MockTranscriber` and `MockEmbedder` offline.
- Against real ffmpeg: `cargo run -p roughcut-core --example playable_check`
  (playable copies of `.mkv` / `.webm` test clips, `.mp4` and `.mkv` joins;
  prints `PLAYABLE-OK`).
- `WhisperRs` compiles only with `--features whisper-native`, which the default
  test run leaves out.
