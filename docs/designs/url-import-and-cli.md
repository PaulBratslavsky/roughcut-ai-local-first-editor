# URL import and the `roughcut` CLI

*   **Author:** Paul Bratslavsky (the owner)
*   **Status:** Approved (EGM review 2026-11-05). Step 1 implemented on
    `feat/url-import`; Steps 2–3 to do
*   **Date:** 2026-11-04 (revised 2026-11-05 after the EGM review, Stages 5–7)

--------------------------------------------------------------------------------

## Overview

RoughCut is a local-first desktop video editor (a Tauri app with a Rust core):
talking-head footage in, a transcript-driven rough cut out, exported to
Premiere Pro, DaVinci Resolve or Final Cut Pro, or rendered to MP4. This
design adds two ways for a program, not only a person at the app window, to
bring video in and edit it:

1.  **Import from a link.** A new registry tool, `import_url`, downloads one
    video from a link (YouTube, or another site yt-dlp supports) into the
    *download folder* with yt-dlp, then creates a project around it like a
    file import. The transcript comes from on-device Whisper, as for any
    import. The app gets an "Import from a link" field next to "open a
    video". Link import is off until the user installs yt-dlp in Setup.
2.  **A `roughcut` command-line program** (macOS in this release). It calls
    the same tool registry the app and MCP clients use. By default it talks
    to the running app's MCP endpoint and opens the app if it is closed;
    Claude Code and Claude Desktop, through `mcp-shim`, get the same
    behaviour. `roughcut serve` runs the editing engine with no window, for
    sessions where the app can't open.

Neither changes how editing works: every capability is still one registry
row, and every caller still goes through `tools::dispatch` with its `Caller`
(the UI, an MCP client, or the in-app agent).

Terms used below: `<data dir>` is RoughCut's own folder
(`~/Library/Application Support/roughcut/` on macOS). The *download folder*
is where link imports are saved (default `~/Movies/RoughCut/Downloads/`, a
subfolder so downloads don't mix with recordings left in `~/Movies/RoughCut/`
by the removed in-app recorder, [ADR-0008](../adr/0008-remove-in-app-recording.md)). *Hosted
mode* is the user-enabled switch that sends AI steps to Claude
([ADR-0007](../adr/0007-hosted-model-mode.md)). An *NLE* is a video editor
RoughCut exports to (Premiere Pro, DaVinci Resolve, Final Cut Pro).

## The Problem

Programs can't easily get video into RoughCut or reach its editing tools:

*   **MCP works only while the app runs.** The MCP server starts inside the
    Tauri shell; with the app closed, Claude Code's RoughCut tools fail to
    connect, and nothing opens the app for them.
*   **Import takes a file path only.** To edit a video that is online, the
    user downloads it with another tool, finds the file and imports it. An AI
    client can't go from a link to an edit on its own.
*   **There is no command line.** A shell script or a scheduled job can't call
    RoughCut without speaking MCP's JSON-RPC itself.
*   **No window means no RoughCut.** Over SSH, or on a Mac where nobody is
    logged in to a desktop session, the app can't open at all.

This blocks the workflow the owner wants: hand RoughCut a link or a file from
a script or a Claude Code session, have it transcribed and cut, and read the
result (a project id, an export path) from the tool's answer. The editing
tools for that already exist (65 of the 66 registry tools are open to MCP);
the gaps are getting the video in and reaching the engine.

The main risk: a link import fetches content from a site the caller names.
[`docs/07-architecture.md`](../07-architecture.md) principle 1 says the app
makes no network traffic unless the user starts it, and
[ADR-0004](../adr/0004-provisioning-outside-the-tool-registry.md) keeps
download tools out of the registry because "any MCP client" could use them.
This design changes both rules on purpose (see "The egress rule" and
[ADR-0009](../adr/0009-url-import-egress.md)).

## Goals

1.  One call turns a link into a project with its video attached:
    `import_url` from the app, MCP or the CLI. Download progress shows on the
    app's import screen (`Progress` events) and in `roughcut serve`'s
    terminal; the CLI prints the elapsed time while it waits, because MCP
    returns only the final result.
2.  The downloaded video is ordinary media: transcription, cuts, shorts and
    every export work on it as on an imported file. The project keeps the
    link it came from.
3.  A failed download (bad link, private video, playlist, live stream,
    network drop) creates no project and leaves no partial file in the
    download folder, and the error gives the reason. A download cut off by
    quitting the app leaves only a hidden temporary folder, which the next
    import removes.
4.  On macOS, every MCP-visible tool can be called from the CLI: it prints
    the tool's JSON result on stdout and exits non-zero on an error, so
    scripts can chain calls.
5.  On macOS, with the app closed, the CLI, `mcp-shim` and therefore Claude
    Code and Claude Desktop still work: they open the app in the background
    and wait for its endpoint. Elsewhere they exit with "start RoughCut
    first".
6.  The app and `roughcut serve` never edit the same library at the same
    time.
7.  This feature adds network traffic only when the user installs, updates
    or removes yt-dlp and Deno in Setup, or when the user or a client the
    user connected asks for a link import. It adds no network traffic and
    runs no new program at app start.

## Non-Goals

*   **YouTube captions as the transcript.** The owner chose Whisper for its
    word timings; captions are not downloaded.
*   **Playlists, channels, or several links per call.** One video per
    `import_url`; a playlist link is refused; a script can loop.
*   **Private or sign-in-only videos.** No cookies, browser profiles or
    account sign-in are passed to yt-dlp, so videos behind a sign-in or a bot
    check fail with yt-dlp's reason.
*   **Attaching a download to an existing project.** `import_url` always
    creates a new project.
*   **Uploading or publishing** to YouTube or any other site.
*   **Remote access.** The endpoint stays on 127.0.0.1 with its token; serving
    other machines is [ADR-0005](../adr/0005-online-editor-architecture.md)'s
    online-editor path.
*   **Checking download rights.** RoughCut can't know whether the user may
    download a video; the import field says to import only videos the user
    has the right to use.
*   **Graphics, music or captions burned into the video.**
*   **The CLI on Linux and Windows.** It builds there (CI), but launching the
    app and installing the CLI are macOS-only in this release.
*   **DaVinci Resolve changes.** A separate design (the owner's to-do list).

## Open Questions

*   **Before Step 3**: the owner confirms `roughcut serve` (review escalation
    E2) and re-weighs Alternative F for a macOS-only release (reusing the
    app binary for `serve` instead of a second engine binary).

The owner decided the other design questions while drafting (2026-11-04);
the review's escalations are in
[the review report](../reviews/2026-11-05-url-import-and-cli-review.md)
(E1, E2).

## Critical User Journeys (CUJs)

### CUJ 1: Turn link import on

*   **Given**: the app is open; yt-dlp is not installed.
*   **When**: the user opens Setup and clicks "Install" on the "Import from
    a link" row.
*   **Then**: Setup downloads yt-dlp and Deno into `<data dir>/bin/`, checks
    each against its published SHA-256, and shows both versions. The row says
    that the user and any connected client (Claude Code, scripts using
    `roughcut`) can now import links. A checksum mismatch deletes the
    download, installs nothing and shows "checksum mismatch for yt-dlp: not
    installed".

### CUJ 2: Paste a link in the app

*   **Given**: link import is on.
*   **When**: the user pastes a YouTube link into "Import from a link".
*   **Then**: the import screen shows the video's title and a progress bar.
    When the download finishes, a project named after the title opens and
    transcription starts, as after a file import.

### CUJ 3: Claude Code goes from a link to a cut

*   **Given**: Claude Code is connected to RoughCut through `mcp-shim`; the
    app is closed; link import is on.
*   **When**: the user says "import this link, cut the silences, export a
    Premiere XML".
*   **Then**: the shim opens the app in the background and Claude calls
    `import_url`, `transcribe`, `remove_silences` and `export`. The export
    needs the user's approval, so the app brings its window to the front
    with the approval prompt.

### CUJ 4: A shell script with the app closed

*   **Given**: the app is closed; `roughcut` is installed.
*   **When**: a script runs `roughcut import-url <link> --transcribe`.
*   **Then**: the CLI opens the app in the background, waits for its
    endpoint, and prints the `import_url` result, then the `transcribe`
    result, as JSON. The app stays open afterwards.

### CUJ 5: Install the command line tool

*   **Given**: the app is open.
*   **When**: the user clicks "Install command line tool" in Setup.
*   **Then**: macOS asks for an administrator password once, and
    `/usr/local/bin/roughcut` then points at the copy inside RoughCut.app.
    Setup shows "Relink" if the app has moved since.

### CUJ 6: No window (headless)

*   **Given**: an SSH session on the Mac, with no app window possible.
*   **When**: the user runs `roughcut serve`, then calls tools with `roughcut`
    or through `mcp-shim` from another terminal.
*   **Then**: the calls work as they do against the app, within `serve`'s
    limits (see "`roughcut serve`"). An export asks for approval in `serve`'s
    terminal. Opening the app while `serve` runs shows "RoughCut is running
    headless" and doesn't open a second editor on the library.

### CUJ 7: A link that can't be imported

*   **Given**: link import is on.
*   **When**: `import_url` gets a playlist, a live stream, a private video or
    a broken link.
*   **Then**: no project is created and nothing is left in the download
    folder. The error says why: "this link is a playlist; import one video at
    a time", "this is a live stream; import it after it ends", or yt-dlp's own
    `ERROR:` line.

### CUJ 8: Link import is off

*   **Given**: yt-dlp is not installed.
*   **When**: an MCP client or the CLI calls `import_url`.
*   **Then**: the call fails with "link import is off: install yt-dlp in
    RoughCut's Setup (only you can turn it on)". Nothing is fetched.

## Technical Plan (Architecture)

### Components and data flow

```text
 app UI ──Tauri invoke──► tools::dispatch(Caller::Ui) ──────────────┐
                                                                    ▼
 Claude Code / Desktop ─stdio─► mcp-shim ─┐                       Editor ──► library.db
 roughcut call / import-url ──────────────┤                         ▲
     (both use launch.rs: endpoint        │                         │
      unreachable → open the app,         ▼                         │
      wait for mcp.json, retry)   HTTP + token ─► MCP server ─► tools::dispatch(Caller::Mcp)
                                                  (in the app, or in `roughcut serve`)

 import_url ─► Editor::import_url ─► UrlFetcher (yt-dlp + Deno from <data dir>/bin)
     1. metadata pass: refuse playlists and live streams; read title, size, formats
     2. download pass into <download folder>/.roughcut-<uuid>/  (removed on every exit path)
     3. yt-dlp moves the file to <download folder>/<title> [<id>].<ext>
     4. create_project(title, file) ─► project with media.source_url = the link
```

*   `launch.rs` is a small library in the `mcp-shim` crate shared by the shim
    and the CLI: endpoint discovery; sending a request, retrying only when
    the request never reached the app (so an export is never sent twice);
    and, on macOS, opening the app and waiting for it.
*   The app and `roughcut serve` are two shells around the same `Editor`.
    Each takes the library lock before it builds the `Editor`, then starts the
    MCP server, which writes `<data dir>/mcp.json`. The lock means only one of
    them is live; `mcp.json` names the endpoint of whichever started last and
    can be stale after a crash, so clients treat a refused connection as
    "not running".

### Decisions

| Question | Decision |
| -------- | -------- |
| Transcript of a linked video | On-device Whisper, as for any import (Alternative A). |
| Who may call `import_url` | The app (`Caller::Ui`) and MCP clients (`Caller::Mcp`), with no per-download approval (Alternatives B, C). Not the in-app agent (`agent: false`). Link import is on only after the user installs yt-dlp in Setup. |
| How yt-dlp is installed | The app installs its own yt-dlp and Deno into `<data dir>/bin/` from Setup, checked against the published SHA-256 sums; "Update" reinstalls the latest (Alternative D). |
| Where downloads go | The download folder, set in Setup (default `~/Movies/RoughCut/Downloads/`); `import_url` takes an optional `out_dir` for one call, which must be the download folder or a folder inside it (Alternative E). |
| CLI packaging | A new crate `cli/` builds `roughcut`, which ships inside RoughCut.app and is symlinked into `/usr/local/bin` from Setup. In Step 2 it is a small client over `mcp-shim`'s shared library; Step 3 adds the engine for `serve` (Alternative F). |
| App and headless on one library | An exclusive OS file lock: the first process to take it wins, the second refuses with a message naming the holder (Alternative G). |
| Approvals in `roughcut serve` | Asked in `serve`'s terminal, each with an id and a deadline; refused when `serve` has no terminal (Alternative H). |
| Progress over MCP | None. The download outlives the request instead (Alternative I). |

### The egress rule

Principle 1 today: the app makes no network traffic unless the user starts
it, and only a human action in the app can *enable* egress (ADR-0007).
[ADR-0009](../adr/0009-url-import-egress.md) changes it for link import:

*   **The user turns it on.** Installing yt-dlp in Setup is the enabling
    action. It is a Tauri shell command, outside the registry, so no client
    can do it ([ADR-0004](../adr/0004-provisioning-outside-the-tool-registry.md)
    still holds for installers). "Remove" turns link import off.
*   **Then clients may import without asking.** Once it is on, the app and MCP
    clients call `import_url` with no per-download approval. That is the
    owner's choice: unattended scripts are the point of the feature. ADR-0009
    amends ADR-0004's "no download tool in the registry" for this one tool.
*   **Bounds on every call.** One video (playlists and channels refused); no
    live streams; only `http`/`https`; no `localhost`, loopback,
    private-network or link-local host named in the link (a name that
    resolves to such an address, or a redirect to one, isn't caught); never
    overwriting a file; the download folder changeable only in the app, and
    `out_dir` limited to folders inside it.

### Import from a link

#### The tool

`import_url { url, name?, out_dir? }`, `agent: false`, `meta: false` (meta
tools are the few, such as the in-app agent's entry point, that MCP clients
can't call).

*   It creates a new project named `name` if given, otherwise the video's
    title, and returns `{ project, file_path, title, source_url, warnings }`.
    `warnings` carries notes such as "the video isn't H.264; Premiere may
    not open it".
*   It doesn't transcribe. In the app, the import screen starts
    transcription after the call, as it does after a file import; MCP clients
    call `transcribe`; the CLI's `import-url --transcribe` calls it next.
*   `out_dir`, when given, must be the download folder or a folder inside it
    (created if missing), so a script can sort its downloads without writing
    anywhere else. The file name always comes from yt-dlp.

#### Checks before anything is fetched

*   Link import is on (the app's own yt-dlp exists in `<data dir>/bin/`);
    otherwise `Unavailable` with the CUJ 8 message.
*   ffmpeg is present (yt-dlp needs it to merge video and audio); otherwise
    `Unavailable`. In demo mode (fixture adapters, no toolchain) the tool
    returns the same error.
*   The link is `http` or `https` and doesn't name a loopback,
    private-network or link-local host, and `out_dir` is inside the download
    folder; otherwise `InvalidArg`. (`InvalidArg`
    is a bad request; `Unavailable` is a missing tool or a site that can't
    be reached.)

#### Fetching

Two yt-dlp runs, both with the user's yt-dlp config ignored, no cookies, the
link after `--` (so it can't be read as an option), and Deno passed
explicitly:

1.  **Metadata pass** (no download): refuse a playlist or channel, and a
    video whose live status is live or upcoming, with the CUJ 7 messages.
    Read the title, the video's own link, the estimated size and the formats
    the download will use.
2.  **Download pass** on the video's own link: prefer H.264 video and AAC
    audio up to 1080p, falling back to the best available up to 1080p,
    remuxed to MP4 when possible. Files are written to a temporary folder,
    `<download folder>/.roughcut-<uuid>/`, and yt-dlp moves the finished file
    to `<download folder>/<title> [<video id>].<ext>` (Windows-safe, title
    trimmed). If that file already exists, yt-dlp keeps it and RoughCut uses
    it: the name contains the video id, so it is the same video.

Progress events (`ProgressTask::UrlImport`, no project id) come from the
download pass through the existing `progress::stream_progress` seam. Each
names the stream and its percent ("video 1/2: 42%"), because yt-dlp downloads
video and audio one after the other.

The engine creates the temporary folder, and one spawned task downloads,
creates the project and records its link; the tool call awaits that task. If
the caller stops waiting (a client timeout, an interrupted CLI), the task
still finishes and the project still appears, as a `find_clips` scan does
today. yt-dlp runs in its own process group, because its standalone build is
a launcher that runs the real yt-dlp (and ffmpeg) as children. When the
download fails, the temporary folder is removed. When the app quits or
`serve` exits, `Editor::cancel_imports` kills each running import's process
group; the hidden temporary folder may stay behind, so each `import_url`
first removes `.roughcut-*` folders that no running import owns.

#### After the download

`create_project(title, file)` probes and attaches the file. If that fails,
the file is deleted, unless it was already there before this import (the same
video, reused): then it may belong to another project. `Media.source_url`
records the video's own link (the probe's `webpage_url`); a relink of the
same video keeps it, and `append_media`, which makes a joined file, drops
it. ADR-0008's playable-copy path applies as for any import;
when the probed video codec (`Media.codec`) isn't H.264, the result carries a
warning.

#### Installing yt-dlp and Deno

This stays outside the registry, in `setup.rs`, next to the whisper and
llama-server installs ([ADR-0004](../adr/0004-provisioning-outside-the-tool-registry.md)).
Setup's "Import from a link" row has Install, Update and Remove, the two
versions, and the download folder picker.

*   **Install / Update**: download the latest yt-dlp standalone build and the
    latest Deno for this machine from their GitHub releases, check each
    against the release's published SHA-256, put them in `<data dir>/bin/`,
    and record their versions in a file. Update repeats the install. It is
    all or nothing: both are staged in a folder, checked, made executable and
    run once with `--version` before either replaces the copy in use, so a
    failure (a checksum mismatch, a download error, a binary that won't
    start) leaves the previous state.
*   **Status** reads the recorded versions; it never runs the binaries. A
    yt-dlp version is a date, so Setup marks it "may be out of date" after 60
    days without any network check; a failed import suggests Update.
*   **Remove** deletes both binaries and turns link import off.

### The `roughcut` CLI

```text
roughcut tools                          list the tools MCP clients may call
roughcut call <tool> [<json> | -]       call one tool; arguments from the argument or stdin
roughcut import-url <url> [--name N] [--out-dir DIR] [--transcribe]
                                        import_url, then transcribe if asked
roughcut serve                          run the engine headless and serve MCP (Step 3)
roughcut status                         which endpoint it would use; who holds the library (Step 3)

Global: --endpoint URL --token T (pin an endpoint, as for the shim), --no-launch
```

*   **Client commands** speak the same JSON-RPC as `mcp-shim` (`tools/list`,
    `tools/call`) with the Bearer token, using `launch.rs` for discovery:
    flags, then `ROUGHCUT_MCP_ENDPOINT` / `ROUGHCUT_MCP_TOKEN`, then
    `<data dir>/mcp.json`.
*   **Output**: the tool's result JSON on stdout. On a tool error, the error
    JSON goes to stderr with exit code 1. Bad usage exits 2. When RoughCut
    can't be reached or started, the CLI exits 3.
*   **Waiting**: no request timeout. When stderr is a terminal, a line such
    as `import_url … 1m12s` is rewritten each second.
*   **Opening the app** (macOS, `launch.rs`): when the endpoint refuses the
    connection and `--no-launch` isn't set, open the app hidden
    (`open -g -j` on the bundle that contains the CLI, else by bundle id).
    Then wait up to 60 s for an `mcp.json` newer than the launch whose
    endpoint answers `tools/list`. From Step 3, a library held by `serve`
    means "don't launch": exit 3 if `serve` doesn't answer.
*   **Calls from the CLI are MCP calls** (`Caller::Mcp`): the CLI gets what
    Claude Code gets. Export and delete still ask for approval: the app
    brings its window to the front to ask (from Step 2), or `serve` asks in
    its terminal.
*   **Unattended export is not possible**: a person must approve it, and with
    nobody to answer it is refused after two minutes.
    `ROUGHCUT_NO_CONFIRM=1` stays a test and CI switch for processes started
    directly; an app opened by `open` doesn't see it.

### `roughcut serve`

1.  Take the library lock as `serve`, or exit 3 naming the holder.
2.  Build the `Editor` as the app does (`Editor::bootstrap`), with
    confirmations required (`require_confirm`), the app log started, and an
    event sink (the core's callback for events) that writes to the terminal.
3.  Start the MCP server and print the endpoint.
4.  **Events**: each `Progress` event becomes one line on stderr.
5.  **Approvals**: each request prints an id, its summary and its deadline.
    One answer applies to one request: with one waiting, `y` or `n`
    answers it; with several waiting, the answer must name the id. When a
    request times out, `serve` prints "denied (timed out)" and throws away
    anything typed for it. With no terminal on stdin, every request is
    denied at once, and `serve` says so.
6.  **Exit** (Ctrl-C, SIGTERM): stop running imports, delete `mcp.json` if
    it still names this endpoint, then exit; the OS releases the lock.

**`serve`'s limits** (the app has these; `serve` doesn't):

*   The managed llama-server starts only from a Setup click in the app.
    `serve` uses whatever model server is configured and running (Ollama or
    another).
*   Hosted mode reads the API key from the macOS Keychain, which may ask the
    user to allow access for the `roughcut` program. Over SSH nobody can
    answer, so Hosted AI steps fail there.
*   Approvals need a terminal (above).

### One library, one editor

The lock is an exclusive OS file lock on `<data dir>/library.lock` (the
standard library's `File::try_lock`). Only that lock decides who holds the
library. After taking it, the holder writes `{ holder: "app" | "serve",
pid, since }` to a separate file, `library.lock.json`, which is used only
for messages (on Windows a locked file can't be read by another process). A
crash releases the lock; a stale `library.lock.json` then names a dead holder
and is ignored, because `try_lock` succeeds.

*   **The app** takes the lock before building the `Editor`. When `serve`
    holds it, the app shows "RoughCut is running headless (`roughcut serve`,
    process N). Stop it, then open RoughCut again." and quits. A second app
    can't get that far: `tauri-plugin-single-instance` stops it first.
*   **`serve`** exits 3 when the app holds the lock: "RoughCut is open;
    `roughcut` commands use it directly".
*   **Not covered**: tests (in-memory stores), and the developer examples in
    `core/examples/` (for example `llm_status.rs`), which open the real
    library without the lock.

### Docs

[ADR-0009](../adr/0009-url-import-egress.md) records the egress rule above.
[`docs/zero-egress-verification.md`](../zero-egress-verification.md) adds
the new traffic (GitHub release downloads for yt-dlp and Deno; the link's
site and the hosts yt-dlp contacts for it) and checks the yt-dlp child
process as well as the app: with no import running, no yt-dlp process
exists. [`docs/07-architecture.md`](../07-architecture.md) principle 1
gains the link-import exception.

## Alternatives Considered

### Alternative A: YouTube's captions as the transcript

*   **Pros**: No Whisper run; works on a machine without a model.
*   **Cons**: Automatic captions have rougher word timings than Whisper's,
    and uploaded captions have only line timings. RoughCut's cuts land on
    word timings.
*   **Reason for Rejection**: Cut precision depends on word timings, and
    Whisper's are the most accurate RoughCut has.

### Alternative B: Approve every download an MCP client asks for

*   **Pros**: The user sees each download before it starts; reuses the
    approval prompt that MCP export and delete use.
*   **Cons**: Unattended scripts stall and are refused after two minutes.
*   **Reason for Rejection**: Unattended use from scripts and Claude Code is
    the point of the feature. The user's one-time install in Setup is the
    human action that enables it, and each call is bounded (see "The egress
    rule").

### Alternative C: Let the in-app agent import links

*   **Pros**: "Import this link" works in RoughCut's own chat.
*   **Cons**: Breaks the rule that the agent never creates projects, exports
    or changes preferences.
*   **Reason for Rejection**: The `Caller` policy stays as it is; MCP clients
    and the UI cover the need.

### Alternative D: Use yt-dlp from Homebrew (alone, or as the first choice)

*   **Pros**: No download or update code in RoughCut; `brew upgrade` keeps it
    current.
*   **Cons**: No Homebrew means no link import; a Homebrew copy may lack the
    JavaScript runtime YouTube needs; using both copies makes "which yt-dlp
    ran?" a support question. The app's own copy is also only as new as its
    last Update, but Setup can see its age and warn.
*   **Reason for Rejection**: One copy, installed and dated by the app, with
    Deno beside it, works without Homebrew and can be checked for age.

### Alternative E: Save downloads in a fixed folder

*   **Pros**: Nothing to configure.
*   **Cons**: The hidden `<data dir>/media/` folder hides gigabytes from the
    user; a fixed visible folder can't be moved off a small internal disk.
*   **Reason for Rejection**: A download folder set in Setup, with a visible
    default and `out_dir` per call, covers both for one setting.

### Alternative F: Make the app binary the CLI, or grow `mcp-shim` into it

*   **Pros**: The app binary already contains the engine; a grown shim stays
    small.
*   **Cons**: The release app is built as a windowed program on Windows, so a
    CLI run from it prints nothing, and CLI startup would share code with app
    startup. A grown shim would still need the app binary for `serve`.
*   **Reason for Rejection**: A separate `roughcut` binary with the engine
    behaves the same everywhere and leaves the shim's name alone. The shim
    and the CLI share `launch.rs`, so launching isn't written twice.

### Alternative G: Let the app take over from, or attach to, a running `serve`

*   **Pros**: Opening the app would never show "stop serve first".
*   **Cons**: Taking over has to wait for, or cut off, a long job in `serve`.
    Attaching needs an event stream from another process into the app's UI,
    which MCP doesn't have: roughly step 1 of
    [ADR-0005](../adr/0005-online-editor-architecture.md).
*   **Reason for Rejection**: "First one wins" can't lose an edit; a
    forgotten `serve` costs a message, not data.

### Alternative H: `serve --yes`, or `--yes` plus terminal prompts

*   **Pros**: A background `serve` could export and delete for clients.
*   **Cons**: `--yes` approves every request, deletes included, for any
    client, including one misled by text it read.
*   **Reason for Rejection**: Approvals in `serve` behave like approvals in
    the app: a person answers them.

### Alternative I: Progress over MCP (streamed notifications or a polling tool)

*   **Pros**: Scripts and Claude Code would see download and transcription
    progress, and a client that resets its timeout on progress wouldn't give
    up.
*   **Cons**: Streaming needs the hand-written MCP server to hold responses
    open and match each event to its call. A `get_progress` tool adds a
    registry row, docs, mock coverage and polling.
*   **Reason for Rejection**: Detaching the download from the request solves
    the timeout problem (the project appears even if the client gave up), and
    the app and `serve`'s terminal show progress. Listed under Future
    Improvements.

--------------------------------------------------------------------------------

## Non-Functional Requirements

### Risks and mitigations

| Risk | Mitigation, or why it is accepted |
| ---- | --------------------------------- |
| A client misled by text it read (prompt injection) starts downloads | Link import is on only after the user installs yt-dlp; one video per call; no live streams; no loopback or private host named in the link; files never overwritten; files only inside the download folder. Accepted: once it is on, any connected client can download any public video there; a DNS name or redirect that leads to a private address isn't caught (the check reads the link as written). |
| Disk use: a video can be several GB | The metadata pass's size estimate is shown in the progress message. Leftover temporary folders are removed on the next import. Accepted: no size cap in v1; deleting a project keeps its video, as today. |
| yt-dlp stops working when YouTube changes | Setup shows yt-dlp's age and suggests Update after 60 days or after a failed import. The log records failures by host and yt-dlp version. |
| Legal and distribution | RoughCut doesn't bundle yt-dlp (youtube-dl's repository was taken down by a DMCA notice in 2020); the user downloads it at runtime from yt-dlp's own releases. The import field tells users to import only videos they have the right to use. |
| Privacy | The link is stored in the project (`source_url`) and returned by the tool, on this Mac only. The log records the host and yt-dlp's version, never the full link or the title. |
| Integrity of yt-dlp and Deno | Each download is checked against the SHA-256 its release publishes, over HTTPS. That catches a corrupted download, not a compromised release. |
| Rollback | All additions. An older build ignores `Media.source_url` and `Preferences.download_dir` and drops them when it saves; an older app doesn't take the library lock, so an old app next to a new `serve` is unprotected until both are updated. |

### Performance

Download time is set by the network. Merging H.264 video with AAC audio is a
stream copy with no re-encode. Setup's status reads two small files and runs
nothing.

--------------------------------------------------------------------------------

### Additional Resources

*   The video that prompted this:
    [youtube.com/watch?v=KD71g4TK6Sg](https://www.youtube.com/watch?v=KD71g4TK6Sg),
    an editing workflow driven from Claude Code.
*   yt-dlp: [EJS wiki page](https://github.com/yt-dlp/yt-dlp/wiki/EJS)
    (JavaScript runtimes, `--js-runtimes`) and
    [releases](https://github.com/yt-dlp/yt-dlp/releases); Deno
    [releases](https://github.com/denoland/deno/releases).
*   [ADR-0004](../adr/0004-provisioning-outside-the-tool-registry.md)
    (provisioning outside the registry),
    [ADR-0005](../adr/0005-online-editor-architecture.md) (online editor),
    [ADR-0007](../adr/0007-hosted-model-mode.md) (user-enabled egress),
    [ADR-0008](../adr/0008-remove-in-app-recording.md) (remove in-app
    recording; item 3: non-MP4 media get a playable copy).
*   [`docs/07-architecture.md`](../07-architecture.md),
    [`docs/zero-egress-verification.md`](../zero-egress-verification.md),
    [`core/src/mcp/README.md`](../../core/src/mcp/README.md),
    [`mcp-shim/src/main.rs`](../../mcp-shim/src/main.rs).
*   The review: [2026-11-05-url-import-and-cli-review.md](../reviews/2026-11-05-url-import-and-cli-review.md).

### Future Improvements

*   **Progress over MCP**: MCP progress notifications during a call
    (Alternative I).
*   **Several links per call**: playlists, or a list of links, one project
    each.
*   **Verify yt-dlp's GPG signature** (`SHA2-256SUMS.sig`), not only the
    checksum.
*   **Run `serve` as a login service** (a launchd agent).
*   **The CLI on Linux and Windows**: launching the app and installing the
    command.
*   **DaVinci Resolve Studio**: a separate design.

--------------------------------------------------------------------------------

## Detailed Implementation Plan

Three steps, each shippable on its own and tested before the next:
(1) `import_url` with its Setup row and UI, (2) the CLI's client commands and
launching in `mcp-shim`, (3) `roughcut serve` and the library lock.

### Target Files to Create/Modify

#### Step 1: import from a link (implemented on `feat/url-import`)

Built and tested as listed; the review's Stage 9 findings for Step 1 are
resolved in the code (see the review report).

#### 1. `core/src/model.rs`, `core/src/events.rs` (Modify)

*   `Media.source_url: Option<String>` and `Preferences.download_dir:
    Option<String>`, both `#[serde(default)]`; `source_url: None` in every
    `Media` literal. `UrlImport { project: Project, file_path: String, title:
    String, source_url: String (the probe's webpage_url), warnings:
    Vec<String> }`. `ProgressTask::UrlImport`.

#### 2. `core/src/adapters/fetch.rs` (Create), `core/src/adapters/mod.rs` (Modify)

*   `#[async_trait] trait UrlFetcher`: `ready()` (link import on and ffmpeg
    present; `MockFetcher` is always ready, so tests pass under demo mode and
    `FABLE_DEMO=1`), `version()`, `probe(url)`, `download(probe, home, temp,
    sink, on_pid)`. `Probe { webpage_url, id, title, duration, size_approx,
    streams: Vec<(format_id, "video" | "audio")> }`; `Fetched { file, reused }`.
*   `YtDlpFetcher`:
    *   Every run: `--ignore-config --no-playlist --color no_color`,
        `--js-runtimes deno:<path>` when Deno is installed, `--ffmpeg-location
        <ffmpeg>`, and the format choice `-f "bv*+ba/b" -S
        "res:1080,vcodec:h264,acodec:aac" --merge-output-format mp4
        --remux-video mp4` (in the probe too, so its streams and size match
        the download). The link is the last argument, after `--`.
    *   The probe adds `-J --flat-playlist` and reads `_type`,
        `live_status` / `is_live`, `id`, `title`, `webpage_url`, `duration`,
        and `requested_formats[]` (`format_id`, `vcodec`, `filesize` /
        `filesize_approx`; summed for `size_approx`).
    *   The download adds `-P home:<dir> -P temp:<temp> -o
        "%(title).150B [%(id)s].%(ext)s" --windows-filenames --no-overwrites
        --newline --progress --progress-template "download:RCPROG
        %(info.format_id)s %(progress.downloaded_bytes)s
        %(progress.total_bytes)s %(progress.total_bytes_estimate)s" -O
        "after_move:RCFILE %(filepath)s"`. Both pipes go through
        `progress::stream_progress`. `RCFILE` lines give the file. Non-progress
        stderr is kept for `error_reason`, which returns the last `ERROR:` line
        without its `[site] id:` tag. Exit 0 with no `RCFILE` falls back to
        the newest `[<id>].` file in `home`.
    *   The child runs in its own process group, with `kill_on_drop`.
*   `ytdlp_path()` / `deno_path()`: `<data dir>/bin/yt-dlp|deno[.exe]`, or
    `YTDLP_PATH` / `DENO_PATH` (development only). `versions_file()`
    (`link-import.json`), `recorded_versions()`, `OFF_MESSAGE`.
*   `MockFetcher`: links containing `playlist`, `live` or `fail` behave like
    those cases; others write `Mock video [mock123].mp4`.
*   Unit tests: argument lists, probe refusals and streams, progress across
    two streams, error lines, find-by-id.

#### 3. `core/src/engine/mod.rs`, `core/src/engine/media.rs`, `core/src/engine/projects.rs` (Modify)

*   `Inner.fetcher` (default `YtDlpFetcher`; `set_fetcher_for_tests`;
    `test_instance` uses `MockFetcher`), `Inner.imports` (import id → process
    group id), `cancel_imports()` (`kill -s KILL -- -<pgid>`; `taskkill /F /T`
    on Windows).
*   `import_url(url, name, out_dir)` as in the Technical Plan:
    *   `check_link` refuses non-`http(s)`, unparseable links and
        `names_local_host` hosts.
    *   `folder_inside` expands `~/`, refuses `..` and resolves a relative
        `out_dir` inside the download folder.
    *   It sweeps temporary folders, probes, then spawns the task that
        downloads and runs `finish_url_import` (create the project, set
        `source_url`, add the warnings).
    *   It logs the `url_import_*` events through `private_out`, which
        replaces links, the video id and the title.
    *   The relink branch of `import_media` keeps `source_url`.
    *   `download_folder_path(prefs)` is public, for Setup.
*   `projects.rs`:
    *   `update_preferences` refuses a `download_dir` change from non-UI
        callers.
    *   `names_local_host` refuses `localhost`, `*.localhost` and `*.local`,
        and these addresses: loopback, private, link-local (including
        169.254.169.254), unspecified (0.0.0.0, `::`), carrier-grade NAT
        (100.64/10), IPv6 ULA (fc00::/7) and link-local (fe80::/10), and
        IPv4-mapped forms of these.
    *   `is_loopback_url` (the `inference_endpoint` guard) is unchanged.

#### 4. `core/src/tools.rs`, `docs/tool-api.md` (Modify)

*   The `import_url` row (`agent: false, meta: false`; 67 rows, 66 open to
    MCP), whose description tells clients it doesn't transcribe, needs link
    import on, refuses playlists, live streams and local hosts, and keeps
    going if the caller stops waiting. Its `tool-api.md` entry, and
    `url_import` in the progress-task list.

#### 5. `core/src/applog.rs` (Modify)

*   `UrlImportStart { host, ytdlp }`, `UrlImportEnd { host, ms, bytes }`,
    `UrlImportFailed { host, ms, code, error }`.

#### 6. `core/src/setup.rs` (Modify)

*   `SetupStatus.link_import: LinkImportStatus { installed, ytdlp, deno,
    outdated, download_dir, supported }`, read from files only.
*   `install_link_import(editor)`:
    *   It downloads into a staging folder: yt-dlp hashed against
        `SHA2-256SUMS`, Deno's zip against its `.zip.sha256sum` (hashes
        lowercased; either file shape parsed by `hash_for`).
    *   It unpacks with `tar -xf` (`unzip` on Linux) and makes both
        executable.
    *   It runs both with `--version`, then moves both into `bin/` and
        writes `link-import.json` last.
    *   A `download_to` hash failure becomes "checksum mismatch for <tool>:
        not installed".
*   `remove_link_import()`.
*   `ytdlp_is_old` reads `YYYY.MM.DD[.N]`: more than 60 days old means
    `outdated`; any other format is never flagged.

#### 7. `core/tests/e2e.rs` (Modify)

*   Through the registry with `MockFetcher` and a temporary `download_dir`
    (set as the UI):
    *   An MCP import into a subfolder: title, name, `source_url`, file
        inside, no temporary folder left, transcribable.
    *   Reuse of the same video, with its warning.
    *   Refusals before any fetch: `file:`, `-o…`, `127.0.0.1`,
        `192.168.1.2`, `localhost`, a playlist, a live stream, an `out_dir`
        of `/tmp` or `../`.
    *   A failed download leaves no project, file or temporary folder.
    *   `download_dir` refused from MCP; the agent isn't offered the tool.
    *   Link import off gives the CUJ 8 error with nothing fetched.
    *   A leftover temporary folder is swept; a relink keeps `source_url`.

#### 8. `app/src-tauri/src/lib.rs` (Modify)

*   `install_link_import` / `remove_link_import` commands;
    `cancel_imports()` in the `RunEvent::Exit` handler (through
    `try_state`).

#### 9. Frontend (Modify / Create)

*   Regenerated types (`UrlImport`, `LinkImportStatus`, `Media`,
    `Preferences`, `ProgressTask`, `SetupStatus`), re-exported in
    `types.ts`.
*   `api.ts`: `installLinkImport`, `removeLinkImport`, and the mock
    `link_import` status.
*   `mockApi.ts`: `import_url` and the `nolinks` switch.
*   `ingest.ts`: `ingestUrl`.
*   `panels/LinkImportField.tsx` (Create): a link field with progress, the
    error, "open setup" when link import is off, and the rights note.
    Used under the drop zone in `EmptyState.tsx` and in a "+ From link"
    popover in `TopBar.tsx`.
*   `SetupPanel.tsx`: the "Import from a link" row (Install / Update /
    Remove, versions, "may be out of date", the download folder with
    Choose…, the consent text) and the footer sentence.
*   `styles.css`: on the `:root` tokens.

#### 10. Docs (Modify / Create)

*   `docs/adr/0009-url-import-egress.md` (Create).
*   `docs/zero-egress-verification.md`: the link-import section and the
    child-process check.
*   `docs/07-architecture.md`: principle 1 and the log events table.
*   `docs/designs/logging.md`: the events table.
*   `CONTEXT.md`: link import, download folder, `UrlFetcher`.
*   `core/src/README.md`, `core/src/adapters/README.md`,
    `core/src/engine/README.md`: the EGM contract READMEs.
*   `docs/designs/SITEMAP.md`: ADR-0009 and this design's row.

#### Step 2: the CLI client and launching

#### 20. `mcp-shim/src/launch.rs` (Create), `mcp-shim/src/lib.rs` (Create), `mcp-shim/src/main.rs`, `mcp-shim/Cargo.toml` (Modify)

*   **Proposed Changes**: move `discover()` and the send-and-retry loop
    (`send`, which re-discovers and retries only when the request never
    reached the app) into `launch.rs`, and add `launch_app() ->
    Result<Endpoint>` for macOS:
    1.  Find the bundle from `current_exe()` with symlinks resolved, else
        use the bundle id.
    2.  Run `open -g -j`.
    3.  Poll for an `mcp.json` newer than the launch whose endpoint answers
        `tools/list`, for up to 60 s.

    Other platforms return "start RoughCut first". The crate gains a `lib`
    target. The shim calls `launch_app()` when a send is refused and it
    isn't pinned with flags.

#### 21. `Cargo.toml` (workspace), `cli/Cargo.toml` (Create)

*   **Proposed Changes**: add member `cli` and `clap` (derive). `clap`'s
    usage errors exit 2, matching the CLI's codes. Crate `roughcut-cli`,
    binary `roughcut`. In Step 2 it depends only on `roughcut-mcp-shim`
    (lib), `clap` and `serde_json`; Step 3 adds `roughcut-core` with
    `whisper-native` (as the app does) and `tokio` for `serve`.

#### 22. `cli/src/main.rs`, `cli/src/client.rs` (Create)

*   **Proposed Changes**: the command tree and exit codes; `tools`, `call`
    and `import-url` through `launch.rs`'s `send`; the elapsed-time line;
    and `import-url --transcribe`. The tool result arrives as the
    `content[0].text` JSON and is printed as-is.

#### 23. `app/src-tauri/src/lib.rs` (Modify)

*   **Proposed Changes**:
    *   On a `ConfirmRequest` event, show, unminimise and focus the main
        window, so a hidden app can be answered.
    *   `install_cli` / `cli_status`. `install_cli` runs `osascript … with
        administrator privileges` to create `/usr/local/bin` if missing and
        symlink `roughcut` to `RoughCut.app/Contents/MacOS/roughcut`.
        `cli_status` reports missing / ok / points elsewhere ("Relink").

#### 24. `app/src-tauri/tauri.bundle.conf.json` (Create), `scripts/build-macos-app.sh`, `.github/workflows/release.yml`, `.github/workflows/ci.yml`, `.gitignore` (Modify)

*   **Proposed Changes**:
    *   `tauri.bundle.conf.json`: `{"bundle": {"externalBin":
        ["binaries/roughcut"]}}`, used only by release builds. Tauri's build
        script fails when a listed external binary is missing, so dev
        builds and CI's `cargo build -p roughcut-app` don't list it.
    *   The build script and the release workflow build `roughcut-cli`, copy
        it to `app/src-tauri/binaries/roughcut-aarch64-apple-darwin`, and
        pass `--config tauri.bundle.conf.json`. The release also attaches
        `roughcut`.
    *   CI builds and tests `roughcut-cli` on all three OSes.
    *   `.gitignore` ignores `app/src-tauri/binaries/`.

#### 25. `frontend/src/panels/SetupPanel.tsx`, `frontend/src/ipc/api.ts` (Modify)

*   **Proposed Changes**: a "Command line tool" row: Install / Relink / the
    installed path (macOS only).

#### 26. Docs for Step 2

*   `cli/README.md` (Create): an EGM contract README.
*   `mcp-shim` comments: the launch behaviour.
*   `README.md`: a CLI quick start.
*   `docs/07-architecture.md`: the CLI and launching.
*   `core/src/mcp/README.md`: discovery moved to `launch.rs`.

#### Step 3: headless and the lock (blocked on the owner's E2 decision)


#### 27. `core/src/library_lock.rs` (Create), `core/src/lib.rs` (Modify)

*   **Proposed Changes**: `acquire(Holder::App | Holder::Serve) ->
    Result<LibraryLock, Held>`:
    *   Opens `<data dir>/library.lock` without truncating and calls
        `try_lock`.
    *   On success, writes `library.lock.json`; on failure, reads it for the
        message.
    *   `peek() -> Option<HolderInfo>` lets the CLI test the lock: it calls
        `try_lock` and releases at once.
    *   Unit tests: a second `acquire` fails and the holder JSON names the
        first; it succeeds after the first guard drops; a stale JSON with a
        free lock is ignored.

#### 28. `cli/src/serve.rs` (Create), `cli/src/main.rs`, `cli/Cargo.toml` (Modify)

*   **Proposed Changes**: add `roughcut-core` (`whisper-native`) and `tokio`.
    `serve` as in "`roughcut serve`":
    *   A `TerminalSink`.
    *   An approval queue with ids, deadlines and one stdin reader. A
        timed-out request is marked denied and its later input is thrown
        away.
    *   SIGINT / SIGTERM handling: `cancel_imports()`, then `mcp.json`
        cleanup.
    *   `status` (endpoint plus `library_lock::peek`).
    *   The launch rule gains "held by `serve` → don't launch".

#### 29. `app/src-tauri/src/lib.rs` (Modify)

*   **Proposed Changes**: take the lock in `setup` before
    `Editor::bootstrap` and keep the guard in `AppState`. When `serve` holds
    the lock, show a dialog (`tauri-plugin-dialog`, already a dependency),
    then exit.

#### 30. Docs for Step 3

*   `docs/07-architecture.md`, `CONTEXT.md`: `serve` and the lock.
*   `core/README.md`, `cli/README.md`: `serve`'s limits.

### Before Steps 2 and 3: open readiness findings

The Stage 9 review found these gaps in the Step 2 and Step 3 plans. Each must
be decided, and written into the items above, before that step starts.

*   **`launch.rs` interface (Step 2)**:
    *   `discover(flags)` takes the endpoint and token as parameters rather
        than reading process args.
    *   `send` returns a typed error that tells "never reached the app"
        (CLI exit 3) from a JSON-RPC error, an HTTP 401, or a tool result
        with `isError: true` (exit 1).
    *   Launch also when `mcp.json` is missing (a fresh install, or after
        `serve` exits). Env-var pins skip launching, like flag pins.
    *   Launch on the first refused connection, not after the shim's
        retries.
*   **Tests for Step 2**:
    *   The `mcp.json` path is injectable.
    *   `roughcut-core` and `tokio` are dev-dependencies for a
        `start_with(.., None)` test server.
    *   CI runs `cargo test -p roughcut-mcp-shim`.
    *   Named cases cover "no retry once the request reached the app" and
        the launch wait.
*   **Release build (Steps 2–3)**:
    *   The build script uses the host's target triple.
    *   The CLI build step sets `MACOSX_DEPLOYMENT_TARGET=11.0` and the
        clang runtime `RUSTFLAGS`, and runs before `tauri build`.
    *   The app's bundle id is `dev.localfirst.roughcut`.
    *   `install_cli`'s AppleScript uses `quoted form of`.
    *   `cli_status` handles an app running from a translocated path.
*   **`serve` approvals (Step 3)**:
    *   `CoreEvent::ConfirmRequest` gains a deadline, and the core sends an
        event when a request times out, so `serve` doesn't copy the
        120-second constant.
    *   Answers are a short index (`1 y`), with a bare `y` when one request
        waits.
    *   `serve` ignores `ROUGHCUT_NO_CONFIRM`, so it can't become the
        `--yes` that Alternative H rejected.
    *   The workspace's tokio gains the `signal` and `io-std` features
        (`cfg(unix)` for SIGTERM).
*   **App lock (Step 3)**:
    *   Take the lock in `run()` before the Tauri builder.
    *   Show the "running headless" message with the non-blocking dialog
        API, then exit before any state is managed.
    *   `acquire` reports I/O errors separately from "held", and retries
        briefly so a `peek()` can't make the app quit.

### Verification Plan

*   **Build**:
    *   `cargo build --workspace`.
    *   `cd frontend && npx tsc -b && npm run build`.
    *   `scripts/build-macos-app.sh` (Step 2) produces an app with
        `Contents/MacOS/roughcut`.
*   **Tests**:
    *   `cargo test -p roughcut-core`: the fetch, `import_url`, preference
        guard and lock tests, plus the parity guards.
    *   `cargo test -p roughcut-mcp-shim -p roughcut-cli`: discovery,
        argument parsing, result unwrapping and exit codes, against a test
        server from `mcp::server::start_with(.., None)` (`None` writes no
        `mcp.json`).
*   **Static analysis**: `cargo clippy --workspace --all-targets` adds no
    new warnings.
*   **Live checks** (manual, on the Mac):
    1.  Setup → Install: the yt-dlp and Deno versions show.
    2.  Paste
        [youtube.com/watch?v=KD71g4TK6Sg](https://www.youtube.com/watch?v=KD71g4TK6Sg):
        the file lands in `~/Movies/RoughCut/Downloads/`, the project is named after
        the title, transcription runs, and the source link is kept.
    3.  A playlist link and a live link are refused, and nothing is left
        behind.
    4.  Quit during a download: no `yt-dlp` process remains, and the next
        import removes the temporary folder.
    5.  App closed: `roughcut import-url <link> --transcribe` opens the app
        hidden and prints JSON. A Claude Code session through the shim also
        opens it.
    6.  `roughcut serve` over SSH: `roughcut call list_projects '{}'` from
        another terminal works; opening the app shows the "running
        headless" dialog; an MCP `export` prompts in `serve`'s terminal with
        an id.

--------------------------------------------------------------------------------

*Drafted with EGM*
