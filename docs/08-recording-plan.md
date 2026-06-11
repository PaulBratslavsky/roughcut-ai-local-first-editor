# 08 — Recording: camera + screen capture (v1 plan)

Goal: record presentations INSIDE RoughCut — face, screen, or screen with a
face overlay — and drop straight into the existing pipeline (on-device
transcription → rough cut → text editing → export). No OBS, no re-recording
because your face covered the demo.

## Product shape (v1)

A **Record** entry point (empty state + "+ New video ▸ Record") opens the
recorder:

```
┌──────────────────────────────────────────────────────────┐
│  record                                              ✕   │
│                                                          │
│  ┌────────────────────────────────────────────────────┐  │
│  │                                                    │  │
│  │              live camera preview                   │  │
│  │              (or screen placeholder)        ┌────┐ │  │
│  │                                             │ 😀 │ │  │
│  │                                             └────┘ │  │
│  └────────────────────────────────────────────────────┘  │
│                                                          │
│  layout   [ camera ] [ screen ] [ screen + face ]        │
│  face     ( ) oval   (•) rounded square    size S M L    │
│                                                          │
│  camera   FaceTime HD ▾    mic  MacBook Pro ▾            │
│  screen   Display 1 ▾                                    │
│                                                          │
│              [ ● record ]   3-2-1 countdown              │
└──────────────────────────────────────────────────────────┘
```

While recording: menu-bar-style floating chip (elapsed · stop) so the main
window can be hidden during a screen take. Stop → both files land → a project
is created → transcription starts → you're in the editor.

After recording, the layout is **still editable** — Tools gains a `LAYOUT`
card (same presets as above). The preview composites live; nothing is baked
until MP4 export.

## The three design decisions

1. **Record sources separately; composite at view/export time.** Camera and
   screen are two files. Layout (camera / screen / pip + shape + size +
   corner) is project metadata. Non-destructive principle applied to capture
   — and the foundation for v2's per-segment layout switching.
2. **Capture = one ffmpeg process, two avfoundation inputs, two outputs.**
   Shared start clock (sync by construction), `h264_videotoolbox` hardware
   encoding (CPU stays free while presenting), no new dependencies, fits the
   VideoEngine adapter pattern. ScreenCaptureKit is the v2 upgrade, not the
   v1 risk.
3. **Mic audio lives in the camera file and is the master track** — it is
   what whisper transcribes, and cuts derived from it apply to both tracks
   (same clock). System-audio capture needs a loopback driver: v2.

## Model & engine deltas

- `Project.screen_media: Option<Media>` (serde default — back-compat).
- `Project.layout: Layout { mode: Camera|Screen|Pip, shape: Oval|RoundedRect,
  size: S|M|L, corner: BottomRight (only option in v1) }` — an EditOp
  (`set_layout`) so it's undoable and reachable by chat/MCP like everything
  else.
- `core/src/adapters/record.rs`: device enumeration
  (`ffmpeg -f avfoundation -list_devices true -i ""`), `start_recording(opts)
  -> RecordingHandle`, `stop()`. Progress/elapsed via the existing
  ProgressTask enum (`recording`).
- Permissions in `capabilities`/Setup: camera, mic, screen-recording TCC
  status + prompt triggers ("Recording" row, same pattern as ffmpeg/whisper).
- Playback: a second muted `<video>` in the preview, positioned/masked by
  CSS (`clip-path: ellipse` / `border-radius`) — the playback engine already
  owns the clock; the overlay just follows it.
- MP4 export: ffmpeg `-filter_complex` overlay; oval/rounded masks via a
  generated alpha mask. NLE export: both tracks in xmeml/fcpxml (V1 = screen,
  V2 = camera), full-size; baked transforms are export-v1.5.

## Milestones (each shippable, each builds on the last)

**M1 — capture spike + camera-only recording.**
Device enumeration, TCC permission row in Setup, record camera+mic to mp4
with a countdown and a stop button; file lands as a normal project
(auto-import → transcribe). *Spike question this must answer:* can webview
`getUserMedia` preview and ffmpeg share the camera? (Fallback if not: record
camera via webview MediaRecorder; ffmpeg keeps screen duty only.)
→ Value shipped: record a talking head directly in the app.

**M2 — screen-only recording.**
Display picker, screen-recording permission flow, same landing pipeline.
→ Value: transcribed screen-capture explainers.

**M3 — dual capture, synced.**
One ffmpeg, both inputs, two files; `screen_media` on the project; preview
shows the camera full-frame (layout machinery not built yet); the editor
works exactly as today because cuts ride the shared clock.
→ Value: presentation recording with post-hoc framing safety.

**M4 — layout presets.**
The `LAYOUT` card (Tools) + recorder presets: full camera / full screen /
pip bottom-right; oval and rounded-square shapes; S/M/L. Live in preview via
CSS masking; stored as an undoable EditOp.
→ Value: the feature as pitched — switch framing after the fact.

**M5 — export compositing.**
MP4 render bakes the layout (overlay + alpha mask); NLE exports carry both
tracks; export-validation doc gains a dual-track fixture.
→ Value: the recording leaves the app correctly everywhere.

**M6 — polish → v1.**
Mic level meter, mirrored preview, disk-space guard, crash-safe `.part`
recovery on relaunch, `R` shortcut, recording indicator, README/landing
update.

## Parked for v2 (explicitly out)

Per-segment layout switching from the transcript ("camera for the intro,
screen for the demo") — the differentiator this architecture is built to
enable; system audio; window-level capture; ScreenCaptureKit backend; click
highlighting; virtual backgrounds.

## Open risks

- Camera sharing between webview preview and ffmpeg (M1 spike).
- avfoundation frame-rate negotiation (pin per input: screen 30, camera 30).
- Screen-capture TCC is attributed to the app bundle — dev builds prompt for
  the terminal/binary; document in CONTRIBUTING notes.
- Long recordings: moov-at-end on crash — write fragmented mp4 or remux on
  stop (we already have the faststart machinery).
