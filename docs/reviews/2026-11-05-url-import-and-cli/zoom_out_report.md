# Stage 5: Initial Zoom Out Review

**Target:** `docs/designs/url-import-and-cli.md` (full file, lines 1–853)
**Reviewer:** `egm-zoom-out` (fresh context), standard `zoom-out-standard.md`
**Status:** complete. Verdict: no Blockers; revise before implementation
(5 Major, 5 Nit).

## Findings

### 🟡 Z1: `roughcut serve`, the library lock and terminal approvals are speculative scope (YAGNI)

*   **Lines:** 20–24, 67–70, 125–131, 152–154, 175–178, 268–271, 294–322,
    397–415, 453–455, 478–479, 685–694, 760–790.
*   **Discrepancy:** The motivating workflow is a link or file from a script
    or Claude Code. Auto-launch (Goal 5) already covers "the app is closed";
    `serve` is only needed with no GUI session at all (SSH), a need the
    Problem section never states.
*   **Cost:** a second shell around `Editor`; a cross-process lock (app vs
    app is already handled by `tauri_plugin_single_instance`); a
    `TerminalSink` with a stdin approval queue; signal handling and
    `mcp.json` ownership; an admitted rollout gap; CLI install/update
    commands; all of `roughcut-core` linked into the CLI.
*   **Hidden gap:** `cli/Cargo.toml` does not enable `whisper-native`, which
    the app does (`app/src-tauri/Cargo.toml:18`, `core/Cargo.toml:38`), so
    `serve` would transcribe through the whisper-cli fallback, not "as
    against the app" (CUJ 4).
*   **Recommendation:** ship Steps 1–2; move Step 3 to its own design when a
    concrete no-GUI need exists.

### 🟡 Z2: Problem 1 stays unsolved for the clients it names; launch-on-demand is in the wrong component

*   **Lines:** 33–35, 110, 152, 284–289, 385–395, 824.
*   **Discrepancy:** "With the app closed, Claude Code's RoughCut tools fail
    to connect" is fixed only in the new CLI; `mcp-shim` is "unchanged", and
    CUJ 2 still needs the app open. The shim already has a discover / retry /
    re-discover loop (`mcp-shim/src/main.rs:96–143`).
*   **Recommendation:** put launch-and-wait and endpoint discovery in one
    shared place used by the shim and the CLI. Today the CLI would use a new
    `discovery_file()` in core while the shim keeps its own path logic
    (`mcp-shim/src/main.rs:46–49`), two copies that can drift.

### 🟡 Z3: Conflicts with ADR-0004 and architecture principle 1, and the design's reasoning is inconsistent

*   **Lines:** 46–49, 71–73, 149, 251, 269, 324–331, 345–363, 407–415,
    799–813.
*   **Discrepancy:**
    *   ADR-0004 (lines 21–40) rejects exactly this kind of registry row: "a
        … tool in the registry would let a confused agent loop — or any MCP
        client — pull gigabytes from an arbitrary URL".
    *   07-architecture principle 1 and ADR-0007 say egress is started or
        enabled by a human, in the app.
    *   The plan creates ADR-0009 but amends neither ADR-0004 nor principle
        1. The CLI's `install/update yt-dlp` contradicts ADR-0004's "the
        setup screen is the only provisioning surface".
*   **Inconsistency:** Alternatives C and H reject agent access and `--yes`
    because a model may be "misled by something it read"; the same argument
    applies to Claude Code calling `import_url` with no approval.
*   **Recommendation:** keep the owner's decision but reconcile it: (a)
    supersede the relevant parts of ADR-0004 and update principle 1, or (b)
    follow ADR-0007: one user-set switch in the app ("let connected clients
    import links").

### 🟡 Z4: The JavaScript-runtime lookup rebuilds `resolve_binary` and contradicts "one copy the app installs"

*   **Lines:** 207, 233–238, 249, 524–525, 532, 657–659, 838–839.
*   **Discrepancy:** `adapters/video.rs:99–126` already has
    `resolve_binary(name, env_var)`. The design specifies a wider search (`~/.deno/bin`,
    newest nvm, volta) and version parsing for two runtimes. yt-dlp enables
    only Deno by default and recommends it (EJS wiki).
*   **Recommendation:** Deno only, found with
    `resolve_binary("deno", "DENO_PATH")` (or installed by Setup into
    `<data dir>/bin/`). Drop the Node / nvm / volta search and version
    parsing.

### 🟡 Z5: Progress and project-id plumbing ignore existing seams

*   **Lines:** 221–224, 516–520, 555–560, 563–564, 569–574.
*   **Discrepancy:** `core/src/progress.rs::stream_progress` is the existing
    seam for child-process progress (with whole-percent dedup). Adapters take
    the sink (`render_mp4(job, sink)`). The design adds an `on_progress`
    callback, a second throttle, an up-front project id and
    `create_project_with_id`. Other downloads send `project_id: None`; the
    import screen filters progress by task only (`EmptyState.tsx:152–158`).
*   **Recommendation:** `fetch(url, dir, sink)` using `stream_progress` with
    `(UrlImport, None)`, then the existing `create_project(name, Some(path))`.

### 🟢 Z6: Filename handling rebuilds what yt-dlp and the export module already do

*   **Lines:** 169–170, 211, 225–228, 434–436, 526–527, 561–562, 579–580.
*   yt-dlp's default template is `%(title)s [%(id)s].%(ext)s`, with
    `--windows-filenames`, `--trim-filenames`, `-P temp:` and
    `--print after_move:filepath`. Export already has `slug()`. A `-2` copy
    of `<title> [<id>].mp4` is the same video downloaded twice.
*   **Recommendation:** if `<title> [<id>].mp4` exists, reuse it; keep the
    temp folder only for cleanup after a failure.

### 🟢 Z7: The `project_id` option on `import_url` is YAGNI

*   **Lines:** 182–187, 267, 563, 605. No CUJ uses it, and a different video
    is refused only after a possibly multi-GB download.
*   **Recommendation:** drop `project_id` / `--project-id` from v1.

### 🟢 Z8: Implementation detail in the Technical Plan

*   **Lines:** 199–238, 310–313, 526–527. The full command line, format
    selector, search paths and toolchain versions belong in the
    implementation plan; the architecture should state the invariants.
*   **Wrong detail:** in `--progress-template "download:%(progress._percent_str)s"`,
    `download:` is yt-dlp's type prefix, so printed lines carry only the
    percentage, yet file 3 parses lines that start with `download:`. With
    `bv*+ba` the percentage probably restarts at 0 for the audio stream.

### 🟢 Z9: Extra CLI surface

*   **Lines:** 270, 288–289, 704–705, 751–753. `roughcut status` matters
    only with `serve`; Linux and Windows launch code is planned though the
    CLI ships only in RoughCut.app; the `/usr/local/bin` link adds an admin
    prompt where a copyable path or a user-writable folder would do.

### 🟢 Z10: Context accuracy and unstated gaps

*   Line 43 "63 are open to MCP": the registry has 66 rows with one meta row,
    so 65 are open to MCP; line 587 should read 67 rows, 66 open to MCP.
*   Lines 439–442: `install_llama_server` downloads with `sha256: None`
    (`llm_runtime.rs:147–156`), so "the same trust" overstates it.
*   Line 322: `core/examples/llm_status.rs` opens the real library through
    `bootstrap_with_store` (only matters if the lock stays).
*   `~/Movies/RoughCut/` is the removed recorder's folder (ADR-0008).
*   Demo mode is not covered: `--ffmpeg-location` has no value when ffmpeg
    is missing; other adapters pick mock or real per call (`Auto*`).

## Reuse the design gets right

`download::download_to` with sha256; `reserve_path`;
`ProgressTask::RuntimeInstall`; `setup.rs` for provisioning; the `ingest.ts`
seam; one registry row with `Caller` policy unchanged;
`Editor::resolve_confirmation`; three shippable steps.
