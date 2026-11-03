# Remove in-app recording

*   **Author:** Paul Bratslavsky
*   **Status:** Approved (not built): the owner chose the one-file option
*   **Date:** 2026-11-03 (first drafted 2026-10-31)

--------------------------------------------------------------------------------

## Overview

Take recording out of RoughCut. The owner records in OBS, where the scenes
(screen, camera, the camera bubble) are composed, and OBS saves one file;
RoughCut imports that file like any video and does the editing.

Two things come with the removal: OBS's files must play in the preview
(today a `.mkv` imports but shows no picture), and the one feature that
quietly depends on the recorder's folder (adding footage to the end of a
project) must keep working without it.

## The Problem

*   Recording is over 3,000 lines on `feat/shorts`
    (`core/src/adapters/record.rs`, 899; `RecorderPanel.tsx`, 841; twelve
    Tauri commands; five examples), plus about 1,500 lines of native capture
    on the unmerged `feat/native-capture` branch.
*   It is macOS-only (ffmpeg `avfoundation`). The product promises Mac,
    Windows and Linux; each would need its own capture code.
*   It is the most fragile part of the app: about 20 commits went into
    fixing it (OBS holding the camera, a shared camera's bad first
    timestamp, encoder back-pressure at 4K60, privacy permissions, code
    signing), and camera drivers, OBS and macOS updates can break it again.
*   It isn't the product's edge. RoughCut's goal is a powerful AI editor;
    the tools it competes with on editing (Gling, Opus Clip) don't record.
*   The owner already records in OBS.

## Goals

*   Remove in-app recording: the recorder screen, its capture code, the
    recording Tauri commands, the camera and microphone permission strings,
    the recordings library, the recording examples and docs.
*   **OBS files play.** A file OBS saves (`.mkv`, `.mp4`, `.mov`, fragmented
    or hybrid MP4) imports, plays in the preview with sound, transcribes,
    renders and exports.
*   **Nothing a user made is lost.** Projects made with the recorder,
    including dual-capture ones with the camera bubble, keep playing,
    rendering and exporting exactly as before. Recordings on disk stay where
    they are and import like any file. "Add footage to the end"
    (`append_media`) keeps working.

## Non-Goals

*   **Recording in any form**, including a hidden or "beta" recorder.
*   **Separate screen and camera files lined up by sound.** Designed in full
    (commit 81a7f11) and set aside by the owner: the scenes are composed in
    OBS, so there is one file.
*   **Removing the camera-bubble layout.** It stays, unchanged, for the
    projects that already use it (see Decisions).
*   **Choosing among OBS audio tracks.** RoughCut uses the file's main audio
    track (ffmpeg's default pick); OBS's default puts every source on track
    1.

## Decisions

*   **D0: How will you record?** **OBS, one file**, scenes composed in OBS.
    Owner (2026-11-03): "One file option is fine cause all the scenes and
    screens will come from OBS." This replaces the earlier choice of
    separate files (2026-11-01), so the second-track and sound-matching
    design is dropped.
*   **D1: Keep the camera-bubble layout?** **Yes, frozen.** Projects made
    with the recorder's dual capture rely on it (`screen_media`, `layout`,
    `set_layout`, compositing in preview, render and XML). Keeping it costs
    nothing; removing it would break those projects. The registry tool
    `attach_screen_media` stays too (it attaches a screen file assumed to
    start with the camera file, e.g. from MCP); only the Tauri shell's
    duplicate of it goes.
*   **D2: One change.** With nothing to build before the removal except the
    playback fix, both go in one change, the fix first.
*   **The unmerged recording branches**: `feat/native-capture` and
    `fix/release-entitlements` (camera / microphone entitlements, needed only
    by the recorder) are tagged `archive/native-capture` and
    `archive/release-entitlements`; ADR-0008 cites the tags; the branches
    are deleted with the owner's OK.

## Critical User Journeys (CUJs)

### CUJ 1: Edit an OBS recording

*   **Given**: `2026-11-03 10-15-02.mkv`, 25 minutes, saved by OBS with the
    screen and the camera bubble composed in, H.264 + AAC.
*   **When**: the user clicks "+ New video" (or drops the file) and picks it.
*   **Then**: it imports as any video does: the preview plays it with sound
    (RoughCut makes a playable copy in the background, as it already does
    for some MP4s), it is transcribed, and every edit, render and export
    works. The original file is untouched.

### CUJ 2: An old dual-capture project

*   **Given**: a project made with the in-app recorder (camera plus screen,
    camera bubble).
*   **When**: the user opens it after the change.
*   **Then**: it plays, renders and exports exactly as before; the layout
    options are still in the Inspector. Existing recordings in
    `~/Movies/RoughCut` are untouched.

### CUJ 3: Add footage to the end

*   **Given**: a project from an OBS `.mp4`, on a machine where the recorder
    was never used.
*   **When**: in the Footage tab the user clicks "add a video file…" and
    picks a second OBS file with the same codecs and resolution.
*   **Then**: the two are joined losslessly and the new part is transcribed,
    as today.

### CUJ 4: No recorder anywhere

*   **When**: the user looks for a way to record.
*   **Then**: there is none: no Record button, no `R` shortcut, no recorder
    entry on the import screen, no "record a new clip" in the Footage tab,
    no Recording row in Setup. macOS never asks RoughCut for camera or
    microphone access.

## Technical Plan (Architecture)

### 1. OBS files play in the preview (`core/src/adapters/video.rs`)

*   **Today**: `ensure_playable_copy` makes a faststart MP4 copy only when an
    MP4's index is at the end or its audio has more than 2 channels.
    `needs_faststart` walks MP4 boxes and returns false for anything else,
    so a `.mkv` (OBS's crash-safe default) gets no copy, and WebKit can't
    play Matroska: the preview stays black. The import picker already
    accepts `mkv`, `avi` and `webm` (`newProject.ts`, `EmptyState.tsx`).
*   **Change**: also make the copy when the container isn't MP4 / MOV (a
    quick ffprobe of `format_name`, like the existing `audio_channels`
    check; `probe` reads the format but doesn't keep its name). Video is copied
    (H.264 / HEVC: OBS's encoders); audio is copied when it is AAC and
    converted to AAC otherwise (same as the existing >2-channel path). A
    video codec MP4 can't carry (rare from OBS) is converted to H.264, with
    a progress note, since that copy takes minutes rather than seconds.
*   The copy is already built in the background on the first
    `get_media_assets` call, and `media-assets-changed` switches the preview
    to it when it lands (`engine/projects.rs`). Until then the preview plays
    the original, which for a `.mkv` shows no picture, so `PreviewPanel.tsx`
    shows "Preparing preview…" over the player when the file isn't
    `.mp4` / `.mov` / `.m4v` and there is no `playback_path` yet.
*   Everything else already works on the original file: transcription,
    render and export read it through ffmpeg; the copy is only for the
    preview.
*   Fragmented and hybrid MP4 (newer OBS options) are MP4 boxes with the
    index first; nothing changes for them.

### 2. Add footage without the recorder (`core/src/adapters/video.rs`, `engine/media.rs`)

*   `append_media` joins files with `record::combine_recordings`, which
    writes to `~/Movies/RoughCut/combined-<ts>.mp4`. Only the recorder ever
    creates that folder, so on a machine that never recorded the join fails.
*   Move the join (`combine_recordings`, its private `concat` and its codec
    check) to `video.rs` as `concat_media`, writing to `<data dir>/media/`
    (created on demand), with errors that say "footage", not "recordings".

### 3. Remove recording

*   **Core**: delete `adapters/record.rs` and its `pub mod record;` line;
    drop `ProgressTask::Recording` and the recording note in `progress.rs`;
    delete the examples `record_smoke`, `crash_smoke`, `dual_smoke`,
    `screen_smoke`, `takes_smoke` and their sections in
    `core/examples/README.md`.
*   **App shell** (`app/src-tauri/src/lib.rs`): delete the commands
    `record_devices`, `record_start`, `record_status`, `record_stop`,
    `record_pause`, `record_resume`, `list_recordings`,
    `combine_recordings`, `delete_recording`, `screen_permission_status`,
    `request_screen_permission`, `open_privacy_settings` and the shell's
    duplicate `attach_screen_media`; the recorder's exit hook. Delete
    `app/src-tauri/Info.plist` (it holds only the camera and microphone
    keys).
*   **Frontend**:
    *   delete `RecorderPanel.tsx` and the `"recorder"` screen
        (`viewStore.ts`, `App.tsx`);
    *   the top bar's Record entry and the `R` shortcut (the "+ New video"
        menu is left with one item, so it becomes a plain button); the
        import screen's record entry;
    *   the Footage tab keeps "add a video file…" and loses "record a new
        clip", the recordings library and the recorder wording in its note;
    *   Setup's Recording row;
    *   the recording wrappers in `ipc/api.ts` (`attachScreenMedia`,
        `listRecordings`, …) and their `!isTauri` fallbacks;
    *   the recorder's styles, keeping `.mic-meter`, `.playback-meter`,
        `.screen-layer`, `.layout-*` and `.footage-*`, which other panels
        use;
    *   the generated `CaptureDevice`, `CaptureDevices`, `RecordingFile`,
        `RecordingOutcome` and `RecordingStatus` files (ts-rs never deletes
        files) and a regenerated `ProgressTask.ts`;
    *   the preview's "deleted from the library?" error text and the
        `ingest.ts` comment.
*   **Signing**: `scripts/build-macos-app.sh` and the README give privacy
    grants as the reason for stable signing. The signing stays (the hosted
    mode's key store also benefits from a stable signature); the reason is
    reworded.
*   **Docs**:
    *   `docs/08-recording-plan.md` marked superseded (and its SITEMAP row);
    *   README's "Record in the app" replaced by "Recording in OBS" (one
        paragraph: compose the scenes in OBS, import the file);
    *   the recording parts of `07-architecture.md`, `tool-api.md` (the
        shell-command list and the `recording` progress task),
        `architecture-audit.md` (B18, F5), ADR-0005, the core READMEs
        (`adapters`, `engine`, `examples`) and the `roughcut-elephant`
        skill;
    *   `attach_screen_media` and `set_layout` descriptions (registry and
        `tool-api.md`): "a screen file that starts with the camera file"
        instead of "dual-capture";
    *   **ADR-0008: Remove in-app recording** (0006 was the native recorder,
        on the archived branch), citing the archive tags. `CONTEXT.md` has no
        recording entries.
*   **Kept**: `screen_media`, `layout`, `set_layout`, `attach_screen_media`
    (registry tool) and compositing in preview, render and export;
    `append_media`; every project and file made by the recorder.

## Alternatives Considered

### Alternative A: Keep recording, frozen

*   **Reason for Rejection**: the code, the permissions and the fragility
    stay, and Windows / Linux never get it. The owner chose removal.

### Alternative B: Separate OBS files, lined up by sound

*   **Pros**: the layout can be changed while editing (camera-only intro,
    screen full-frame).
*   **Cons**: about a week of work (sound matching, offsets in preview,
    render and XML, a second playable copy).
*   **Reason for Rejection**: the owner composes the scenes in OBS. Kept in
    commit 81a7f11 if it is wanted later.

### Alternative C: Ask users to set OBS to save MP4

*   **Pros**: no playback change.
*   **Cons**: MKV is OBS's crash-safe choice (an MP4 cut off by a crash is
    unreadable), and users import files they already have.
*   **Reason for Rejection**: a one-time remux is cheap and already exists
    for MP4s.

### Alternative D: Also remove the camera-bubble layout

*   **Reason for Rejection**: it would break projects made with the
    recorder, and keeping it costs nothing (D1).

## Non-Functional Requirements

*   **Speed**: the playable copy of a 25-minute OBS `.mkv` (stream copy)
    takes seconds, in the background, while transcription starts; the
    preview shows "Preparing preview…" until it is ready.
*   **Disk**: the copy roughly doubles the file's footprint in the cache,
    as for MP4s today.
*   **Privacy**: the app no longer asks for camera or microphone access.
*   **Compatibility**: projects made with the recorder render, preview and
    export exactly as before (tested).
*   **Platforms**: nothing platform-specific is added; the macOS-only
    recorder is gone.

--------------------------------------------------------------------------------

### Additional Resources

*   [`docs/08-recording-plan.md`](../08-recording-plan.md): the recorder
    this removes; `archive/native-capture`: the native recorder.
*   Code: `core/src/adapters/video.rs` (`ensure_playable_copy`,
    `needs_faststart`, `probe`), `core/src/adapters/record.rs`
    (`combine_recordings`), `core/src/engine/media.rs` (`append_media`),
    `app/src-tauri/src/lib.rs`, `frontend/src/panels/RecorderPanel.tsx`,
    `FootagePanel.tsx`.
*   Commit 81a7f11: the separate-files design (Alternative B).

### Future Improvements

*   Separate files lined up by sound (Alternative B), if layouts are wanted
    while editing.
*   Pick an OBS audio track when a file has several.
*   Allow `.mkv` in the Footage tab's "add a video file…" (today mp4, mov,
    m4v, since joining needs matching containers).

--------------------------------------------------------------------------------

## Detailed Implementation Plan

Rough effort: 2–3 days. Each step leaves `cargo test` and the frontend
build green.

### 1. OBS playback (`core/src/adapters/video.rs`, `PreviewPanel.tsx`)

*   `ensure_playable_copy` remuxes non-MP4 containers (the copy rules in
    Technical Plan §1). Tests: the decision function (container and codecs →
    copy or not, and which audio / video arguments) as unit tests;
    `core/examples/playable_check.rs` (new) makes a 10-second `lavfi` clip as
    `.mkv` (H.264 + AAC) and as `.mkv` with Opus audio, runs
    `ensure_playable_copy` on each, and checks the result is a faststart MP4
    with AAC audio (ffprobe), printing `PLAYABLE-OK`. The preview's
    "Preparing preview…" state.

### 2. `concat_media` (`core/src/adapters/video.rs`, `core/src/engine/media.rs`)

*   Move and rename; write to `<data dir>/media/`; `append_media` calls it.
    Test: `playable_check` also joins two `lavfi` MP4s into a fresh data
    dir and checks the length.

### 3. Remove recording (Delete / Modify)

*   Everything under Technical Plan §3, in one commit, with the regenerated
    types (`TS_RS_EXPORT_DIR="$(pwd)/frontend/src/ipc/generated" cargo test
    -p roughcut-core --features ts-bindings export_bindings`) and the stale
    generated files deleted by hand.

### 4. Docs and branches

*   The docs under §3 and ADR-0008; tag the two branches; delete them only
    with the owner's OK.

### Verification Plan

*   `cargo test -p roughcut-core` (including `doc_parity` and
    `mock_parity`); `cd frontend && npx tsc -b && npm run build`; `cargo run
    -p roughcut-core --example playable_check`; `grep -rn "record_start\|
    RecorderPanel\|NSCameraUsage" app frontend/src core/src` finds nothing.
*   **In the app**, with the owner's OBS files: an OBS `.mkv` and an OBS
    `.mp4` each import, play with sound, transcribe and render; an old
    dual-capture project plays its camera bubble and renders it; "add a
    video file…" works on a fresh data dir; there is no Record entry
    anywhere; a fresh install never shows a camera or microphone prompt.

--------------------------------------------------------------------------------

*Drafted with EGM*
