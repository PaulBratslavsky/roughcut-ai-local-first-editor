# EGM review: URL import and the `roughcut` CLI

**Target:** [`docs/designs/url-import-and-cli.md`](../designs/url-import-and-cli.md)
**Date:** 2026-11-05
**Mode:** consolidated (all five stages run without stopping, at the owner's
standing instruction), with fixes applied after Stage 7, Stage 8 and
Stage 9.
**Stage reports:** [`2026-11-05-url-import-and-cli/`](2026-11-05-url-import-and-cli/)
(each stage's findings, and `goldfish_review_report.md` with every
resolution).

## Outcome

The design went from **1 Blocker** (Stage 7) to none: its main conflict, a
registry tool that downloads without approval, is now reconciled with
ADR-0004 and ADR-0007 in a new ADR-0009 instead of slipping past them. Step 1
(link import) was implemented and tested on `feat/url-import` while Stage 9
ran; Stage 9's Step 1 findings are fixed in the code. Step 2 and Step 3
findings are listed in the design as decisions to make before those steps.

| Stage | Reviewers | Blocker | Major | Nit | Report |
| ----- | --------- | ------- | ----- | --- | ------ |
| 5 Initial zoom out | zoom-out | 0 | 5 | 5 | `zoom_out_report.md` |
| 6 Comprehension | comprehension | 0 | 7 | 7 | `comprehension_report.md` |
| 7 Critic (feature spec) | design, structural, style | 1 | 24 | 36 | `critic_report.md` |
| 8 Intermediate zoom out | zoom-out | 0 | 3 | 7 | `intermediate_zoom_out_report.md` |
| 9 Implementation readiness | readiness, codebase details, style | 4 | 34 | 36 | `implementation_readiness_report.md` |

## Escalated to the owner

Applied as described; the owner should confirm or change them.

*   **E1: an enabling step for link import.** The owner chose "the app and
    MCP clients, no per-download approval". To meet ADR-0007 ("only a human
    action in the app enables egress"), installing yt-dlp in Setup is that
    action, and Remove turns link import off. The reviewers' other option
    was a separate switch, off by default, that would make MCP and CLI
    imports fail until the user turns it on.
*   **E2: `roughcut serve`.** Two zoom-out passes called `serve` and the
    library lock speculative. They stay, because the owner chose "both" CLI
    modes. Step 3 is marked blocked until the owner confirms, and re-weighs
    Alternative F (reuse the app binary for `serve`) now that the CLI is
    macOS-only.
*   **`out_dir`.** It is kept (the owner's "Preferences + out_dir"), but
    limited to folders inside the download folder, so the rule that only the
    user moves the download folder still means something.

## Main changes to the design

*   **Egress.** ADR-0009; install-as-consent; bounds on every call: one video
    (playlists refused), no live streams, no local or private hosts, files
    only in the download folder, nothing overwritten; the log keeps the host
    only.
*   **Correctness.**
    *   A metadata pass runs before any download.
    *   Format sorting caps every fallback at 1080p, and the real file
        extension is kept.
    *   The download outlives the request (MCP clients time out).
    *   yt-dlp runs in its own process group, so a quit stops it.
    *   One cleanup rule for the temporary folder.
    *   The Setup install is all or nothing.
    *   Deno is installed with yt-dlp.
*   **Reuse.**
    *   `progress::stream_progress`, `create_project` and yt-dlp's own file
        naming are used.
    *   The `llm_runtime` pattern is followed for the app's binaries, with
        no PATH or Homebrew lookup.
    *   One `launch.rs` is shared by the shim and the CLI, so Claude Code
        also gets "open the app when it's closed".
*   **Scope.**
    *   `project_id` was dropped from `import_url`.
    *   Step 2's CLI is a small client; the engine comes with Step 3.
    *   Launching the app and installing the CLI are macOS-only in this
        release.
*   **Docs.**
    *   One term ("download folder").
    *   Linked ADRs.
    *   A Risks and Mitigations table.
    *   New journeys for Setup install, CLI install, refusals and "off".
*   **Changed after drafting.** The default folder is
    `~/Movies/RoughCut/Downloads/`, so downloads don't mix with the removed
    recorder's files.

## Step 1 verification

*   `cargo test -p roughcut-core`: 166 unit tests (1 ignored), 30 end-to-end
    tests, and both parity tests pass. The new tests cover the fetch
    adapter, the hosts, the log scrubbing, `out_dir`, the checksum files, the
    version dates, `import_url` through the registry (MCP, the UI, refusals,
    failure cleanup, reuse, the leftover sweep, relink, and link import off).
*   `cargo check -p roughcut-app` and the frontend build pass, with no new
    clippy warnings in the changed files.
*   Checked in the browser build (on the mock): importing from a link on the
    empty screen and from the top-bar popover, the playlist refusal, the
    `nolinks` "off" path through Setup, and the Setup row.
*   **Not yet checked live:** a real yt-dlp download. Running yt-dlp outside
    the app was blocked in the review session, so the first real run is the
    owner's: Setup → Install, then paste a link.
