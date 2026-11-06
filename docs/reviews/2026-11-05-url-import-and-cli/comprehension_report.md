# Stage 6: Comprehension Test

**Target:** `docs/designs/url-import-and-cli.md` (FeatureRange 1–485,
DetailedRange 486–853)
**Reviewer:** `egm-comprehension` (fresh context, no standard)
**Status:** complete. Every needed file was readable. No Blockers (the
reviewer could paraphrase every section); 7 Major, 7 Nit.

## What this proposes

The design adds a registry tool, `import_url`. It checks that the link is
http(s), runs one yt-dlp download (locked-down arguments, link after `--`)
into a hidden temporary folder inside a download folder you can set in
Preferences (default `~/Movies/RoughCut/`), and moves the file to
`<title> [<id>].mp4`. It then creates or attaches a project that records the
source link. Any failure deletes everything. The app installs and updates its
own copy of yt-dlp from Setup. A new `roughcut` binary (crate `cli/`, shipped
inside RoughCut.app) calls the MCP endpoint as an MCP client and opens the
app in the background if it isn't running. `roughcut serve` runs the editor
with no window and handles approvals on its own terminal. An OS file lock
(`library.lock`) makes sure only the app or `serve` uses the library at one
time. The work ships in three steps: import, CLI client, then serve and the
lock.

## How the existing system works, per this document

RoughCut is a desktop video editor in a Tauri shell. Every capability is a
row in one tool registry, reached through `tools::dispatch` with a `Caller`
(Ui, Mcp, Agent) that decides who may call what. The in-app agent may not
create projects, export or change preferences. An MCP server runs only inside
the app on 127.0.0.1 with a token and writes `<data dir>/mcp.json`.
`mcp-shim` connects Claude Code over stdio to that server. MCP calls to export
or delete wait for approval in the app (refused after two minutes;
`ROUGHCUT_NO_CONFIRM=1` turns this off). Import takes only a local file path,
non-MP4 files get a playable preview copy (ADR-0008), and the app starts
on-device Whisper transcription after an import. Setup installs whisper and
llama-server outside the registry (ADR-0004), and the product promises no
network traffic by default.

## Findings

### 🟡 C1: "accepted with decision A" points nowhere (437–438)

Nothing is labelled decision A: the Decisions table has no labels and
Alternative A is about captions. Point to the "Where downloads go" row.

### 🟡 C2: "invariant 4 / 5 / 7" has no source (48, 619, 671)

The numbers come from `.claude/skills/roughcut-elephant/SKILL.md`. The
README's numbered principles use different numbers (there, 4 is "one tool
registry" and zero egress is 1). Name the source or state each rule in place.

### 🟡 C3: Approvals when the CLI opened the app in the background (284–292, CUJ 3, Goals 4–5)

`open -g` keeps the window behind, so an export or delete sent from a script
with the app closed hits the problem Alternative B names ("the prompt sits in
a window nobody is looking at"). Say what happens (window comes forward,
refused, or 120 s wait), and whether the app stays running after the CLI
exits.

### 🟡 C4: How a client learns who holds the lock (285, 702–703, 270)

The holder JSON is written when the lock is taken and never cleared, so after
`serve` exits it still says "serve" unless the CLI also tests the lock. On
Windows `File::try_lock` uses `LockFileEx` (mandatory byte-range locks), so
another handle can't read the locked file: "on failure it reads that JSON"
(315) and the unit test (769) would fail on the Windows CI job.

### 🟡 C5: Step 2 depends on Step 3 (488–490 vs item 25, 702–703)

Step 2's launch rule checks the library lock, which only exists from Step 3.

### 🟡 C6: Three names for the destination folder (17, 62, 137, 151, 169, 446, 849)

"RoughCut's media folder" can be read as `<data dir>/media/`, the hidden
folder Alternative E rejects. Use one term, "the download folder".

### 🟡 C7: Which platforms get the CLI (152, 289, 480, 734, 742)

It ships inside RoughCut.app and release CI is macOS-only, yet CI builds it
on three OSes and the launch step covers Linux/Windows. Say whether Goals 4–5
are macOS-only in this release.

### 🟢 C8: Tool counts (43, 587)

66 rows, one meta: 65 open to MCP today, 66 after. "67 rows (MCP 64)"
implies three hidden rows.

### 🟢 C9: Undefined terms

"RoughCut" is never introduced; "the owner"; "HyperFrames"; "EGM";
`meta: false`; "Hosted mode"; `<data dir>`.

### 🟢 C10: ADR references

ADR-0005 is cited (85, 403) but missing from Additional Resources; ADR-0007 is
listed but not cited; no ADR is linked by path.

### 🟢 C11: Wording that assumes existing code

"approvals on" (the `require_confirm` flag); `TerminalSink` (the event-sink
abstraction); `start_with(.., None)` (no `mcp.json` written); what makes an
`mcp.json` "fresh".

### 🟢 C12: Vague behaviour

What decides `Unavailable` vs `InvalidArg` (522); what drops a call (199);
whether "nothing new runs at startup" (72) covers only network traffic or
also the `--version` checks behind `SetupStatus`.

### 🟢 C13: `project_id` with a different video (186–187, 563)

The refusal comes only after the whole download.

### 🟢 C14: Drafting leftovers in `.gitignore` (747)

`discussion_summary.md` and `design_draft.md` are unexplained.
