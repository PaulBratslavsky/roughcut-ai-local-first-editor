# Import instead of record

*   **Author:** Paul Bratslavsky
*   **Status:** Approved: revised after four fresh-eyes reviews; owner decisions recorded
*   **Date:** 2026-10-31

--------------------------------------------------------------------------------

## Overview

Take recording out of RoughCut and make importing do everything recording
did. Creators already record in OBS, on a camera or a phone, in Riverside or
QuickTime; RoughCut's job is the editing.

The one thing in-app recording gave that plain import doesn't is a screen
file and a camera file on the same clock, which is what the camera-bubble
layout (screen full-frame, camera in a corner) needs. So this design adds a
**second track**: after making a project from one file, the user adds the
other one, and RoughCut lines the two up by their sound, automatically, with
a seconds field to fine-tune. Then the recorder, its capture code, its
permissions and its macOS-only code go.

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
*   It isn't the product's edge: the tools RoughCut competes with on editing
    (Gling, Opus Clip) don't record.
*   What only recording does today: dual capture writes a video-only screen
    file on the **same clock** as the camera file, so `attach_screen_media`
    can assume both start together (offset 0). Two files recorded separately
    start at different moments, often in different containers (OBS writes
    `.mkv` by default), and the good microphone is often on the screen
    recording rather than the camera.

## Goals

*   **A second track from a file.** The project's own file is the main
    track: its sound is the soundtrack and it is what gets transcribed. The
    user adds the other file (a screen or a camera recording), and RoughCut
    finds the offset between the two from their sound, says whether it is
    sure, and lets the user type or nudge the offset.
*   The camera-bubble layout, the MP4 render and the Premiere / Resolve XML
    honour the offset. A screen file in any common container plays in the
    preview.
*   Then remove in-app recording: the recorder screen, capture code, the
    recording Tauri commands, the camera and microphone permission strings,
    the recordings library, the recording examples and docs.
*   Nothing a user made is lost: projects made with dual capture keep
    working exactly as before; recordings on disk stay where they are and
    import like any file; `append_media` keeps working.
*   MCP clients can add and line up a second track too.

## Non-Goals

*   **Recording in any form**, including a hidden or "beta" recorder.
*   **More than one second track**, or switching between camera angles.
*   **Correcting drift** (two recorders whose clocks run at slightly
    different speeds). It is *detected* and reported (see Accuracy); fixing
    it is a Future Improvement.
*   **Guessing from file timestamps.** `creation_time` is second-precision at
    best, often missing in OBS files, and its meaning (start or end) and time
    zone vary by device; a confident-looking wrong guess is worse than
    "couldn't line these up — set the offset".
*   **Syncing by transcript words.** Sound onsets first; words are a Future
    Improvement.
*   **Detaching a second track.** Adding another file replaces it, and the
    layout's `camera` mode already hides the screen; a remove button is a
    Future Improvement.
*   **OBS's composited single file**: already an ordinary import.
*   **The second file's sound.** It is used only to line the files up; the
    soundtrack is always the main track's (as today: the preview's screen
    layer is muted, the render and the XML take no screen audio).
*   **Other export formats.** FCP XML, OTIO and EDL ignore the second track,
    as they ignore the screen file today.

## Decisions

Recorded 2026-11-01. The owner answered D0 and D2; D1 follows from D0.

*   **D0: How will you record after this?** **OBS, saving separate screen
    and camera files** (for example with the Source Record plugin), so the
    layout can still be changed while editing. Owner's words: "the app does
    not need to record or capture screen, purely for editing, will import
    from OBS." Consequences:
    *   Phase 1 is needed: one composited file would have made it
        unnecessary, but it can't be re-laid out.
    *   In OBS the microphone is normally an audio source of the main
        recording, so the good sound is usually on the **screen** file. A
        Source Record camera file may have no sound of its own.
    *   Verification uses the owner's OBS pairs.
*   **D1: Which way round can a project be?** **(b) Either way round**,
    because of D0: with OBS the project is usually made from the screen file
    (it has the mic) and the camera file is added as the bubble ("Add camera
    recording"). Camera main with screen added (CUJ 1) works the same way.
    *   A camera file with no sound can't be lined up by sound. Source
        Record starts it together with the main recording, so the offset-0
        fallback is usually right; the message says so ("…assuming both
        started together") instead of only "couldn't line these up".
    *   Rejected: (a) camera main only, which fails the common OBS setup.
*   **D2: When to remove.** **(a) Add and sync first, remove second.**
    Phase 1 is tried on the owner's OBS pairs and its thresholds tuned while
    the recorder still exists; Phase 2 follows without waiting.

The unmerged recording branches are not a question: `feat/native-capture`
and `fix/release-entitlements` (camera / microphone entitlements, needed
only by the recorder) are tagged `archive/native-capture` and
`archive/release-entitlements`, the new ADR cites the tags, and the branches
are deleted with the owner's OK.

## Critical User Journeys (CUJs)

### CUJ 1: Camera bubble from two OBS files

*   **Given**: `talk-camera.mp4` (24 min, a camera with its own mic) and
    `talk-screen.mkv` (OBS display capture with desktop audio on track 1 and
    the mic on track 2, started 7.4 s after the camera).
*   **When**: the user makes a project from the camera file, then in the
    Footage tab clicks **Add screen recording** and picks the `.mkv`.
*   **Then**: the screen file is attached at once ("lining up…"), then: "The
    screen starts 7.40 s into the camera footage — lined up by sound
    (sure)." The preview shows the screen full-frame with the camera in the
    corner; typing sounds land on the keystrokes. The layout options are as
    today.

### CUJ 2: The good mic is on the screen file (the usual OBS case)

*   **Given**: the same session, but the camera's mic is a scratch track.
*   **When**: the user makes the project from `talk-screen.mkv`, then clicks
    **Add camera recording** and picks the camera file.
*   **Then**: as CUJ 1, with the screen's sound as the soundtrack and the
    transcript made from it; the camera is the bubble.

### CUJ 3: Fine-tune

*   **Given**: CUJ 1; the user sees the typing land a little late.
*   **When**: they click "−1 frame" twice next to the offset field.
*   **Then**: the field reads 7.333 s (two frames of the camera's 30 fps);
    the preview moves at once, paused or playing; the MP4 and the XML use
    the new value; "Reset to measured" brings back 7.400 s.

### CUJ 4: No usable sound

*   **Given**: a second file with no audio (an OBS Source Record camera file
    without its own mic), or only system audio the other file never heard.
*   **When**: the user adds it.
*   **Then**: "Couldn't line these up by sound, so they're assumed to have
    started together (0 s). If they didn't, play a moment with a visible
    action and type the offset." The offset is 0 s, marked unsure. For
    Source Record files, which start with the main recording, 0 s is usually
    right.

### CUJ 5: Long recording that drifts

*   **Given**: a 60-minute pair whose clocks differ by 80 ppm.
*   **When**: the user adds the second file.
*   **Then**: "Lined up by sound at the start (sure), but the two drift
    apart by 0.29 s by the end — check the last minutes." The offset fits
    the start.

### CUJ 6: An old dual-capture project

*   **Given**: a project made with the in-app recorder.
*   **When**: the user opens it after Phase 2.
*   **Then**: it plays, renders and exports exactly as before. The recorder
    button is gone; existing recordings in `~/Movies/RoughCut` are untouched.

### CUJ 7: Claude Code adds a screen file

*   **When**: it calls `attach_second_track { project_id, file_path, role:
    "screen" }`.
*   **Then**: the result is `{ attached: true, offset_s, sync }` after the
    measurement, as in CUJ 1; the app's Footage tab and preview update.

## Technical Plan (Architecture)

### Data model (`core/src/model.rs`)

`screen_media` becomes one structure that keeps the file, its role and its
alignment together, so an offset can't exist without a file:

```text
Project.second_track: Option<SecondTrack>
SecondTrack {
  media: Media,
  role: Screen | Camera,      // what the SECOND file is
  offset_s: f64,              // see "The offset" below
  sync: TrackSync,
}
TrackSync {
  method: SameClock | Audio | Manual | None,
  sure: bool,
  measured_offset_s: Option<f64>,      // what the audio match found
  audio_offset_at_measure_s: f64,      // the main track's A/V nudge then
  drift_s: Option<f64>,                // end minus start, when measured
  measured_at: Option<DateTime<Utc>>,
}
```

*   **Migration** (`Project::upgrade()`): a project with `screen_media` set
    loads as `second_track = { media, role: Screen, offset_s: 0, sync:
    SameClock (sure) }`; `screen_media` is read (serde default) but no longer
    written. `schema_version` is bumped.
*   **The offset**: `offset_s` is in the main track's **video** time: the
    second file's 0:00 plays at main-video time `offset_s`. Second-file time
    = main-video time − `offset_s`. Positive: the second file started later.
*   **The A/V nudge**: the app already shifts the main track's sound against
    its picture by `audio_offset_s`. The audio match measures sound against
    sound, so the stored offset is `measured lag + audio_offset_s`, and
    `audio_offset_at_measure_s` records the nudge used. If the user changes
    the A/V nudge later, "Reset to measured" recomputes with the current one.
*   **Not journaled**, like `layout` and `audio_offset_s`: undo moves the
    cut, not the alignment. "Reset to measured" is the way back.
*   `role`: `Camera` means the main track is the screen and the second file
    is the camera (the usual OBS case, CUJ 2).

### Finding the offset (`core/src/media_align.rs`, new)

A pure function over two sound envelopes; no I/O.

*   **Input**: one 50 Hz envelope per file (RMS per 20 ms of all its audio
    tracks mixed — see Adapters), or `None` for a file with no audio.
*   **Onsets**: `o[n] = max(0, ln(e[n] + ε) − ln(e[n−1] + ε))`; onsets match
    across different microphones hearing the same voice and keystrokes; raw
    levels don't.
*   **Coarse search**: onset curves taken down to 5 Hz (max per group);
    Pearson correlation of the overlapping part for every lag where the two
    files overlap by at least 60 s — the whole length, so a screen recording
    of minutes 22–27 of a 40-minute camera file is found. Convention:
    `c(L) = corr(main[n], second[n − L])`; the best `L` is the lag.
*   **Fine search**: at 50 Hz, ±2 s around the coarse peak, with parabolic
    interpolation of the peak: better than 10 ms.
*   **Sure**: the peak's correlation `r ≥ 0.4` and `r / best r more than
    1 s away ≥ 1.5` (starting values, tuned on D0's pairs and committed).
    Zero variance gives `r = 0`. A peak at the edge of the search is unsure.
*   **Drift**: the fine search repeated on the first and the last 10 minutes
    of the overlap (when the overlap is over 20 minutes); `drift_s` = end −
    start. Over half a frame of the main track: reported, and `sure` is
    false.
*   **Not found** (no audio, flat peak `r < 0.1`): `method: None`, offset
    0, unsure.

### Adapters (`core/src/adapters/video.rs`)

*   **`VideoEngine::audio_envelope(&Media) -> Result<Option<Vec<f32>>>`**
    (new): ffprobe counts the audio streams; none → `Ok(None)`; otherwise
    ffmpeg mixes them all (`amix`), resamples, and writes 50 Hz RMS values
    to stdout, the same pattern as `decode_raw_peaks`, in memory (no temp
    WAV, so it can't collide with a transcription of the same file). Mixing
    all tracks catches an OBS mic on track 2. An ffmpeg failure on the audio
    is treated as "no audio", not an error. The mock returns `Ok(None)` (demo
    mode: "couldn't line up", never an error). The matching runs in
    `spawn_blocking`.
*   **Playable copy for the second file**: `ensure_playable_copy` today runs
    only for the main media and only fixes MP4s with the index at the end
    (or more than 2 audio channels). It is extended to remux, losslessly,
    any container WebKit can't play (`.mkv`, `.webm`, `.flv`) when the
    codecs allow it (H.264 / HEVC + AAC). `Editor::media_assets` (today it
    looks only at the main media) also prepares the second track's copy,
    cached by file identity like the main file's, and returns it as
    `MediaAssets.second_playback_path`, which the preview plays. When the codecs don't allow a copy,
    the Footage tab says the file needs converting (the render and XML
    still use the original).
*   **`RenderJob.screen`** carries the offset and the role, so the
    compositor knows which file is full-frame. Every render of a project
    with a second track passes it (`core/src/export/mod.rs`: the main export
    and saved clips; the `RenderJob` literals in `render_seek_check.rs` and
    the test helper in `video.rs` change with it).
*   **Frame rate**: dual capture was 30 fps on both sides; imported screen
    files are often 60 fps or variable. The second file's chain in
    `composite_graph` gets `fps=<main fps>` so the overlay follows the main
    track's timing.

### Using the offset

One rule everywhere: the second file covers main-video time
`[offset_s, offset_s + second.duration]`. Outside that range its first or
last frame holds.

*   **MP4 render** (`render_plan` / `composite_graph`), per input group with
    main seek `S`:
    *   second-file seek `S2 = to_ms(clamp(S − offset_s, 0, second.duration
        − SCREEN_TAIL_S))`; `shift = S − offset_s − S2`.
    *   `shift > 0.0005`: `setpts=PTS−shift/TB` (as today).
    *   `shift < −0.0005`: `tpad=start_mode=clone:start_duration=−shift`
        (the piece starts before the second file does).
    *   end padding `stop_duration = max(0, main.duration − offset_s −
        second.duration) + SCREEN_TAIL_S + 1`, in the same `tpad`. At offset 0
        this equals today's formula, so dual-capture renders are unchanged.
    *   The second file's own audio is never used: the soundtrack is the
        main track's.
*   **Preview** (`PreviewPanel.tsx`): a pure `secondTimeFor(mainTime)`
    helper; `t < 0` → pause the second video at 0 and start it after
    `−t / rate`; `t > duration` → pause at the last frame; otherwise seek
    when it drifts more than half a frame (always when paused) and play when
    the main plays. Re-evaluated on `seeked`, `play`, `ratechange` and when
    the offset changes.
*   **Premiere / Resolve XML** (`xmeml.rs`): `off_f = to_frame(offset_s,
    fps)`; per clip `s_in = in_f − off_f`, `s_out = min(out_f − off_f,
    s_total)`; skip empty clips; when `s_in < 0`, the clip's `in` is 0 and
    its record start moves later by `−s_in` (a gap on the screen track where
    the MP4 holds the first frame). The XML is frame-rounded, the MP4 exact:
    they can differ by half a frame. The second file's own timebase is
    written for its clip (a 60 fps OBS screen next to a 29.97 camera).
*   **9:16 crops**: unchanged — a composited project's crop is set by hand.

### Tools (`core/src/tools.rs`) — all `agent: false`, visible to MCP

*   **`attach_second_track { project_id, file_path, role? = "screen" }`**
    → `{ attached, offset_s, sync }`. Needs the main media first. Probes,
    stores the track with `method: None`, emits `TimelineChanged`, then
    measures (single run per project: `inflight::try_claim`; a few seconds;
    `progress` task `align_tracks`), writes the result only if the same
    file is still attached, emits again, and returns. Measuring never fails
    the attach: an error becomes `method: None` with the reason in the
    result. Replaces `attach_screen_media`, which stays as an alias for one
    release (role screen) so scripts keep working.
*   **`set_track_offset { project_id, offset_s }`** → `{ offset_s, sync }`.
    Needs a second track; finite; clamped to ± the main duration; rounded to
    1 ms; keeps `method` and `measured_offset_s`; emits `TimelineChanged`.
*   **`reset_track_offset { project_id }`**: back to `measured_offset_s +
    current audio_offset_s` (when there is one).
*   The shell command `attach_screen_media` (a duplicate the recorder used)
    stays until Phase 2 for the recorder, which writes `method: SameClock`
    through the Editor directly; it goes with the recorder.

### Frontend (Phase 1)

*   **Footage tab** (`FootagePanel.tsx`): "Add screen recording" and "Add
    camera recording" (the user picks which; the role isn't detected). Picker extensions: mp4,
    mov, m4v, mkv, webm. With a track attached: its file name; the sync line
    (one string per method and sign, e.g. "The screen starts 7.40 s into the
    camera footage — lined up by sound (sure)" / "…3.20 s before…" / drift
    and no-sound messages); an offset field in seconds (three decimals) with
    −1 frame / +1 frame (one frame of the main track's rate; 30 if unknown)
    and "Reset to measured" — the existing `AudioSyncSection` pattern.
*   **Layout** (`InspectorPanel.tsx`): unchanged; shown when a second track
    exists.
*   **Browser mock**: the Footage tab and the screen preview are Tauri-only
    today (the Footage tab returns early outside Tauri) and stay so; the
    mock implements the tools (sets the track on its project fixture, a
    fixed measured offset, field and nudges in memory) so parity holds, and
    the UI states are checked in the app.

### Removing recording (Phase 2)

*   **Core**: delete `adapters/record.rs` and its `pub mod record;` line.
    Move the lossless concat that `append_media` uses (`combine_recordings`,
    its private `concat` and its codec guard) to `adapters/video.rs` as
    `concat_media`, writing to `<data dir>/media/` (created on demand)
    instead of `~/Movies/RoughCut` (only the recorder ever created that
    folder), with errors that no longer say "recordings". Drop
    `ProgressTask::Recording` and the recording note in `progress.rs`. Delete the examples `record_smoke`,
    `crash_smoke`, `dual_smoke`, `screen_smoke`, `takes_smoke` and their
    rows in `core/examples/README.md`. Remove `attach_screen_media`'s alias.
*   **App shell**: delete the commands `record_devices`, `record_start`,
    `record_status`, `record_stop`, `record_pause`, `record_resume`,
    `list_recordings`, `combine_recordings`, `delete_recording`,
    `screen_permission_status`, `request_screen_permission`,
    `open_privacy_settings` and the shell `attach_screen_media`; the
    recorder's exit hook; `app/src-tauri/Info.plist` (it holds only the
    camera and microphone keys).
*   **Frontend**: delete `RecorderPanel.tsx`; the `"recorder"` screen
    (`viewStore.ts`, `App.tsx`); the top bar's Record entry and the `R`
    shortcut (the "+ New video" menu is left with one item, so it becomes a
    plain button); the import screen's record entry; the Footage tab's
    "record a new clip", recordings library and their copy; Setup's
    Recording row; the recording wrappers in `ipc/api.ts` and their
    `!isTauri` fallbacks; the recorder's styles (keeping `.mic-meter`,
    `.playback-meter`, `.screen-layer`, `.layout-*` and `.footage-*`, which
    other panels use); the stale generated `CaptureDevice(s)`,
    `RecordingFile`, `RecordingOutcome`, `RecordingStatus` types (ts-rs
    doesn't delete files) and a regenerated `ProgressTask.ts`; the
    preview's "deleted from the library?" error text; the `ingest.ts`
    comment.
*   **Docs**: `docs/08-recording-plan.md` marked superseded (and its
    SITEMAP row); README's "Record in the app" replaced by "Camera + screen
    recordings"; the recording parts of `07-architecture.md`, `tool-api.md`
    (the shell-command list and the `recording` progress task),
    `architecture-audit.md` (B18, F5), ADR-0005, the core READMEs
    (`adapters`, `engine`, `examples`) and the `roughcut-elephant` skill;
    **ADR-0008: Remove in-app recording** (0006 was the native recorder, on
    the archived branch), citing the archive tags.
*   **Signing**: `scripts/build-macos-app.sh` and the README give privacy
    grants as the reason for stable signing. The signing stays (the key
    store for hosted mode also benefits from a stable signature); the
    reason is reworded.
*   **Kept**: the second track, `layout`, `set_layout`, compositing in
    preview, render and export; `append_media`; every project and file made
    by the old recorder; `CONTEXT.md` has no recording entries to remove.

## Alternatives Considered

### Alternative A: Keep recording, frozen

*   **Reason for Rejection**: the owner chose removal; the code, the
    permissions and the fragility would stay, and Windows / Linux never get
    it.

### Alternative B: Remove recording and the camera-bubble layout with it

*   **Reason for Rejection**: the layout is editing, and tutorial creators
    rely on it; OBS users would have to bake the layout in before editing.

### Alternative C: Line up by hand only, or by file timestamps

*   **Reason for Rejection**: separately started recordings are often
    minutes apart; timestamps are coarse and unreliable (see Non-Goals). The
    sound match handles the common case; the offset field handles the rest.

### Alternative D: Sync by transcript

*   **Reason for Rejection**: a second whisper pass over the second file
    (minutes), and it fails on silent typing. A Future Improvement if onsets
    prove unreliable.

### Alternative E: Keep `screen_media` and add separate offset fields

*   **Pros**: no migration.
*   **Cons**: an offset can outlive its file, and the camera-as-second-file
    case (D1) needs a role anyway.
*   **Reason for Rejection**: one `SecondTrack` keeps them together; the
    migration is one `upgrade()` step.

### Alternative F: A general list of extra media

*   **Reason for Rejection**: a second camera means angle switching, not a
    bubble; nothing needs a list yet. `media_align` is generic over two
    envelopes, so it can be reused.

## Non-Functional Requirements

*   **Speed**: measuring a 24-minute pair under 15 s on an M-series Mac
    (decoding two envelopes dominates; measured in Verification).
*   **Accuracy**: within one frame of the main track at the start of the
    overlap on pairs where both files hear the speaker. Over long
    recordings clocks drift (20–100 ppm: 30–360 ms an hour); drift over half
    a frame is detected and shown, not corrected.
*   **Privacy**: everything on-device. Removing recording removes the app's
    camera and microphone permission prompts.
*   **Compatibility**: old dual-capture projects migrate to a same-clock
    second track and render, preview and export exactly as before (tested).
*   **Platforms**: nothing platform-specific is added.

--------------------------------------------------------------------------------

### Additional Resources

*   [`docs/08-recording-plan.md`](../08-recording-plan.md) — the recorder this
    removes; `archive/native-capture` — the native recorder.
*   Code: `core/src/engine/media.rs` (`attach_screen_media`,
    `append_media`), `core/src/adapters/video.rs` (`render_plan`,
    `composite_graph`, `decode_raw_peaks`, `ensure_playable_copy`),
    `core/src/export/xmeml.rs`, `core/src/export/mod.rs`, `PreviewPanel.tsx`
    (second `<video>` follower), `FootagePanel.tsx`, `InspectorPanel.tsx`
    (`AudioSyncSection`).

### Future Improvements

*   Drift correction (a speed factor from the start and end offsets).
*   Sync by transcript words.
*   Several second tracks, or takes split across files.
*   A "Remove second track" button.
*   Detect an OBS composited file and offer to re-frame it.

--------------------------------------------------------------------------------

## Detailed Implementation Plan

Rough effort: Phase 1 about 6–7 days; Phase 2
about 1–2 days. Each step leaves `cargo test` and the frontend build green.

### Phase 1: add and line up a second track

#### 1. Model (`core/src/model.rs`, `events.rs`)

*   `SecondTrack`, `TrackRole`, `TrackSync`, `SyncMethod`;
    `Project.second_track`; the `upgrade()` migration;
    `ProgressTask::AlignTracks`; `MediaAssets.second_playback_path`;
    regenerate the frontend types (`TS_RS_EXPORT_DIR=… cargo test -p
    roughcut-core --features ts-bindings export_bindings`). Tests: an
    old project JSON with `screen_media` loads as a same-clock second track.

#### 2. `core/src/media_align.rs` (Create)

*   Onsets, coarse and fine search, sure, drift. Tests on synthetic
    envelopes (aperiodic clicks from a seeded generator, ≤ 2 minutes so debug
    tests stay fast): a known positive and negative lag found within 10 ms;
    different gain and added noise; no shared sound → not found; a lag far
    into a long main track; drift detected when the second envelope is
    stretched by 100 ppm.

#### 3. Adapters (`core/src/adapters/video.rs`)

*   `audio_envelope` (ffmpeg + mock + auto); `ensure_playable_copy` for
    other containers; `RenderJob.screen` with offset and role;
    `render_plan` / `composite_graph` math and the `fps=` step. Tests:
    render-plan strings for positive and negative offsets, a piece wholly
    before the second file, one past its end, the camera-as-second role, a
    60 fps second file against a 30 fps main, and offset 0 identical to
    today. `render_seek_check` (real ffmpeg, PSNR) gains a positive and a
    negative offset case.

#### 4. Engine and export (`core/src/engine/media.rs`, `core/src/export/mod.rs`, `xmeml.rs`)

*   `attach_second_track` (attach, emit, measure, guarded write),
    `set_track_offset`, `reset_track_offset`; the recorder's same-clock
    path; render jobs carry the track; xmeml offsets and the second file's
    own timebase. Tests: xmeml goldens for positive, negative and zero
    offsets and a clip wholly before the second file; a measurement whose
    file was replaced meanwhile is discarded; the demo-mode e2e test
    (`e2e.rs`, today's `attach_screen_media` call) attaches and gets
    "couldn't line up" instead of an error.

#### 5. Tools and docs (`core/src/tools.rs`, `docs/tool-api.md`, `frontend/src/ipc/mockApi.ts`, `CONTEXT.md`, core READMEs)

*   The three tools and the alias, documented and mocked in the same step
    (parity tests); `align_tracks` in the progress-task list; **SecondTrack**
    and `media_align` in `CONTEXT.md`; the offset rules in
    `core/src/export/README.md` and `core/src/adapters/README.md`.

#### 6. Frontend (`FootagePanel.tsx`, `PreviewPanel.tsx`, `ipc/queries.ts`, `styles.css`)

*   Add buttons, sync line, offset field, nudges, reset; the preview
    follower and the second file's playable copy.

#### 7. `core/examples/align_check.rs` (Create)

*   Against real ffmpeg: generate a 3-minute source with `lavfi` (seeded
    random clicks and noise), derive a "camera" mp4 and a "screen" `.mkv`
    (second audio track, `highpass=300,volume=0.25`, different lengths) at
    offsets +7.4 s and −3.2 s; check each is found within 20 ms and the
    `.mkv` gets a playable copy; print the time taken and `ALIGN-OK`.

### Phase 2: remove recording

#### 8. Core, app shell, frontend, docs (Delete / Modify)

*   Everything under "Removing recording (Phase 2)"; tag and (with the
    owner's OK) delete the archived branches; ADR-0008. `append_media`
    gains a test that concatenates two `lavfi` clips with `concat_media`
    (ffmpeg-gated, like `render_seek_check`).

### Verification Plan

*   `cargo test -p roughcut-core`; regenerated types leave no diff; `cd
    frontend && npx tsc -b && npm run build`; `cargo run -p roughcut-core
    --example align_check`.
*   **Real pairs (the user's, per D0)**: at least two pairs where both files
    hear the speaker, one `.mkv` from OBS, one pair whose screen file has no
    usable sound, and one long pair (measure the first and last 5 minutes:
    is drift real?). Pass: within one frame by eye on a keystroke; the
    first two "sure"; the silent one falls back. The tuned thresholds are
    committed.
*   **In the app**: the preview before the second file starts and after it
    ends; a nudge moving the preview while paused; an MP4 with a negative
    offset; a Premiere import of an XML with an offset.
*   **Old projects**: a test loads an old dual-capture project JSON and
    checks its render plan and XML match today's output.

--------------------------------------------------------------------------------

*Drafted with EGM*
