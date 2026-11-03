# ADR-0008: Remove in-app recording — record in OBS, import one file

**Status:** accepted (2026-11-03)

## Context

RoughCut could record as well as edit: camera, screen, or both at once
("dual capture", the screen file playing under a camera bubble), with
pause / resume takes and a recordings library in `~/Movies/RoughCut`
(`docs/08-recording-plan.md`). That was over 3,000 lines:
`core/src/adapters/record.rs` (899), `frontend/src/panels/RecorderPanel.tsx`
(841), twelve Tauri commands and five examples. About 1,500 more lines of
native AVFoundation capture sit on the unmerged branch `feat/native-capture`,
with its own ADR-0006. That ADR was never merged, but the number 0006 is
taken by it.

- **macOS only.** Capture used ffmpeg's `avfoundation` input. The product
  promises Mac, Windows and Linux; each would need its own capture code.
- **The most fragile part of the app.** About 20 commits went into fixing
  it: OBS holding the camera, a shared camera's bad first timestamp, encoder
  back-pressure at 4K60, privacy permissions, code signing. Camera drivers,
  OBS and macOS updates can break it again.
- **Not the product's edge.** RoughCut is an AI editor. The tools it
  competes with on editing (Gling, Opus Clip) don't record.
- **The owner records in OBS** already.

The owner decided (`docs/designs/remove-recording.md`, D0–D2) to remove
recording and to compose the scenes (screen, camera bubble) in OBS, so there
is one file to import.

## Decision

1. **No recording in RoughCut**, in any form, including a hidden or "beta"
   recorder. Removed: `adapters/record.rs` and `ProgressTask::Recording`;
   the twelve recording Tauri commands (`record_*`, `list_recordings`,
   `combine_recordings`, `delete_recording`, the screen-permission commands,
   `open_privacy_settings`), the shell's duplicate `attach_screen_media` and
   the recorder's exit hook; `app/src-tauri/Info.plist`, which held only the
   camera and microphone usage strings; the recorder screen
   (`RecorderPanel.tsx`), the `R` shortcut, the Record entries in the top
   bar, the import screen and the Footage tab, the recordings library and
   Setup's Recording row; the examples `record_smoke`, `crash_smoke`,
   `dual_smoke`, `screen_smoke` and `takes_smoke`.
2. **Record in OBS, import one file.** The user composes the scenes in OBS
   and imports the file like any video. "+ New video" is now a plain button
   that opens a file.
3. **Non-MP4 files get a playable copy for the preview.** WebKit can't open
   Matroska, so an OBS `.mkv` imported but showed no picture.
   `video::ensure_playable_copy` now also makes the copy
   (`<data dir>/cache/<key>/play.mp4`) when the container isn't MP4 / MOV:
   H.264 or HEVC video is copied (HEVC tagged `hvc1`), other video is
   converted to H.264; AAC audio with at most 2 channels is copied, other
   audio becomes stereo AAC; only the first video and first audio track are
   kept. MP4 behaviour is unchanged. Only the preview uses the copy:
   transcription, render and export read the original. The preview shows
   "Preparing preview…" until the copy is ready. If the copy fails, the
   reason is kept for the rest of the run (`MediaAssets.playback_error`, not
   retried until a restart) and the preview says so, adding that editing,
   transcription and export still use the original file.
4. **Adding footage joins into the data dir.** `append_media` joined files
   with `record::combine_recordings`, which wrote to `~/Movies/RoughCut`, a
   folder only the recorder created. The join is now `video::concat_media`.
   It refuses files whose first video track (codec, size) or first audio
   track (codec, channels, sample rate) differ, joins only those two tracks,
   and writes `<data dir>/media/joined-<timestamp>-<8 hex>.mp4` (the folder
   is created on demand). When any input isn't MP4 / MOV the joined file is
   an `.mkv`, so the preview copy can still fix what an MP4 couldn't play.
   The Footage tab's "add a video file…" accepts `.mkv` too.
5. **The camera-bubble layout stays, unchanged.** `screen_media`, `layout`,
   `set_layout`, the registry tool `attach_screen_media` (now described as a
   screen file that started at the same moment as the camera file, offset 0)
   and compositing in the preview, the MP4 render and the XML export all
   stay, so projects made with the recorder's dual capture keep working.

## Consequences

- macOS never asks RoughCut for camera or microphone access.
- Windows and Linux no longer miss a feature the Mac build had. Nothing
  platform-specific was added.
- Local code signing stays (`scripts/build-macos-app.sh`), for a new reason:
  a stable signature keeps per-app grants, such as access to the Keychain
  item that holds the Hosted-mode API key, from resetting on every rebuild.
- Nothing a user made is lost. Old projects, dual-capture ones included,
  play, render and export as before. Files in `~/Movies/RoughCut` stay where
  they are and import like any file; nothing reads that folder any more.
- A non-MP4 import keeps a second copy in the cache, as some MP4s already
  did. A stream copy takes seconds; converting a video codec MP4 can't
  carry (rare from OBS) takes minutes.
- In a new OBS file the camera bubble is part of the picture, so its layout
  can't be changed while editing. Separate screen and camera files, lined up
  by sound, were designed in full (commit 81a7f11) and set aside; that design
  is the starting point if layouts while editing are wanted later.
- The preview copy, the render and the join use the file's first audio
  track (OBS's track 1). Choosing among OBS's audio tracks is future work.
- The branches `feat/native-capture` (the native recorder and its ADR-0006)
  and `fix/release-entitlements` (camera and microphone entitlements for
  hardened-runtime builds, needed only by the recorder) are kept as the tags
  `archive/native-capture` and `archive/release-entitlements`.
- `docs/08-recording-plan.md` is superseded and kept as history.
