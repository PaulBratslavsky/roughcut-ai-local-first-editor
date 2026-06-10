# RoughCut for DaVinci Resolve

One-click AI rough draft inside Resolve: select a clip in the media pool,
run the script, and an edited timeline appears — silences, filler words, and
duplicate takes removed on-device, every cut still an editable clip.

Works in the **free** version of Resolve (it's an in-app script, not an
external-scripting integration).

## Install

```sh
./install.sh        # copies the script into Resolve's Scripts/Utility menu
```

Requirements: the RoughCut app installed and running (the script launches it
if needed), with ffmpeg + a whisper model set up (RoughCut's gear-icon Setup
screen handles both).

## Use

1. In Resolve, add your footage to the media pool and select the clip.
2. Workspace ▸ Scripts ▸ Utility ▸ **RoughCut AI Draft**.
3. Watch progress in the console (Workspace ▸ Console). Transcription is the
   long step — a 40-minute 4K talking-head takes a few minutes.
4. A new timeline named "<clip> — AI rough draft" appears in the media pool.

The same edit also exists as a project inside the RoughCut app, where you can
refine it by editing the transcript or chatting with the local model, then
re-export.

## How it works

The script is a thin client over RoughCut's local tool API (the same
localhost endpoint Claude Desktop uses): `create_project` → `transcribe` →
`generate_rough_cut` → `export` (FCP7 xmeml) → Resolve's
`MediaPool.ImportTimelineFromFile`. No cloud, no uploads — the only network
involved is loopback.

## Testing without Resolve

`python3 "RoughCut AI Draft.py" /path/to/video.mp4` runs the entire pipeline
standalone and prints the XML path instead of importing.
