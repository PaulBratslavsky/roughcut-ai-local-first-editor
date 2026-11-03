# roughcut-core examples

## Overview

This directory holds standalone Cargo example binaries for the `roughcut-core`
crate. They are manual smoke tests and diagnostic tools: they drive the real
library API against real hardware, local services, or media files, then print
results. Several of them end by printing a sentinel line (for example
`PLAYABLE-OK`) after their `assert!`s pass. None of them are run by
`cargo test`. Each one is a separate binary with its own
`#[tokio::main] async fn main()`, and the crate's `Cargo.toml` discovers them
automatically (there are no `[[example]]` entries).

Run any of them from the workspace root (`/Users/paul/fable`):

```sh
cargo run -p roughcut-core --example <name> [-- args]
```

Most examples need external tooling (ffmpeg/ffprobe, a local model server,
or network access), and some need macOS. The **Requires** column below lists
what each one needs.

## Interface

Every file exposes only `main`. The table lists the `roughcut_core` API each
one exercises.

| Example | Purpose | `roughcut_core` API exercised | Args / inputs | Requires | Pass signal |
| --- | --- | --- | --- | --- | --- |
| `agent_live` | Runs the agent loop on demo fixture footage and asks it to cut the "hiking" tangent | `Editor::new(.., demo=true)` with `SqliteStore::open_in_memory`, `MockVideoEngine`, `MockTranscriber`; `tools::dispatch` (`Caller::Ui`): `create_project`, `transcribe`, `get_timeline`, `apply_instruction`, `get_transcript`; custom `StdoutSink: EventSink` (prints `agent-step` events) | none (`file_path` `/demo/footage.mp4` is a fixture path) | Ollama or llama-server for the real LLM path. The in-memory store has no saved preferences, so `Preferences::default()` supplies the endpoint from `INFERENCE_ENDPOINT` (default `http://localhost:11434/v1`) and the model from `INFERENCE_MODEL` (default `gemma4:26b`). If the server is unhealthy, or the step-0 model call fails, `agent::run_instruction` falls back to the offline path | Informational only: prints `hiking tangent still included? <bool>`, no assert |
| `face_smoke` | Finds faces with Apple Vision on sampled frames and computes the 9:16 crop. Then repeats through the tool registry | `FfmpegCli.probe` / `extract_frame`; `face::sample_times`, `face::png_size`, `face::detect_faces`, `face::crop_x_for_faces`; `Editor::new(.., AutoVideoEngine, MockTranscriber, NullSink, false)`; tools `create_project`, `create_clip` (`format: "vertical"`), `list_clips`, `center_clip_on_face` | `face_smoke <video> [start_s] [end_s]` (end defaults to `min(duration, start+30)`) | macOS for detection, ffmpeg/ffprobe, dev-dep `tempfile`. Only the Vision backend in `face.rs` is `cfg(target_os = "macos")`; elsewhere the example compiles, `detect_faces` returns `Unavailable`, and it panics at `expect("detect")` | Informational only: prints faces and `crop_x`. Panics (`expect`) on any failure, including `expect("frame size")` when `end <= start` (no frames sampled) |
| `llm_install` | Installs the llama.cpp `llama-server` sidecar | `llm_runtime::install_llama_server(&sink)` | none | Network (GitHub API), `tar`, macOS release asset | Prints `installed: <path>`, also when it was already installed; on error prints `ERR <e>` to stderr and calls `process::exit(1)` |
| `llm_status` | Prints the setup/provisioning status as pretty JSON (same shape the setup screen fetches) | `SqliteStore::open(&SqliteStore::default_path())`, `Editor::bootstrap_with_store`, `setup::status` → `SetupStatus` | none | Optional local inference server (probed) | Informational only |
| `playable_check` | Checks the preview's playable copy and the footage join against real ffmpeg: OBS-style `.mkv` and odd-sized `.webm` test clips get a faststart MP4 copy with one stereo AAC track, a plain MP4 gets none, and `concat_media` joins MP4s into an `.mp4` and `.mkv` files into an `.mkv` in the data dir | `adapters::video::ensure_playable_copy`, `adapters::video::concat_media`, `FfmpegCli.probe` (`VideoEngine`), `store::data_dir` | none: makes 10-second `lavfi` test clips (`obs-aac.mkv` H.264 + AAC, `obs-opus.mkv` H.264 + Opus, `obs-hevc.mkv` HEVC + AAC, `obs-two-tracks.mkv` with a stereo track 1 and a 6-channel track 2, `browser-odd.webm` VP9 + Opus at 641×361, faststart `plain.mp4` and `plain-2.mp4`) under a throwaway `HOME` in the temp dir | ffmpeg/ffprobe with `libx264`, `libx265`, `libopus` and `libvpx-vp9` | `PLAYABLE-OK` |
| `render_declick` | Runs the full pipeline (transcribe → rough cut → MP4 export) on a fixed source file | `Editor::bootstrap_with_store` (in-memory store), `require_confirmations_for_tests(false)`; tools `create_project`, `transcribe`, `generate_rough_cut`, `export` (`target: "mp4"`) | Hard-coded input `/tmp/nle-test-source.mp4` | ffmpeg/ffprobe and a working transcriber (`AutoTranscriber`) | Informational only: prints `rendered: <path>`. Panics (`unwrap`) on any tool error |
| `render_progress` | Checks that export progress events stream during an MP4 render | Same pipeline as `render_declick`, plus custom `CountSink: EventSink` that collects `progress` events with `task == "export"` | Hard-coded input `/tmp/nle-test-source.mp4` | Same as `render_declick` | `PASS` |
| `render_seek_check` | Checks the per-piece seeking MP4 render: a short from minute 40, two pieces in reverse order, A/V offset either way, a 9:16 crop, and screen compositing (pip and screen-only) | `FfmpegCli.probe` / `render_mp4` with `adapters::video::RenderJob { pieces, .. }`, `model::PlaySpan`, `model::Layout` | none: generates a 45-minute test source and a 3-minute screen file under the temp dir (once, reused) | ffmpeg/ffprobe | `ALL PASS` (exit 1 otherwise). Checks output length, PSNR of frames at known output times against the source (and that a wrong frame scores low), that a delayed track starts without silence, and that seeking beats decoding from 0:00 (prints both times) |
| `shorts_eval` | Runs a Shorts scan on a real transcript with the local model, to compare the finder's results before and after a change | `Editor::new` with `SqliteStore::open` (a throwaway file in the temp dir), `FfmpegCli`, `MockTranscriber`; `set_secret_store_for_tests`; `Store::save_transcript`; `tools::dispatch` (`Caller::Ui`): `set_preferences`, `create_project`, `find_clips { scan: true, count: 60 }` | `<video> <transcript.json> <out.json> [model]`: the video is only probed; the transcript is a `get_transcript` result | A local model server (`INFERENCE_ENDPOINT` or the default) | Prints `<n> candidates by "llm" in <s>s` and writes the `find_clips` result to `out.json`; no assertions |

## Invariants

These are the behaviors the examples assert. If one of these assertions
starts failing after a library change, treat it as a regression.

- `playable_check`: the data dir resolves under the throwaway `HOME`. Every
  non-MP4 test clip gets a copy named `play.mp4` with even dimensions,
  exactly one audio track that is stereo AAC (for `obs-two-tracks.mkv`, the
  source's first track, not the 6-channel one), `moov` before `mdat`
  (faststart), and a length within 0.15 s of the source. H.264 stays H.264,
  HEVC stays HEVC tagged `hvc1`, and the 641×361 VP9 becomes 640×360 H.264. A
  second call returns the same cached path. A faststart MP4 gets no copy
  (`None`). Joins:
  - two 10 s MP4s → a faststart `.mp4` of 20 s (±0.2 s) under
    `<data dir>/media/`;
  - two HEVC `.mkv` files → an `.mkv` whose preview copy is tagged `hvc1`;
  - the two-track `.mkv` joined with itself keeps one (stereo) audio track,
    and joining that result with the two-track file again gives 30 s
    (±0.3 s), as two "add footage" steps in a row;
  - H.264 + HEVC is refused ("don't match") instead of joined.
- `render_progress`: at least 3 export progress messages are emitted, and the
  last one is exactly `"render complete"` (emitted in
  `core/src/adapters/video.rs`).
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

## Side Effects

- **`playable_check` uses a throwaway data dir**: it sets `HOME` to
  `<temp dir>/roughcut-playable-<pid>/` before anything reads the data dir, so
  the test clips, the copies (`<data dir>/cache/<key>/play.mp4`) and the joined
  files (`<data dir>/media/joined-<timestamp>-<8 hex>.mp4` / `.mkv`) all land
  there, and the real data dir is untouched. The folder is removed at the end; a failed
  assert leaves it behind. ffmpeg's concat list is a `roughcut-concat-*.txt`
  in the temp dir, removed after each join.
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
