# Stage 9: Implementation Readiness Test

**Target:** `docs/designs/url-import-and-cli.md`, DetailedRange (lines
609–968 of the version reviewed)
**Reviewers:** `egm-implementation-readiness` and `egm-codebase-details`
(standard `implementation-readiness-standard.md`), `egm-style` (persona
rules), each in a fresh context.
**Status:** complete. Step 1 was being implemented in the same worktree
during the review; the reviewers flagged where the code had already departed
from the plan text, and those decisions are now written into the plan.

| Reviewer | Verdict | Blocker | Major | Nit |
| -------- | ------- | ------- | ----- | --- |
| implementation-readiness | not ready | 2 | 14 | 10 |
| codebase-details | not ready | 1 | 13 | 11 |
| style | revise | 1 | 7 | 15 |

## Blockers

*   **R-B1 (readiness): the fetcher seam and the import guard disagree** about
    who owns the process and the temporary folder; the project must be created
    inside the spawned task, or a dropped request leaves a file with no
    project.
*   **R-B2 / C-B1 (readiness, codebase): the planned e2e tests can't pass.**
    `test_instance` is in demo mode and CI sets `FABLE_DEMO=1`; "link import
    is on" read the real `<data dir>/bin/yt-dlp`; the default download folder
    is the developer's real `~/Movies`. Tests must never touch real user state.
*   **S-B1 (style): live check 4 contradicted the sweep rule** ("older than a
    day" vs "the next import removes it").

## Majors (grouped)

*   **Format choice in the probe** (R-M1, C-M4): the probe lacked `-f` / `-S`,
    so its streams and size didn't match the download.
*   **Pipes, progress messages, exit 0 with no file** (R-M2).
*   **Killing yt-dlp doesn't kill the download** (R-M3): the standalone build
    is a PyInstaller launcher whose child, and ffmpeg, survive a SIGKILL to
    the launcher's pid.
*   **A reused file could be deleted** (R-M4, C-M2).
*   **`out_dir` and host rules underspecified** (R-M5, C-M10, C-M11, S-M3):
    `~`, `..`, symlinks; 0.0.0.0, `::`, IPv4-mapped, ULA, CGNAT, `.local`;
    `is_loopback_url` must stay as it is.
*   **`UrlImport`'s shape** (R-M6).
*   **Setup install not all-or-nothing** (R-M7, C-M3): the executable bit,
    the checksum message, hash parsing, `tar` details, staging.
*   **Log privacy** (R-M8, C-M5): `applog::scrub` keeps links and video ids.
*   **Top-bar UI undefined** (R-M9, C-M13).
*   **`launch.rs` interface, launch triggers, exit-code mapping** (R-M10,
    C-M6, C-M7), **test harness** (R-M13, C-M8), **serve approvals** (R-M11,
    C-M9), **app lock refusal** (R-M12), **release build** (C-M12),
    **Step 3 gated on E2** (R-M14).
*   **Style**: progress formula, override contradiction, unpack wording, the
    launch rule's file, per-step test gates, measurable live checks (S-M1 to
    S-M7).

## Nits

Recorded in the review session; the ones applied are listed in
`goldfish_review_report.md` (Stage 9 section).
