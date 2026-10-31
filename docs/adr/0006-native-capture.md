# ADR-0006: Native AVFoundation capture replaces ffmpeg capture

**Status:** accepted (2026-10)

Supersedes decision 2 of [`08-recording-plan.md`](../08-recording-plan.md)
("Capture = one ffmpeg process, two avfoundation inputs, two outputs").

## Context

The recorder ran one ffmpeg process per take, with `-f avfoundation` inputs
for the camera + mic and, in dual sessions, the display. The plan named
ScreenCaptureKit as the v2 upgrade and treated ffmpeg as the low-risk v1.

It wasn't low-risk. On 2026-10-29, every take failed within 0.1 s while OBS
Studio was running. OBS (32.2.2) locks a camera's configuration for as long as
a camera source uses a custom resolution (`OBSAVCapture.m`,
`lockForConfiguration` held until the device or preset changes). ffmpeg's
avfoundation input always calls `lockForConfiguration` to set its format and
frame rate, and treats a failed lock as fatal ("Could not lock device for
configuration", `libavdevice/avfoundation.m`). Apps that don't insist on the
lock — FaceTime, Zoom, browsers, RoughCut's own webview preview — share the
camera fine. RoughCut couldn't, and creators who stream are exactly the users
who keep OBS open.

The failure was also invisible: the capture child's stderr went to
`/dev/null`, so the recorder flipped to "paused" with no reason, and resuming
repeated it.

## Decision

Capture natively, in-process, with AVFoundation (`core/src/adapters/capture.rs`,
via `objc2-av-foundation`, the objc2 generation Tauri and the face adapter
already use). One path for every recording; no ffmpeg capture fallback.

- **The configuration lock is optional.** If RoughCut can take it, it picks
  the best camera mode (the same rule as before) and releases the lock once
  the session runs. If another app holds it, the camera is shared at its
  current format.
- **Encoding** is `AVAssetWriter` → hardware H.264 + mono AAC, fragmented MP4
  every 2 s (crash-safe, like `+frag_keyframe+empty_moov` was).
- **Sync** stays by construction: every sample carries a host-clock timestamp,
  and every writer of a take starts at the same source time — the moment the
  last video stream delivered its first frame.
- **Errors are first-class.** Starting a take waits until every stream
  delivers and fails with a reason otherwise (permission off, device gone,
  display not delivering). A take that dies mid-recording reports why
  (`RecordingStatus.error`): AVCaptureSession runtime errors, writer errors,
  or a stalled camera/mic.
- **ffmpeg stays for post-processing**: concatenating takes (stream copy, or
  scale + re-encode when another app changed the camera format between
  takes) and the mic-level normalization that used to run at record time.

## Consequences

- Recording works alongside OBS, Zoom, FaceTime and anything else holding the
  camera; the take uses whatever resolution that app chose.
- The recorder now depends on AVFoundation behavior directly. Display capture
  uses `AVCaptureScreenInput`, which Apple deprecated in favor of
  ScreenCaptureKit but still ships (ffmpeg used the same class). Moving the
  display path to ScreenCaptureKit is the natural next step and fits inside
  `capture.rs`.
- A shared camera runs at the other app's format, so takes can be 4K or
  60 fps sources; video is capped at ~30 fps and the bitrate scales with
  size (camera 4–16 Mbps). Some shared cameras stamp their first frame 0;
  samples far from the host clock are dropped.
- Capture runs inside the app process, so camera/mic/screen permission is the
  app's own (no child process attribution). The hardened-runtime entitlements
  for camera and microphone (`app/src-tauri/Entitlements.plist`, on branch
  `fix/release-entitlements` until it merges) are required
  for signed release builds.
- Native capture also made a live display preview cheap: the recorder shows
  the selected display (8 fps, ≤ 640 px) before recording, over a raw-bytes
  Tauri command. It stops whenever a take starts.
- The smoke examples (`core/examples/*_smoke.rs`) exercise the native path;
  `crash_smoke` now SIGKILLs a child copy of itself mid-take.
