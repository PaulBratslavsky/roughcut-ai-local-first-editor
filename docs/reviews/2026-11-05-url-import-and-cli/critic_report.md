# Stage 7: Critic Review

**Target:** `docs/designs/url-import-and-cli.md` (FeatureRange, lines 1–485)
**Reviewers:** `egm-design`, `egm-structural` (standard
`feature-spec-standard.md`), `egm-style` (persona rules), each in a fresh
context, one round (uninterrupted mode).

## egm-style: Revise (0 Blocker, 8 Major, 14 Nit)

### 🟡 S1: "Media folder" means two folders (17, 62, 137)

The Overview, Goal 3 and CUJ 5 say "RoughCut's media folder"; elsewhere it is
the download folder (151, 169–170, 226, 446). `<data dir>/media/` already
exists (`core/src/adapters/video.rs:631`). Use "download folder" everywhere.

### 🟡 S2: Wrong invariant number; "can see" is untestable (46–49)

"(invariant 4)" matches only `.claude/skills/roughcut-elephant/SKILL.md`; in
`docs/06-build-spec.md` zero egress is item 1. "Something the user started
and can see" contradicts CUJ 2–3 (a client starts it; the app may be
hidden). Replace with Goal 7's rule and cite the principle list.

### 🟡 S3: Goal 7 and the ADR rule overclaim (71–73, 326–331)

They omit existing egress (model downloads, Hosted mode). Goal 7 omits
"update". The ADR has no number. Limit the claim to what this feature adds,
make the lists match, name the ADR.

### 🟡 S4: "`mcp.json` always names the one live endpoint" is contradicted (175–178)

A crash or SIGKILL leaves it stale (305); 287 waits for a "fresh" file;
older builds don't take the lock (453–455). Qualify it and define "fresh".

### 🟡 S5: "A clear error" for live streams is unspecified (220)

A `--match-filter` rejection likely prints a skip notice and exits 0, leaving
no `ERROR:` line. Specify the message.

### 🟡 S6: The final file is always `.mp4` though non-MP4 output is possible (170, 225–227 vs 211, 216–219)

Write `.<ext>`.

### 🟡 S7: Goal 5 has no platform limit (67–68 vs 284–289, 480)

Limit it to macOS or define the failure (exit 3 with a message).

### 🟡 S8: "Accepted with decision A" points nowhere (436–438)

Point to the "Where downloads go" row or Alternative E.

### 🟢 Nits

1.  31–37: the lead-in doesn't fit bullet 2; "come back to a result" is vague.
2.  "The owner" is undefined; Alternative D's rejection reason is "the owner
    chose" instead of the technical reason.
3.  The link field has three names (19, 103, 649).
4.  The MCP tool set has three names; "63" differs from `core/README.md:71`
    (65).
5.  152: "links" means both crate linking and a symlink.
6.  153: "the first process to start" should be "the first to take the lock".
7.  184–192: "named `name`, or the title" wording; "the same function"
    unnamed; whether the app transcribes MCP-started imports.
8.  Subjectless sentences (194–197, 233–238, 279–281, 451).
9.  216–217: "every NLE reads" is absolute; "NLE" unexpanded.
10. 86–91: second person; "HyperFrames" undefined; "discussed separately"
    where?
11. Vague CUJ titles and wording (125, 130, 133, 138); "what to do" vs "why".
12. Unmeasured judgments in Alternatives A, G, I.
13. "This repo builds with 1.93" is unpinned (no rust-toolchain); memory
    claim has no number.
14. Double colons (443, 481–482); "a fuller hand-off" is vague.

## egm-structural: Revise (0 Blocker, 6 Major, 11 Nit)

Conforms: every template H2 present and in order; no skipped heading
levels; fences closed; Decisions table well-formed; CUJs use Given/When/Then;
all nine Alternatives have Pros, Cons and Reason; CUJ tool names exist in
`core/src/tools.rs`.

### 🟡 T1: Broken cross-reference "decision A" (437–438)

Name the target, or letter the Decisions rows to match Alternatives A–I.

### 🟡 T2: "invariant 4" points to the wrong rule (47–48)

`docs/07-architecture.md` (line 66) and `README.md` (line 252) number zero
egress as principle 1; principle 4 is "One tool registry". Cite and link.

### 🟡 T3: The diagram contradicts the text (159–173)

(a) The launch fallback hangs off the junction shared with `mcp-shim`, so it
reads as applying to the shim. (b) Only `dispatch(Caller::Ui)` arrows into
`Editor`; `dispatch(Caller::Mcp)` leads nowhere. The "(inside the app, or
inside serve)" note sits under "HTTP + Bearer", not "MCP server".

### 🟡 T4: "media folder" names a different, existing folder (17, 62, 137)

Also: `<tmp>` (211, 225) is never tied to `<download dir>/.roughcut-<uuid>/`;
the file name is `<title> [<video id>].mp4` at 170 but
`<sanitised title> [<id>].mp4` at 226. One term, one placeholder.

### 🟡 T5: Risks are scattered; the main risk is missing from NFR (46–49, 86–88, 324–331, 369–372, 429–455)

No Privacy/egress entry in NFR; download rights only in Non-Goals; yt-dlp
breakage only in Alternative D's Cons; the old-app-plus-new-serve gap has no
mitigation or explicit acceptance. Add a Risks & Mitigations list.

### 🟡 T6: No CUJ for installing yt-dlp or the CLI (98–140 vs 150, 152, 240–251, 269)

Add a Setup install/update CUJ (including a checksum mismatch) and a CUJ for
linking the CLI.

### 🟢 Nits

1.  CUJ 5 bundles several journeys; the JS-runtime warning is a success
    outcome. Split it.
2.  Two headings named "Overview" (H2 line 10, H3 line 157); the Decisions
    table comes before the diagram.
3.  Additional Resources / Future Improvements are H3 under the NFR H2 (from
    the template).
4.  Missing horizontal rule between Alternatives and NFR (template line 102).
5.  Additional Resources: plain-text ADR names instead of relative links (cf.
    `hosted-models.md:613–617`); bare video id; ADR-0005,
    `docs/zero-egress-verification.md` and `docs/07-architecture.md` cited but
    not listed; ADR-0008's gloss should say it is item 3 of "Remove in-app
    recording".
6.  The Status line mixes a status with commentary.
7.  Bold run-in labels are styled three ways (182–260, 294).
8.  CLI synopsis rows 267/269 lack descriptions; `install yt-dlp | update
    yt-dlp` is ambiguous.
9.  The link field has two names (19, 103).
10. Alternatives E–I bundle two options; D's rejection reason cites the
    owner's choice, not the technical reason.
11. The Technical Plan carries implementation detail (file paths, a
    signature, Rust versions, field-level model changes) that duplicates the
    DIP.

## egm-design: Not ready (1 Blocker, 10 Major, 11 Nit)

### 🔴 D1: Changes the egress and registry trust rules without superseding ADR-0004 / ADR-0007, and puts no limits on downloads (46–49, 71–73, 149, 151, 182, 324–331, 345–354, 434–438)

ADR-0004 rejects a registry download tool ("would let a confused agent loop —
or any MCP client — pull gigabytes from an arbitrary URL"); ADR-0007 made
principle 1 "only a human action can *enable* egress". `import_url` is open to
any MCP client with any URL, no approval, no enabling step, and `out_dir`
anywhere writable (like `export_clips`, which needs approval). Only http(s)
is checked, so loopback and LAN hosts pass. `zero-egress-verification.md`
uses `lsof -p <app pid>`, which can't see a yt-dlp child. Recommendations:
amend both ADRs explicitly; make enabling a human action clients can't reach
(installing yt-dlp in Setup, or a consent field `set_preferences` can't
change); bound each call; limit `out_dir` for MCP or require approval; make
`download_dir` UI-only; refuse loopback/private hosts for non-UI callers;
change the verification to cover child processes.

### 🟡 D2: Nothing limits a call to one video (79–80, 199–212, 225–228)

`--no-playlist` only applies to a video+playlist URL; a pure playlist or
channel URL downloads every entry into the same `video.%(ext)s`. Run a
metadata-only pass first and refuse playlists; it also gives `live_status`,
`duration`, `filesize_approx` and the title. Add `-I 1` as a backstop.

### 🟡 D3: The format fallback breaks the playable-copy claim and Goal 2 (58–60, 170, 205–206, 216–219, 226)

`playable_fix` (`video.rs:533–553`) never re-encodes video inside an MP4, so a
VP9/AV1 merge gets no preview copy; the fallback has no height cap;
`b[ext=mp4]` is usually 360p; a single-file `b` pick can be webm yet is
renamed `.mp4`. Use format sorting (`-S res:1080,vcodec:h264,acodec:aac
--remux-video mp4`), keep the real extension, and handle or warn about
non-H.264 video.

### 🟡 D4: "No partial file left" fails on cancel, crash or exit (61–63, 135–139, 169, 199–200, 229–231, 305–306)

Code after an `.await` never runs when the future is dropped; `kill_on_drop`
doesn't fire on process exit; hidden `.roughcut-<uuid>/` folders can hold GBs;
a refused attach leaves the moved file. One guard for child + temp folder;
`serve` kills children before exit; sweep stale `.roughcut-*` at the start of
each `import_url`.

### 🟡 D5: Over MCP, the download lives only as long as the request (53–57, 108–114, 155, 199–200, 417–427)

Clients time requests out (TypeScript SDK default 60 s, reset only by
progress). Dropping the handler kills yt-dlp, so a long download can never
succeed. Precedent: a `find_clips` scan persists if the client stops
waiting. Let the download finish regardless of the request, or return a job
id, or send progress; revise Alternative I.

### 🟡 D6: Problem 1 stays unsolved for MCP clients (33–35, 67–68, 152, 385–395)

Only the CLI auto-launches; the shim is unchanged. Add a mechanism (for
example `roughcut mcp`, or launch in the shim) or move it to Non-Goals.

### 🟡 D7: Lock-holder detection is unspecified, and reading the JSON is wrong (153, 175–178, 270, 284–286, 296, 310–321)

Stale JSON after a crash; Windows `LockFileEx` blocks reading the locked
file; truncating before locking wipes the holder's JSON; no app-held-by-app
message. Use a sibling `library.lock.json`, decide with `try_lock`, truncate
after locking.

### 🟡 D8: `serve` is not a full peer of the app (125–131, 175, 294–306, 448–450)

Hosted key: Keychain access is granted to the app's signature, so `roughcut`
gets a dialog (none over SSH). The managed llama-server starts only from a
Setup click. `applog` is set up only by the shell. List what `serve` can't do
or fix each gap.

### 🟡 D9: Terminal approvals can apply an answer to the wrong request (154, 300–304, 407–415)

`request_confirmation` denies after 120 s; a late `y` can approve the next
request. Print an id and deadline, discard input on timeout, require the id
when several are waiting.

### 🟡 D10: Alternative D's reasons apply to the chosen design (139–140, 150, 233–238, 247–251, 365–374)

The app copy is also only as new as the last Update click; `brew install
deno` reintroduces Homebrew; the EJS wiki says YouTube *needs* a runtime.
Install Deno like yt-dlp; offer an update after a failed import; warn when
the version (a date) is old.

### 🟡 D11: Risks & Limits don't meet the standard (429–455)

No Risks & Mitigations section. Missing: breakage detection (no
`url_import` log events), legal/distribution (youtube-dl's 2020 DMCA
takedown; downloading at runtime is the mitigation), what `source_url`
exposes.

### 🟢 Nits

1.  CUJ 1: the project exists only after success, so where does progress
    show? The metadata pass gives the title up front.
2.  `--match-filter "!is_live"` skips with exit 0 (no `ERROR:`); the spelling
    is `--match-filters`; check `live_status` (also catches upcoming streams).
3.  `_percent_str` is a display field; a merged download runs 0→100% twice;
    use `downloaded_bytes` / `total_bytes` / `total_bytes_estimate`.
4.  Check `project_id` rules before downloading; demo mode and missing
    ffmpeg are unspecified.
5.  `source_url` is lost on relink and `append_media`, and when an older
    build re-saves.
6.  `/usr/local/bin` is root-owned and may not exist; a link into the bundle
    breaks if the app moves; `open -g` doesn't hide (`-j` does); resolve
    `current_exe` through the symlink.
7.  yt-dlp publishes `SHA2-256SUMS.sig`; "`-U` checks checksums" is
    unsupported by the supplied docs; llama-server uses `sha256: None`;
    `releases/latest/download/<asset>` avoids the API rate limit; ARM64 and
    musl assets are left out.
8.  Facts: 66 rows, 65 MCP-visible; "invariant 4"; "decision A";
    `llm_status.rs` opens the real library; "always names the one live
    endpoint".
9.  `getSetupStatus()` runs on mount (`EmptyState.tsx:148`); `--version`
    probes there run a self-extracting binary each time: cache them.
10. Unattended export can't work (hidden app, `serve` without a terminal,
    `ROUGHCUT_NO_CONFIRM` can't reach an app opened with `open`): state it.
11. Rollback: name the loss of `source_url` and `download_dir` when an older
    build saves.
