# Stage 8: Intermediate Zoom Out Review

**Target:** `docs/designs/url-import-and-cli.md` (revised after Stages 5–7,
lines 1–946)
**Reviewer:** `egm-zoom-out` (fresh context), standard `zoom-out-standard.md`,
with `zoom_out_report.md` and `goldfish_review_report.md` as context
**Status:** complete. No Blockers; 3 Major, 7 Nit. Most Stage 5–7 fixes
simplified the design (Deno only, `stream_progress`, `create_project`,
yt-dlp naming files, no `project_id`, one `launch.rs`, ADR-0009).

## Findings

### 🟡 I1: `serve` and the lock grew, and Step 2 already pays for them (Z1, contested as E2)

*   **Lines:** 23–28, 52–54, 90–91, 176–184, 227–232, 242–244, 386–431,
    491–500, 547, 818–824, 843–855, 870–908.
*   **Discrepancy:** Step 3 gained an approval queue with ids and deadlines,
    `library.lock.json`, `peek()`, `status` and a launch rule. In Step 2 the
    CLI already links `roughcut-core` with `whisper-native`, though `tools`,
    `call` and `import-url` need only `launch.rs`, reqwest and serde_json.
    Alternative F's Windows argument doesn't apply in a macOS-only release.
*   **Recommendation:** in Step 2, `roughcut-cli` depends only on the shim's
    library; `roughcut-core` comes with Step 3. Confirm E2 with the owner
    before Step 3; re-weigh Alternative F then.

### 🟡 I2: `out_dir` makes the `download_dir` guard pointless

*   **Lines:** 241, 264, 270, 281–282, 354, 488–490, 541, 678, 692–694,
    731–737.
*   **Discrepancy:** "the download folder changeable only in the app" is a
    bound, yet any MCP client can pass `out_dir` to any existing folder. No
    CUJ uses `out_dir`.
*   **Recommendation:** drop `out_dir` from v1 so the guard means something.

### 🟡 I3: Reusing `resolve_binary` contradicts "on only after Setup installs it"

*   **Lines:** 253–256, 286, 344, 649–652, 661–662, 714–716.
*   **Discrepancy:** `resolve_binary` (`video.rs:99–126`) also searches PATH
    and the Homebrew folders, so a Homebrew yt-dlp would turn link import on
    and "Remove" wouldn't turn it off. "Installed" has two sources of truth
    (the binary, `link-import.json`).
*   **Recommendation:** follow `llm_runtime.rs:24–47`:
    `bin_dir().join("yt-dlp")`, `is_file()` means installed, the JSON only
    supplies versions; unpack Deno's zip with the existing `tar` call.

### 🟢 I4: `Fetched.vcodec` / `RCVCODEC` duplicate `Media.codec`

Build the "isn't H.264" warning from `project.media.codec`.

### 🟢 I5: Temporary-folder cleanup is specified three ways (80–82, 318–324, 679–680, 934–935)

The guard kills yt-dlp on exit and deletes the folder on failure; leftovers
after a quit or crash go to the next import's sweep. Make Goal 3 match.

### 🟢 I6: The progress heuristic is fragile (314–316, 643–646)

A retry can reset `downloaded_bytes`. Print `%(info.format_id)s` and track
streams by it, or show the percent per stream ("video 1/2").

### 🟢 I7: The UI's loopback exemption is speculative; the host check overstates (262–263, 291–292, 541, 675–677, 693–696, 734–735)

One rule for every caller. The check reads the link as text; a DNS name that
resolves to a private address, or a redirect, passes. Say so in Risks.

### 🟢 I8: `launch.rs` should own the send-and-retry loop (224–226, 362–365, 804–816, 826–830)

The shim retries only when the request never reached the app
(`mcp-shim/src/main.rs:132–137`), which prevents a double export. Share it.

### 🟢 I9: `roughcut import-url` duplicates `call` (163–166, 354–355, 828–830)

Keep it only if the owner values CUJ 4's spelling.

### 🟢 I10: The default folder is the removed recorder's folder (36, 241, 611, 679)

ADR-0008 says nothing reads `~/Movies/RoughCut` any more; downloads would
land among old recordings. Say so, or choose a separate default.

## Status of the Stage 5 findings

| Id | Status |
| -- | ------ |
| Z1 | Open: contested by the owner (E2); partly fixed. See I1. |
| Z2 | Resolved; the send loop is not shared yet (I8). |
| Z3 | Resolved (ADR-0009, principle 1; the CLI's install command removed). |
| Z4 | Resolved for Node / nvm / volta; the `resolve_binary` reuse creates I3. |
| Z5 | Resolved. |
| Z6 | Resolved. |
| Z7 | Resolved. |
| Z8 | Resolved. |
| Z9 | Resolved; the admin-prompt part declined on recorded grounds. |
| Z10 | Resolved except the recorder folder (I10). |
