# RoughCut for DaVinci Resolve

Two Scripts-menu entries (Workspace ▸ Scripts ▸ Utility):

- **RoughCut AI Draft** — select a clip in the media pool, run it, and an
  edited timeline appears: silences, filler words, and duplicate takes
  removed on-device, every cut still an editable clip.
- **RoughCut Import Cut** — pull your CURRENT RoughCut edit into Resolve
  (most recently edited project → Resolve XML → imported, media auto-linked).
  Use it after refining in the app.

Both work in the **free** version of Resolve — they're in-app scripts, which
have full API access. (Driving Resolve from OUTSIDE — RoughCut's
"Send to DaVinci Resolve" export item — needs Resolve **Studio** with
Preferences ▸ System ▸ General ▸ "External scripting using" set to Local;
on free Resolve that button still exports the XML and reveals it for
File ▸ Import ▸ Timeline.)

## Install

Easiest: RoughCut's Setup screen (gear icon) ▸ DaVinci Resolve ▸
**Install plugin** — the scripts are embedded in the app. Or from a checkout:

```sh
./install.sh        # copies the scripts into Resolve's Scripts/Utility menu
```

Requirements: the RoughCut app installed and running (the script launches it
if needed), with ffmpeg + a whisper model set up (RoughCut's gear-icon Setup
screen handles both).

## Use

Fresh draft from footage:

1. In Resolve, add your footage to the media pool and select the clip.
2. Workspace ▸ Scripts ▸ Utility ▸ **RoughCut AI Draft**.
3. Watch progress in the console (Workspace ▸ Console). Transcription runs
   on the Apple GPU — a 26-minute talking-head takes a couple of minutes.
4. A new timeline named "<clip> — AI rough draft" appears in the media pool.

Bring a refined cut over:

1. Edit in the RoughCut app (transcript edits, chat, Target length…).
2. Workspace ▸ Scripts ▸ Utility ▸ **RoughCut Import Cut** — approve the
   export in the RoughCut window if asked.
3. The timeline lands in the media pool, linked to the original file.

First import on a fresh Resolve install: macOS may make Resolve ask where
the media lives — pick the folder once (this grants file access) or enable
Desktop access under System Settings ▸ Privacy & Security ▸ Files and
Folders.

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
