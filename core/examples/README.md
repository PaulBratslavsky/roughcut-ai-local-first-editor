# roughcut-core examples

## Overview

This directory holds standalone Cargo example binaries for the `roughcut-core`
crate. They are manual smoke tests and diagnostic tools: they drive the real
library API against real hardware, local services, or media files, then print
results. Most of them end by printing a sentinel line (for example `SMOKE-OK`)
after their `assert!`s pass. None of them are run by `cargo test`. Each one
is a separate binary with its own `#[tokio::main] async fn main()`, and the
crate's `Cargo.toml` discovers them automatically (there are no `[[example]]`
entries).

Run any of them from the workspace root (`/Users/paul/fable`):

```sh
cargo run -p roughcut-core --example <name> [-- args]
```

Most examples need macOS plus external tooling (ffmpeg/ffprobe, camera,
microphone, display, a local model server, or network access). The
**Requires** column below lists what each one needs.

## Interface

Every file exposes only `main`. The table lists the `roughcut_core` API each
one exercises.

| Example | Purpose | `roughcut_core` API exercised | Args / inputs | Requires | Pass signal |
| --- | --- | --- | --- | --- | --- |
| `agent_live` | Runs the agent loop on demo fixture footage and asks it to cut the "hiking" tangent | `Editor::new(.., demo=true)` with `SqliteStore::open_in_memory`, `MockVideoEngine`, `MockTranscriber`; `tools::dispatch` (`Caller::Ui`): `create_project`, `transcribe`, `get_timeline`, `apply_instruction`, `get_transcript`; custom `StdoutSink: EventSink` (prints `agent-step` events) | none (`file_path` `/demo/footage.mp4` is a fixture path) | Ollama or llama-server for the real LLM path. The in-memory store has no saved preferences, so `Preferences::default()` supplies the endpoint from `INFERENCE_ENDPOINT` (default `http://localhost:11434/v1`) and the model from `INFERENCE_MODEL` (default `gemma4:26b`). If the server is unhealthy, or the step-0 model call fails, `agent::run_instruction` falls back to the offline path | Informational only: prints `hiking tangent still included? <bool>`, no assert |
| `crash_smoke` | Kills the capture process with SIGKILL mid-take and checks that the orphaned take still probes | `record::list_devices`, `record::start_camera`, `record::status`, `FfmpegCli.probe` (`VideoEngine`) | none | macOS avfoundation, ffmpeg/ffprobe, camera, mic, `pkill` | `CRASH-SMOKE-OK` |
| `dual_smoke` | Camera+mic **and** screen captured in one session, giving two stitched files | `record::start_capture_full(&sink, cam, mic, false, Some(screen))`, `record::stop` → `RecordingOutcome { path, screen_path }`, `FfmpegCli.probe` | none | macOS, ffmpeg/ffprobe, camera, mic, display, Screen Recording permission | `DUAL-SMOKE-OK` |
| `face_smoke` | Finds faces with Apple Vision on sampled frames and computes the 9:16 crop. Then repeats through the tool registry | `FfmpegCli.probe` / `extract_frame`; `face::sample_times`, `face::png_size`, `face::detect_faces`, `face::crop_x_for_faces`; `Editor::new(.., AutoVideoEngine, MockTranscriber, NullSink, false)`; tools `create_project`, `create_clip` (`format: "vertical"`), `list_clips`, `center_clip_on_face` | `face_smoke <video> [start_s] [end_s]` (end defaults to `min(duration, start+30)`) | macOS for detection, ffmpeg/ffprobe, dev-dep `tempfile`. Only the Vision backend in `face.rs` is `cfg(target_os = "macos")`; elsewhere the example compiles, `detect_faces` returns `Unavailable`, and it panics at `expect("detect")` | Informational only: prints faces and `crop_x`. Panics (`expect`) on any failure, including `expect("frame size")` when `end <= start` (no frames sampled) |
| `llm_install` | Installs the llama.cpp `llama-server` sidecar | `llm_runtime::install_llama_server(&sink)` | none | Network (GitHub API), `tar`, macOS release asset | Prints `installed: <path>`, also when it was already installed; on error prints `ERR <e>` to stderr and calls `process::exit(1)` |
| `llm_status` | Prints the setup/provisioning status as pretty JSON (same shape the setup screen fetches) | `SqliteStore::open(&SqliteStore::default_path())`, `Editor::bootstrap_with_store`, `setup::status` → `SetupStatus` | none | Optional local inference server (probed) | Informational only |
| `record_smoke` | Records N seconds from camera and mic, stops cleanly, and probes the result | `record::list_devices`, `record::start_camera`, `record::status` (`recording`, `elapsed_s`), `record::stop`, `FfmpegCli.probe` | `record_smoke [secs=3] [cam] [mic]`. If cam/mic are not given, it picks the device whose name contains `MacBook`, else panics | macOS avfoundation, ffmpeg/ffprobe, camera, mic | `SMOKE-OK` |
| `render_declick` | Runs the full pipeline (transcribe → rough cut → MP4 export) on a fixed source file | `Editor::bootstrap_with_store` (in-memory store), `require_confirmations_for_tests(false)`; tools `create_project`, `transcribe`, `generate_rough_cut`, `export` (`target: "mp4"`) | Hard-coded input `/tmp/nle-test-source.mp4` | ffmpeg/ffprobe and a working transcriber (`AutoTranscriber`) | Informational only: prints `rendered: <path>`. Panics (`unwrap`) on any tool error |
| `render_progress` | Checks that export progress events stream during an MP4 render | Same pipeline as `render_declick`, plus custom `CountSink: EventSink` that collects `progress` events with `task == "export"` | Hard-coded input `/tmp/nle-test-source.mp4` | Same as `render_declick` | `PASS` |
| `screen_smoke` | Records the screen and mic for 4 s, then probes the result | `record::start_capture(&sink, screen, mic, true)`, `record::stop`, `FfmpegCli.probe` | none | macOS, ffmpeg/ffprobe, display, mic, Screen Recording permission | `SCREEN-SMOKE-OK` |
| `takes_smoke` | Multi-take flow: start → pause → resume → stop, ending in one concatenated file | `record::start_camera`, `record::pause` / `record::resume` → `RecordingStatus { paused, recording, take, total_s }`, `record::stop`, `record::list_recordings` → `RecordingFile { name, duration_s }`, `FfmpegCli.probe` | none | macOS avfoundation, ffmpeg/ffprobe, camera, mic | `TAKES-SMOKE-OK` |

Device selection: when no index is supplied, the capture examples pick the
first camera or mic whose name contains `"MacBook"`. `dual_smoke` matches the
camera named exactly `"MacBook Pro Camera"`. If no camera or mic name
matches, they fall back to index `0`. `record_smoke` is the exception: it
panics instead of falling back.

Display selection: `screen_smoke` and `dual_smoke` take the first entry of
`record::list_devices().screens` and panic (`expect`) if there is none. There
is no index fallback for displays.

## Invariants

These are the behaviors the examples assert. If one of these assertions
starts failing after a library change, treat it as a regression.

- `crash_smoke`: after the capture child is killed, `record::status().recording`
  must be `false` ("status must notice the dead child"). The orphaned take must
  still probe, with `duration > 0.5`.
- `dual_smoke`: a dual session must return `RecordingOutcome.screen_path`
  (`Some`). Both files must be longer than 1.0 s, and their durations must be
  within 1.0 s of each other ("streams should be near-synced").
- `record_smoke`: the probed duration must exceed `max(secs - 2.5, 0.5)`. The
  2.5 s margin allows for avfoundation warm-up, which takes about 2 s.
- `render_progress`: at least 3 export progress messages are emitted, and the
  last one is exactly `"render complete"` (emitted in
  `core/src/adapters/video.rs`).
- `screen_smoke`: the screen recording must be longer than 1.0 s and wider
  than 800 px.
- `takes_smoke`: after `pause`, `paused && take == 1`. After `resume`,
  `recording && take == 2`. `stop` returns the concatenated file, not a take
  (the path does not contain `-take`), and a camera-only session has no
  `screen_path`. The stitched file is longer than 2.0 s and shows up in
  `record::list_recordings`. The status `take` is a counter, not the file
  number: the resumed take is written as `-take3.mp4` (see Side Effects).
- The tool calls in `agent_live`, `face_smoke`, `render_declick`, and
  `render_progress` go through `tools::dispatch(.., Caller::Ui)`, the same
  registry the UI uses. Tool names and argument keys must stay in sync with the
  registry.
- `render_declick` and `render_progress` call
  `editor.require_confirmations_for_tests(false)`, so nothing in them can block
  waiting for a UI confirmation. In `core/src/tools.rs` the `export`
  confirmation is only requested for `ActionSource::McpClient`. These examples
  dispatch as `Caller::Ui`, so for them the call is a safeguard rather than a
  requirement.
- Developer note: the header comment in `dual_smoke.rs` mentions
  "pause/resume", but the code only does start → 5 s → stop.

## Side Effects

- **Recordings library**: `record::start_camera`, `start_capture`, and
  `start_capture_full` create `~/Movies/RoughCut` and refuse to start with
  less than 2 GB free (checked with `df -Pk`). They spawn an ffmpeg
  avfoundation capture that writes `<kind>-YYYYmmdd-HHMMSS-take<N>.mp4`, where
  `<kind>` is `recording`, `screen`, or `presentation` (dual sessions also
  write `-screen-take<N>.mp4`). `record::resume` numbers the new take
  `takes.len() + 2`, so the take after one pause is `-take3.mp4`: `takes_smoke`
  leaves `<base>-take1.mp4` and `<base>-take3.mp4` (no `-take2`).
  `record::stop` concatenates the takes into `<base>.mp4` (and
  `<base>-screen.mp4`) and keeps the take files. These files are **not**
  cleaned up by the examples (`core/src/adapters/record.rs`).
- **`crash_smoke` kills processes system-wide**: it runs
  `pkill -9 -f avfoundation`, which kills **every** process whose command line
  matches `avfoundation`, not only this example's child. It never calls
  `record::stop`, so an orphaned `recording-*-take1.mp4` stays in
  `~/Movies/RoughCut`.
- **macOS privacy prompts**: the capture examples can trigger
  camera, microphone, and Screen Recording permission prompts.
- **`llm_status` touches the real app database**: it opens (or creates)
  `<dirs::data_dir()>/roughcut/library.db` via `SqliteStore::default_path()`.
  `Editor::bootstrap_with_store` then calls `purge_trash(30)`, which
  **permanently deletes** projects trashed more than 30 days ago. `setup::status`
  sends `GET {inference_endpoint}/models` with a 2 s timeout.
- **`llm_install` network and disk**: it fetches
  `https://api.github.com/repos/ggml-org/llama.cpp/releases/latest` and
  downloads the release archive to
  `<dirs::data_dir()>/roughcut/bin/llama-server.tar.gz`. It extracts into
  `bin/llama-extract/`, moves the files next to `llama-server` (the binary
  plus its dylibs) into `bin/`, deletes the archive and the extract folder,
  and sets `llama-server` to mode `0755`. If `tar` fails or the archive has
  no `llama-server`, the archive and extract folder are left behind
  (`core/src/llm_runtime.rs`). If `llama-server` is already installed, it
  makes no changes.
- **`render_declick` / `render_progress`**: they read
  `/tmp/nle-test-source.mp4` and write `/tmp/declick-test.mp4` or
  `/tmp/progress-test.mp4`. Both use an in-memory SQLite store. Outside demo
  mode, `Editor::transcribe` writes `roughcut-<media id>.wav` in
  `std::env::temp_dir()` and removes it after transcription. It then starts a
  background task: `clean_transcript` sends `GET {endpoint}/models` and, if
  the server is healthy, chat requests; `index_transcript` POSTs to
  `{endpoint}/embeddings`. The endpoint is `INFERENCE_ENDPOINT` (default
  `http://localhost:11434/v1`), because the in-memory store uses
  `Preferences::default()` (`core/src/engine/media.rs`).
- **`face_smoke`**: it writes between 6 and 12 PNG frames (one per ~2 s of
  the span, `f00.png` upward) into a `tempfile::tempdir()` that is removed
  automatically. The clip face passes
  (one in the background on `create_clip`, plus the explicit
  `center_clip_on_face`) write frames under
  `<data_dir>/roughcut/cache/faces/<uuid>/` and delete them afterwards
  (`core/src/engine/clips.rs`). Uses an in-memory store. Sleeps 8 s.
- **`agent_live`**: uses an in-memory store and mock engines. It sends HTTP
  requests to `INFERENCE_ENDPOINT` (default `http://localhost:11434/v1`)
  whether or not the server is healthy: `apply_instruction` always sends the
  `GET {endpoint}/models` health probe (2 s timeout), and `transcribe` starts
  a background `index_transcript` that POSTs to `{endpoint}/embeddings` with
  `EMBEDDING_MODEL` (default `nomic-embed-text`). Embedding failures are
  ignored. Chat requests follow only when the probe succeeds.
- Everything else is stdout/stderr output.

## Verification

- Check that all examples still compile. `cargo test` builds examples by
  default but does not run them:
  `cargo test -p roughcut-core`
- Run an individual smoke test manually on suitable hardware:
  `cargo run -p roughcut-core --example <name>`
