# Goldfish Annotated Review Report (Stages 5–7)

Status: resolved (fixes applied to the design on 2026-11-05)

**Target:** `docs/designs/url-import-and-cli.md` (full file; FeatureRange
1–485 and the Detailed Implementation Plan where fixes cascade)

Findings come from `zoom_out_report.md` (Z), `comprehension_report.md` (C)
and `critic_report.md` (S = style, T = structural, D = design). Findings that
make the same point are resolved together. The owner set a standing
instruction to finish the review without pausing, so proposed fixes were
applied as "auto accept"; escalations are listed first for the owner.

## Escalated to the owner (applied, but the owner should confirm)

### E1: An enabling step for link import (D1 🔴, Z3 🟡)

*   **Finding:** ADR-0004 rejects a registry download tool; ADR-0007 says
    only a human action in the app can enable egress. The owner chose
    "app + MCP, no approval" (decision row 2).
*   **Resolution:** PROPOSE FIX that keeps the owner's choice of no
    per-download approval: installing yt-dlp in Setup is the human action
    that turns link import on (an app-only shell command, ADR-0004), and
    "Remove" turns it off. Setup states that connected clients can then
    import links. New ADR-0009 amends ADR-0004's download point and
    principle 1. Added bounds: one video per call (playlists refused), live
    streams refused, loopback / private-network hosts refused for non-UI
    callers, `download_dir` changeable only in the app, never overwrite.
*   **Owner check:** a separate "let connected clients import links" switch
    (default off) was the reviewers' other option; it would make MCP and CLI
    imports fail until the switch is turned on.

### E2: `roughcut serve` stays (Z1 🟡)

*   **Finding:** `serve` and the lock are speculative; auto-launch covers
    "app closed".
*   **Resolution:** CONTEST the removal: the owner chose "both" CLI modes
    (Decisions row 5; `discussion_summary.md` decision 2). PROPOSE FIX for
    the valid parts: the Problem now states the no-window need (SSH, a Mac
    with nobody logged in); the CLI enables `whisper-native`; `serve`'s
    limits are listed (D8); `serve` stays Step 3, built last.

## Applied fixes (PROPOSE FIX)

| Ids | Change |
| --- | ------ |
| D1, Z3, S2, S3, T2, C2 | Cite `docs/07-architecture.md` principle 1 by name, not "invariant 4". Goal 7 and ADR-0009 state only what this feature adds, and list install, update and import. ADR-0009 amends ADR-0004 and principle 1. The zero-egress check covers the yt-dlp child process. |
| D2, D-nit2, S5 | A metadata pass before any download: refuse playlists and channels, refuse live and upcoming streams with a stated message, read the title, duration, size and formats. The download pass then takes the single video's own link. |
| D3, S6 | Format sorting caps every fallback at 1080p and prefers H.264/AAC; remux to MP4; keep the real extension; warn when the video isn't H.264. |
| D4 | One guard owns the yt-dlp child and the temporary folder. Stale `.roughcut-*` folders older than a day are removed at the start of each import. App quit and `serve` exit stop running imports. A refused step after the move deletes the moved file. |
| D5 | The download runs in its own task, so a client that stops waiting doesn't stop it (the `find_clips` scan precedent); the project appears when it finishes. Alternative I's reason is revised. |
| Z2, D6 | Launch-and-wait and endpoint discovery go in one shared library in `mcp-shim`, used by both the shim and the CLI, so Claude Code and Claude Desktop also open the app when it's closed. |
| Z4, D10 | Deno only, installed by Setup next to yt-dlp and found with the existing `resolve_binary`. The Node / nvm / volta search is dropped. The version is a date, so Setup warns when it's old without any network; a failed import suggests Update. |
| Z5, D-nit1, D-nit3 | `fetch(url, dir, sink)` uses `progress::stream_progress` with `(UrlImport, None)`. Progress is computed from bytes across the video and audio streams. `create_project_with_id` is dropped. |
| Z6 | yt-dlp names the file (`<title> [<id>].<ext>`, Windows-safe, trimmed) with `-P temp:` / `-P home:` and `--no-overwrites`. An existing file for the same video is reused. `safe_file_stem` and `reserve_path` are dropped. |
| Z7, C13, D-nit4 | `project_id` is dropped from `import_url` and `--project-id` from the CLI. |
| Z8, T-nit11 | The command line, search paths and versions move to the implementation plan. The Technical Plan keeps the invariants. |
| Z9, C7, S7 | Launching the app and installing the CLI are macOS-only in this release; elsewhere a closed app means exit 3. `roughcut status` moves to Step 3. |
| Z10, C8, D-nit8 | Counts are 66 rows / 65 open to MCP today, 67 / 66 after. The llama-server trust claim is removed. `core/examples/llm_status.rs` is noted under the lock. The old recorder folder and demo mode are covered. |
| C1, S8, T1 | "Decision A" is replaced with a reference to the Risks table. |
| C3, D-nit10 | When an MCP client asks for approval, the app brings its window forward. The app stays open after the CLI exits. Unattended export is stated as a limit. |
| C4, D7 | The lock holder's details go in a sibling `library.lock.json`; `try_lock` alone decides who holds the library. |
| C5 | Step 2 launches the app without a lock check; Step 3 adds the check. |
| C6, S1, T4 | One term, "the download folder", and one temporary-folder name. |
| C9, C10, T-nit5 | RoughCut, the owner, `<data dir>`, Hosted mode, `meta` and NLE are defined. "HyperFrames" is dropped. ADRs and docs are linked by relative path, and the video by URL. |
| C11, C12 | Explained: "approvals on" (`require_confirm`), the event sink, `start_with(.., None)`, a fresh `mcp.json` (newer than the launch), the `Unavailable` / `InvalidArg` split, and "nothing new runs at startup" (no network, no binary run). |
| C14 | The drafting files are deleted, not git-ignored. |
| S4 | `mcp.json` names the endpoint of whichever process last started; it can be stale after a crash. |
| T3 | The diagram is redrawn: only the clients that launch have the launch branch, and both dispatch paths lead into `Editor`. |
| T5, D11 | A Risks & Mitigations table: egress, prompt injection, disk use, yt-dlp breakage, legal / distribution, privacy, rollback. Log events `url_import_start / end / failed` record the host and yt-dlp version only. |
| T6 | New journeys: install yt-dlp and Deno in Setup (with a checksum mismatch), and install the CLI. CUJ 5 is split. |
| D8 | `serve`'s limits are listed (Hosted key prompt, no managed llama-server, approvals need a terminal); `serve` starts the app log. |
| D9 | Approvals in `serve` carry an id and a deadline; input typed after a timeout is thrown away; with several waiting, the answer names the id. |
| D-nit5, D-nit11 | `source_url` is carried over on relink; losses on `append_media` and on an older build saving are stated. |
| D-nit6 | `/usr/local/bin` is created if missing (admin prompt); the link targets the bundle and Setup offers "Relink" if the app moved; the CLI launches with `open -g -j`; `current_exe` is resolved through the link. |
| D-nit7 | Update re-runs the install: the latest release, checked against `SHA2-256SUMS`, from `releases/latest/download/<asset>` (no API call). The unsupported "-U checks checksums" claim is removed. GPG signature checking goes to Future Improvements. |
| D-nit9 | Setup reads versions from files written at install time; nothing runs a binary to get its status. |
| T-nit1, T-nit2, T-nit4, T-nit6, T-nit7, T-nit8, T-nit9, T-nit10, S-nits 1–14 | Applied: split CUJ 5; "Components and data flow" placed before Decisions; a rule before NFR; Status line; H4 labels; CLI synopsis rows; one name ("Import from a link"); Alternatives state technical reasons; wording and subjects fixed. |

## Declined (Nit, recorded)

*   **T-nit3:** Additional Resources and Future Improvements are H3s under
    the NFR H2 because the EGM template puts them there.
*   **Z9 (part):** the `/usr/local/bin` admin prompt stays. A user-writable
    folder isn't on macOS's default PATH, and the prompt is a one-time
    Setup click (VS Code's "install the code command" works the same way).

# Stage 8 resolutions (intermediate zoom out)

| Id | Category | Resolution |
| -- | -------- | ---------- |
| I1 🟡 | PROPOSE FIX + ESCALATE | Step 2's CLI depends only on the shim's library; `roughcut-core` comes in Step 3. A new Open Question asks the owner to confirm `serve` (E2) and re-weigh Alternative F before Step 3. |
| I2 🟡 | PROPOSE FIX (owner check) | `out_dir` is kept (the owner chose "Preferences + out_dir") but must resolve inside the download folder, so the `download_dir` guard means something. |
| I3 🟡 | PROPOSE FIX | yt-dlp and Deno are only `<data dir>/bin/yt-dlp` and `<data dir>/bin/deno` (the `llm_runtime` pattern); a file there means installed; the JSON gives versions only; Deno is unpacked with the existing `tar` call. |
| I4 🟢 | PROPOSE FIX | The H.264 warning comes from `Media.codec`; `vcodec` and its print are dropped. |
| I5 🟢 | PROPOSE FIX | One cleanup rule: on failure the guard kills yt-dlp and removes the folder; on quit or exit yt-dlp is killed and the next import's sweep removes the folder. Goal 3 matches. |
| I6 🟢 | PROPOSE FIX | Progress is tracked per stream by `format_id` ("video 1/2: 42%"). |
| I7 🟢 | PROPOSE FIX | One host rule for every caller; the Risks row says the check reads the link as written. |
| I8 🟢 | PROPOSE FIX | `launch.rs` owns the send-and-retry loop as well as discovery. |
| I9 🟢 | DECLINE | `roughcut import-url` stays: it is CUJ 4's one-line form of the owner's goal, and it only chains two `call`s. |
| I10 🟢 | PROPOSE FIX | The default is `~/Movies/RoughCut/Downloads/`, separate from the removed recorder's files; the Overview says why. |

# Stage 9 resolutions (implementation readiness)

Step 1 was built while Stage 9 ran, so most Step 1 findings were resolved in
the code and then written into the plan (Step 1 items 1–10 now describe what
was built). Step 2 and Step 3 findings are recorded in the design under
"Before Steps 2 and 3: open readiness findings": they must be decided before
those steps start.

| Ids | Category | Resolution |
| --- | -------- | ---------- |
| R-B1 | PROPOSE FIX (done) | `download(probe, home, temp, sink, on_pid)`; the engine creates the temporary folder; one spawned task downloads, creates the project and sets `source_url`. |
| R-B2, C-B1 | PROPOSE FIX (done) | Readiness lives behind the seam (`UrlFetcher::ready`); `MockFetcher` is always ready, so tests pass in demo mode; tests set a temporary `download_dir` as the UI; a separate test fetcher covers "link import is off". |
| S-B1 | PROPOSE FIX (done) | The sweep removes `.roughcut-*` folders no running import owns; Goal 3, the plan and the live check agree. |
| R-M1, C-M4 | PROPOSE FIX (done) | The format arguments are in every run, the probe included; the fields read are named. |
| R-M2 | PROPOSE FIX (done) | Both pipes go through `stream_progress`; `RCFILE` / `RCPROG` prefixes; stderr is kept for the `ERROR:` line; exit 0 without `RCFILE` falls back to the `[id]` file; a title-and-size message comes first. |
| R-M3 | PROPOSE FIX (done) | yt-dlp gets its own process group; `cancel_imports` kills the group (`kill -s KILL -- -<pgid>`, checked on macOS with a throwaway group). |
| R-M4, C-M2 | PROPOSE FIX (done) | `Fetched.reused`; only a file this import created is deleted; reuse adds a warning. |
| R-M5, C-M10, C-M11, S-M3 | PROPOSE FIX (done) | Exact host list with unit tests; `is_loopback_url` unchanged (asserted); `out_dir` expands `~/`, refuses `..`, resolves relative paths inside the folder. Symlinks inside the folder aren't followed (recorded). |
| R-M6 | PROPOSE FIX (done) | `UrlImport { project: Project, file_path, title, source_url: webpage_url, warnings }`. |
| R-M7, C-M3 | PROPOSE FIX (done) | Staged, checked, made executable and run before replacing; `link-import.json` written last; hash-file parsing tested; checksum mismatch message mapped. Straddling a new release between two downloads is accepted (a retry fixes it). |
| R-M8, C-M5 | PROPOSE FIX (done) | `private_out` replaces links, the video id and the title before `ErrText`; unit test. |
| R-M9, C-M13 | PROPOSE FIX (done) | `LinkImportField` in a "+ From link" popover and under the drop zone, with its own progress and error. |
| R-M10–R-M14, C-M6–C-M9, C-M12, S-M5 | ESCALATE to before Steps 2–3 | Written into the design's "Before Steps 2 and 3" list; Step 3 marked blocked on E2. |
| S-M1, S-M2, S-M4, S-M6, S-M7 | PROPOSE FIX (done) | Rewritten in the as-built plan: progress formula, development-only overrides, unpack wording; per-step gates follow the step order. |
| R-N3, C-N4 | PROPOSE FIX (done) | The H.264 check matches the codec's start (`h264…`, `avc…`), so the mock's `"h264 (demo)"` doesn't warn. |
| C-N6 | PROPOSE FIX (done) | `url_import` added to `tool-api.md`'s task list; log tables in `07-architecture.md` and `logging.md`. |
| R-N2 | PROPOSE FIX (done) | `append_media` drops `source_url` (a joined file has no single link); stated. |
| Other nits | DECLINE | Recorded; wording-only. |
