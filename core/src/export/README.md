# export

## Overview

`crate::export` (declared `pub mod export;` in `core/src/lib.rs`) is the
hand-off layer of `roughcut-core`. It turns a project's **current cut** into
files that other tools read:

- NLE interchange: FCP7 xmeml (Premiere Pro and DaVinci Resolve), FCPXML (Final
  Cut Pro), CMX3600 EDL, and OpenTimelineIO JSON.
- SRT captions and a plain-text transcript.
- An MP4 render.

Each format has its own module with one `write` function. Each `write` is a pure
function: it takes a `Project` and either its `Media` or its `Transcript`, and
returns the file content. `mod.rs` maps target names to writers. It also adds
`Editor` methods that snapshot a project and then either write the file or pass
MP4 rendering to the video adapter.

Callers outside this module:

- The `export`, `export_clip` and `export_clips` tool handlers in
  `core/src/tools.rs`. These handlers ask the user to approve writes that an
  external MCP client requests (`export_clips` asks once for the batch).
  This module does no approval check of its own.
- The `generate_captions` tool handler in `core/src/tools.rs`, which calls
  `export::srt::write` directly on `Editor::snapshot` output and bypasses
  `render_text_target` (see Invariants).
- `app/src-tauri/src/lib.rs`, which calls `Editor::export(pid, "resolve_xml", ..)`.

## Interface

### Routing and helpers (`mod.rs`)

| Symbol | Signature | Behavior |
| --- | --- | --- |
| `xml_escape` | `pub fn xml_escape(s: &str) -> String` | Replaces `&`, `<`, `>`, `"`, `'` with `&amp;`, `&lt;`, `&gt;`, `&quot;`, `&apos;`, in that order (`&` first). |
| `render_text_target` | `pub fn render_text_target(project: &Project, transcript: Option<&Transcript>, target: &str) -> Result<String>` | Renders any target except `mp4` to a string (see the target table). |
| `default_extension` | `pub fn default_extension(target: &str) -> &'static str` | File extension for a target. Never fails: unknown targets map to `"txt"`. |
| `Editor::export` | `pub async fn export(&self, project_id: Uuid, target: &str, out_path: &str) -> Result<String>` | `snapshot`, then the private `write_export(.., None)`. Returns the written path after `~/` expansion. |
| `Editor::export_clip` | `pub async fn export_clip(&self, project_id: Uuid, clip_id: Uuid, target: &str, out_path: &str) -> Result<String>` | Exports one saved clip by running the same writers on the clip's timeline (see Invariants). Returns the written path after `~/` expansion. |
| `Editor::export_clips` | `pub async fn export_clips(&self, project_id: Uuid, clip_ids: &[Uuid], out_dir: &str, target: &str) -> Result<Vec<ClipExportResult>>` | Exports several saved clips into `out_dir` (created; `~/` expands), one at a time, as `NN-<slug>.<ext>` (`batch_file_name`), never overwriting (`reserve_path` creates each name empty with `create_new`, adding `-2`, `-3`, … when taken; a failed clip's file is removed). An unknown project, an empty id list or an unknown target fails before any folder is made. A failed clip becomes a result with `error`; the rest still run. Progress: a `BatchSink` turns each render's `export` events into one `export_clips` task at `(i + f) / n`, drops the per-clip "render complete", and the batch ends with "exported k of n". |
| `Editor::snapshot` | `pub fn snapshot(&self, project_id: Uuid) -> Result<(Project, Option<Transcript>)>` | Synchronous. Returns clones of the project and its transcript, for callers that only read. |
| `pub mod edl`, `fcpxml`, `otio`, `srt`, `txt`, `xmeml` | | The format writers listed below. |

Private helpers:

- `require_media`: returns `project.media` or an error.
- `expand_tilde`: expands a leading `~/`.
- `Editor::write_export` (async): the shared write path.
- `fcpxml::rt`: formats a rational time.
- `srt::srt_time`: formats an SRT timestamp.

### Targets

| `target` | Writer | Requires | `default_extension` |
| --- | --- | --- | --- |
| `premiere_xml`, `resolve_xml` | `xmeml::write` | media | `xml` |
| `fcp_xml` | `fcpxml::write` | media | `xml` |
| `edl` | `edl::write` | media | `edl` |
| `otio` | `otio::write` | media | `otio` |
| `srt` | `srt::write` | media and transcript | `srt` |
| `txt` | `txt::write` | media and transcript | `txt` |
| `mp4` | `VideoEngine::render_mp4`, called by `Editor::write_export`. `render_text_target` does not handle it. | media | `mp4` |
| anything else | none: `CoreError::InvalidArg("unknown export target {other}")` | n/a | `txt` |

### Format writers

| Module | Signature | Output |
| --- | --- | --- |
| `xmeml` | `pub fn write(project: &Project, media: &Media) -> String` | FCP7 xmeml (`<xmeml version="4">`): a camera video track and one audio track. If `project.screen_media` is set and at least one screen clipitem is written, a screen video track is added before the camera track. |
| `fcpxml` | `pub fn write(project: &Project, media: &Media) -> String` | FCPXML (`<fcpxml version="1.9">`) with one format `f1` (`FFVideoFormatRateUndefined`), one asset `a1`, and one `asset-clip` per included clip in the `spine`. |
| `edl` | `pub fn write(project: &Project, media: &Media) -> String` | CMX3600 EDL (`TITLE:` / `FCM: NON-DROP FRAME`) with one `AX` / `AA/V` / `C` (cut) event per included clip, each followed by a `* FROM CLIP NAME:` line. |
| `otio` | `pub fn write(project: &Project, media: &Media) -> Result<String>` | Pretty-printed OTIO JSON: `Timeline.1` → `Stack.1` → one `Track.1` (`kind: "Video"`, `name: "V1"`) → one `Clip.1` per included clip, each with a `TimeRange.1` / `RationalTime.1` source range and an `ExternalReference.1`. |
| `srt` | `pub fn write(project: &Project, transcript: &Transcript) -> String` | SRT cues for the current cut, with times mapped to output time. |
| `txt` | `pub fn write(project: &Project, transcript: &Transcript) -> String` | Plain-text transcript of the current cut, with no timecodes. Paragraphs are separated by a blank line. |

Both XML writers start with `<?xml version="1.0" encoding="UTF-8"?>`.

There are no subdirectories.

## Invariants

### Routing and errors

- `render_text_target` calls `require_media` **before** it looks at `target`.
  So a project without media fails with
  `CoreError::InvalidArg("project has no media to export")` for every target,
  including `srt`, `txt`, and unknown targets. The `mp4` path in
  `write_export` calls `require_media` too.
- With no transcript, `srt` and `txt` fail with
  `CoreError::InvalidArg("no transcript; transcribe first")`.
- The `generate_captions` tool handler (`core/src/tools.rs`) does not use
  `render_text_target`. It skips `require_media`, so it returns SRT for a
  project with no media. With no transcript it fails with
  `CoreError::InvalidArg("no transcript")`.
- `render_text_target` does not handle `mp4`, so passing `mp4` to it directly
  returns the unknown-target error. Only `Editor::write_export` handles `mp4`.
- `default_extension` and `render_text_target` handle unknown targets
  differently: `default_extension` returns `"txt"`, while
  `render_text_target` returns an error.
- `otio::write` returns `Result` only because of `serde_json`. A
  `serde_json::Error` becomes `CoreError::InvalidArg`
  (`impl From<serde_json::Error> for CoreError` in `core/src/error.rs`).
- None of the files in this directory call `unwrap()`, `expect()`, or
  `panic!`.

### Cut selection and timing (all writers)

- The writers read the cut only through the timeline's included clips:
  - The NLE writers iterate `Timeline::included_clips()`, which keeps
    `timeline.clips` in `Vec` order and filters on `included`
    (`core/src/model.rs`). Excluded clips never appear.
  - Clips are placed back to back from record time 0, with no gaps or
    transitions.
- Frame conversion is `time::to_frame(seconds, fps) = round(seconds * fps)`
  (`core/src/time.rs`).
- The writers count clip length in frames in two ways, which can differ by one
  frame:
  - **EDL record times and xmeml clip record times** accumulate
    `to_frame(source_out) - to_frame(source_in)`. EDL source and record
    durations therefore match exactly; the code comment notes that importers
    reject events where they don't.
  - **FCPXML `offset`/`duration`, OTIO `duration`, and the xmeml sequence
    `<duration>`** use `to_frame(clip.duration(), fps)`. As a result, the xmeml
    `<duration>` can differ by one frame from where the last clip ends.
- No text writer applies `project.audio_offset_s`. Only the `mp4` `RenderJob`
  passes it on.

### Per-format rules

- **EDL**:
  - Uses an integer rate, `fps_i = media.frame_rate.round().max(1.0)`, so
    29.97 is written as 30 non-drop.
  - Timecodes are `HH:MM:SS:FF` from `time::timecode_frames`.
  - Each event is two lines and a blank line:
    `"{:03}  AX       AA/V  C        {src_in} {src_out} {rec_in} {rec_out}"`,
    then `"* FROM CLIP NAME: {name}"`.
  - Events are numbered `{:03}` starting at `001`.
  - The clip name on the `* FROM CLIP NAME:` line is the file name of
    `media.file_path`, or the full path if it has no file name.
  - `project.name` is written to `TITLE:` without escaping.
- **FCPXML**:
  - The rate is rational: `(round(fps * 1001), 1001)` when
    `|fps - round(fps)| > 0.001`, otherwise `(round(fps), 1)`.
  - `frameDuration` is `"{den}/{num}s"`. The asset `duration` and each
    `asset-clip`'s `offset`, `start`, and `duration` are
    `"{frames * den}/{num}s"`, so they land on frame boundaries. The asset
    `start` is the literal `"0s"`.
  - `project.name` goes through `xml_escape` and is used for the asset, event,
    project, and every `asset-clip`.
  - The media `src` is `file://` plus the path with only spaces changed to
    `%20`. It is **not** XML-escaped.
- **xmeml**:
  - `timebase = round(fps)`. `<ntsc>` is `TRUE` when
    `|fps - round(fps)| > 0.001`, otherwise `FALSE`. There is no drop-frame
    element.
  - Names and `pathurl` go through `xml_escape`. `pathurl` is `file://` plus
    the path with spaces changed to `%20`.
  - Only the first clipitem (`i == 0`) carries the full file definitions:
    `file-1` for the camera (with `<channelcount>2</channelcount>`) and
    `file-2` for the screen. Later clipitems reference them with
    `<file id=".."/>`.
  - Clipitem ids are `clipitem-v{n}` (camera), `clipitem-a{n}` (audio), and
    `clipitem-s{n}` (screen), with `n` starting at 1.
- **xmeml with `screen_media` (a screen file that starts with the camera
  file)**:
  - Screen clipitems use the camera's record and source frames and the
    camera's fps.
  - Each screen clip's out point is clamped to
    `to_frame(scr.duration, fps)`. A screen clipitem is written only if
    `in_f < s_total`.
  - The screen track comes first (V1, underneath), and the camera track
    follows. The screen `<track>` is emitted only when at least one screen
    clipitem was written; if every clip starts at or past the screen file's
    end, there is no screen track even though `screen_media` is set.
  - The sequence width and height come from `screen_media` when it is set.
  - No transforms are baked in.
  - The other writers ignore `screen_media`.
- **OTIO**:
  - `rate` is the raw `media.frame_rate` as `f64`.
  - Clip names are `"{project.name} clip {i+1}"`.
  - `target_url` is `file://{file_path}`, with no percent-encoding.
- **Frame rate of 0**: only EDL clamps the rate (`.max(1.0)`). FCPXML, xmeml,
  and OTIO do not guard against `frame_rate == 0`.

### Transcript writers (`srt`, `txt`)

- A transcript segment is in the cut when its midpoint `(start + end) / 2`
  falls in `[source_in, source_out)` of some clip with `included == true`.
  Both writers apply this test to `timeline.clips`.
- **SRT**:
  - Skips a segment when `is_silence` is true, when `text.is_empty()` (the text
    is not trimmed, so whitespace-only text is still written), when it is not
    in the cut, or when its mapped end is at or before its mapped start.
  - Cue times are `Timeline::source_to_output(seg.start.max(0.0))` and
    `Timeline::source_to_output(seg.end)`. `source_to_output` skips excluded
    regions (`core/src/model.rs`).
  - Cues are numbered from 1, and the number increases only when a cue is
    written.
  - Timestamps are `HH:MM:SS,mmm`, rounded to the millisecond.
  - Each cue is `"{n}\n{start} --> {end}\n{text}\n\n"`.
- **TXT**:
  - Uses `seg.text.trim()`.
  - A silence, a segment that is empty after trimming, or a segment outside
    the cut ends the current paragraph.
  - Segments within a paragraph are joined with a single space, and
    paragraphs with `"\n\n"`.
  - A trailing `"\n"` is always added, so an empty cut produces `"\n"`.

### `Editor` methods

- `export_clip` gets the clip from `Editor::get_clip`, which returns
  `CoreError::NotFound("clip {clip_id}")` if the clip is missing. It then
  changes its snapshot of the project before writing:
  - The name becomes `format!("{} — {}", project.name, clip.title)`, with an
    em dash. Only the NLE writers (`xmeml`, `fcpxml`, `edl`, `otio`) read
    `project.name`, so only those targets show it. `srt`, `txt`, and `mp4`
    output is unaffected (`RenderJob` has no name field).
  - `project.timeline` is replaced with `clip.timeline`.
  - The MP4 render gets `clip.piece_spans()` (one input per piece); the main
    `export` gets `[timeline.play_spans()]` (one piece).
  - `vertical_crop` is `Some(clip.framing())` only when
    `clip.format == ClipFormat::Vertical`, and only the `mp4` render uses it
    (to render 9:16). The NLE writers keep the source frame: a short arrives
    in Premiere / Final Cut / Resolve as a 16:9 cut, unframed.
- `expand_tilde` changes a bare `~` or a leading `~/`, which it replaces with
  `dirs::home_dir()` (joined with the rest of the path). A `~user` path, or a
  system with no home directory, leaves the path unchanged.
- `snapshot` returns owned clones. The project and the transcript are read
  under separate lock acquisitions, so together they are not an atomic
  snapshot.

## Side Effects

- **Pure, with no I/O, locking, or environment access**:
  - `xml_escape`, `render_text_target`, `default_extension`.
  - Every format writer: `xmeml::write`, `fcpxml::write`, `edl::write`,
    `otio::write`, `srt::write`, `txt::write`.
- **Filesystem writes** (`Editor::write_export`, text targets):
  - Calls `std::fs::create_dir_all(parent)` when the parent path is not empty.
  - Then calls `std::fs::write(out_path, content)`, which creates the file or
    silently overwrites an existing one.
  - Both are blocking `std::fs` calls inside an `async fn`.
  - I/O errors become `CoreError::Io`.
- **MP4 render** (`Editor::write_export`, `target == "mp4"`):
  - Builds `crate::adapters::video::RenderJob` and awaits
    `self.video().render_mp4(&job, sink)`, where `sink` is the Editor's own,
    or `export_clips`' `BatchSink` wrapping it.
  - This module does not create the parent directory for this path.
  - The rendering itself happens in the `VideoEngine` implementations in
    `core/src/adapters/video.rs` (`FfmpegCli`, `MockVideoEngine`,
    `AutoVideoEngine`). `FfmpegCli` spawns `ffmpeg`, found through
    `resolve_binary("ffmpeg", "FFMPEG_PATH")`. The `VideoEngine::render_mp4`
    doc says it emits `progress` events (task `export`) to the event sink.
- **Locking and lazy loading**:
  - `snapshot` calls `Editor::with_project` and `Editor::get_transcript`, and
    `export_clip` calls `Editor::get_clip`. All of them lock the `Editor`'s
    `inner.state` `std::sync::Mutex` (`core/src/engine/mod.rs`).
  - On a cache miss, `ensure_loaded` loads the project, its transcript, and the
    `journal:{id}` undo/redo entry from the store into memory.
  - The engine's `lock().unwrap()` calls panic if that mutex is poisoned.
- **Home directory lookup**: `expand_tilde` calls `dirs::home_dir()`. This
  module reads no environment variables directly.

## Verification

- Run: `cargo test -p roughcut-core`
- There are no unit tests inside `src/export`. These end-to-end tests in
  `core/tests/e2e.rs` cover it:
  - `poc_loop_through_the_tool_registry`: the `export` tool for
    `premiere_xml`, `fcp_xml`, `edl`, `otio`, `srt`, and `txt`.
  - `clips_find_save_export`: `export_clip` for `edl`, `srt`, and `txt`.
  - `clips_from_several_pieces_in_either_format`: `export_clip` writing an EDL
    for a two-piece clip. It asserts at least two event lines (lines starting
    `00`), not exactly one event per piece.
- Run only those tests:
  `cargo test -p roughcut-core --test e2e -- poc_loop_through_the_tool_registry clips_find_save_export clips_from_several_pieces_in_either_format`
