# App log

*   **Author:** Paul Bratslavsky
*   **Status:** Implemented on `feat/logging` (see "As built"); the manual
    check in the desktop app is still to do. Reviewed (scope + accuracy
    goldfish reviews, see "Review"); the owner chose the recommendation for
    each open question.
*   **Date:** 2026-11-03 (built 2026-11-04)

--------------------------------------------------------------------------------

## Overview

Give RoughCut a small log on the user's Mac, so a failure or a fallback can
be diagnosed after the progress message is gone. One line per event: each
AI step (which model answered, how long it took, whether it fell back to
rules and why), each change in where Hosted requests run, ffmpeg and ffprobe
failures with the end of their error output, transcription start, end and
failure, and app start.

The log never leaves the machine, never holds transcript text, prompts,
model replies, chat messages, API keys, tokens or audio, and stays under
15 MB (three files of 5 MB, oldest dropped).

## The Problem

*   The app writes no log. Shipping code has two `eprintln!`s
    (`core/src/mcp/server.rs:75`, `app/src-tauri/src/lib.rs:270`); a release
    `.app` has no terminal, so even those go nowhere.
*   A recent investigation couldn't tell whether a Hosted (Claude) story
    outline had failed and fallen back to pause-based topics. The fallback in
    `story::outline_using` (`core/src/story.rs:117-133`) turns a non-fatal
    error, a model that returned fewer than two beats, and a model server
    that is down into the same `method: "heuristic"`, with no record of which.
*   The same silence is everywhere a step degrades:
    *   shorts scoring keeps its rule-based scores when the model fails
        (`clip_finder.rs:186-196`), and per batch (`:621-640`);
    *   flow review returns `deterministic` when the model is down, and
        reports `llm` with no model findings when the reply is unusable
        (`story.rs:484-537`);
    *   chat switches to the offline editor when the first model call fails
        (`agent.rs:296-302`);
    *   chapters, title/description and the background transcript clean-up
        check the model and fall back to rules without making a request when
        it is down (`agent.rs:749-751`, `:820-827`, `:872-889`); clean-up
        also skips failed chunks (`agent.rs:796`), and its result is dropped
        (`engine/media.rs:104-116`).
*   The router knows when Hosted falls back to Local and why
    (`engine/route.rs:501-509`), but keeps only the latest reason for the
    top-bar chip.
*   ffmpeg errors from `FfmpegCli::run` and `run_binary` keep the first 800
    characters of stderr (`adapters/video.rs:145-148`, `:161-164`). The
    playable copy and the join run with `-v error`, so that is the cause; the
    audio extract, frame grab and peaks decode run at ffmpeg's default
    verbosity, so it is the version banner and the input dump. The three
    ffprobe calls run with `-v quiet` (`video.rs:180`, `:486`, `:590`) and
    write no error text at all.

## Goals

*   **Diagnose after the fact.** From the log alone, answer: did this step
    run on Claude, on the local model, or on rules? If it fell back, why?
    How long did it take, and how many tokens did Hosted spend? Why did this
    import, render or transcription fail?
*   **Nothing private in it.** No transcript text, prompts, model replies,
    chat messages, themes, instructions, API keys, the MCP token or audio.
    The rule is enforced by the code's shape (typed events) and by one
    scrubber for error text, and a test proves it. **File paths are logged**
    (the home folder shown as `~`), and a media file's name is often the
    project's name (`frontend/src/newProject.ts:20`); see D2.
*   **Local only.** The log is a file on this Mac. Nothing sends it anywhere;
    the app makes no network call for it.
*   **Bounded.** Size-based rotation: at most three files of 5 MB.
*   **Small.** No new crate, no new registry tool, no change to the tool
    contract or the generated frontend types.

## Non-Goals

*   **Uploading logs, crash reporting, telemetry.** Zero egress stays the
    rule; if the user wants to share the log, they copy the file.
*   **A log viewer in the app.** The file is plain text; Console.app, a text
    editor or `jq` read it.
*   **Logging every tool call.** MCP and UI tool calls aren't logged (see
    Future Improvements).
*   **Frontend (webview) errors**, the semantic search index, and the
    Hosted key check in Setup (Setup already shows its result).
*   **A registry tool to read the log.** An MCP client reading the log would
    send it to that client's model provider; Claude Code can already read
    the file with the user's permission.
*   **Debug-level tracing** of everything the core does.

## Open Questions

Decided by the owner on 2026-11-04: the recommendation in each case
(`~/Library/Logs/RoughCut/`, JSON lines, "Reveal logs" only).

*   **Q1, location.** Recommended: **`~/Library/Logs/RoughCut/`** on macOS
    (the macOS convention; Console.app lists it under Log Reports), and
    `<data dir>/logs/` (`store::data_dir()`) on Windows and Linux.
    Alternative: `<data dir>/logs/` everywhere
    (`~/Library/Application Support/roughcut/logs/` on macOS), next to the
    projects database.
*   **Q2, format.** Recommended: **JSON lines** — one JSON object per line,
    greppable, `jq`-friendly, and the no-secrets test parses it.
    Alternative: plain text (`2026-11-03T10:12:01Z WARN ai_step outline
    fallback …`), easier to read by eye, harder to filter.
*   **Q3, a button in Setup.** Recommended: **"Reveal logs"** — one row in
    Setup that opens Finder at the log file (one small Tauri command that
    returns the path; the existing `reveal_path` does the rest).
    Alternatives: also **"Copy diagnostics"** (app version, mode, toolchain
    status and the last ~200 log lines to the clipboard; one more command and
    a bit more UI); or no button.

## Decisions

Made by the designer; the owner can overrule them.

*   **D1, settled by the brief:** 5 MB × 3 files; no transcript text or
    secrets; no egress.
*   **D2, file paths stay in the log**, with the home folder as `~`. A
    failure's file is the first thing to know ("which import failed"), and
    the log is a file on the user's own Mac. The cost: a media file's name,
    often the project's name, can appear in an error or in ffmpeg's stderr.
    Redacting it reliably in free text (file names with spaces) would mean
    passing the known paths to every error site; if the owner wants that, it
    is a contained follow-up in the scrubber.
*   **D3, ffprobe runs with `-v error` instead of `-v quiet`**, so a failed
    probe has a reason to log. stdout (the JSON) is unchanged; on success
    `-v error` writes nothing to stderr.

## Critical User Journeys (CUJs)

### CUJ 1: "Did Hosted fail on that outline?"

1.  The owner runs Find shorts with Hosted on. The topics come back as
    pause-based sections.
2.  They open Setup → Reveal logs and search for `outline`.
3.  The log shows an `ai_request` for `outline` that failed (code
    `hosted_refused`, or `bad_json` with where the JSON broke), a
    `model_route` line if Hosted went offline and the run moved to Local,
    and an `ai_step` line: `outline`, mode `hosted`, outcome `fallback`,
    reason `inference`, and how long it took.

### CUJ 2: A transcription that fails on the audio

1.  An import's transcription fails at "extracting audio".
2.  The log has an `ffmpeg_failed` line for `ffmpeg audio extract` with its
    exit code and the last lines of ffmpeg's stderr, which name the cause
    (the head of stderr, which the on-screen error shows today, is ffmpeg's
    banner), followed by `transcribe_failed` with stage `audio`.

### CUJ 3: Transcription that never finished

1.  The app was quit during a transcription, or it crashed.
2.  The log shows `transcribe_start` with the media length, language,
    engine and whisper model. With no `transcribe_end` or
    `transcribe_failed` after it, an `app_exit` means it was quit; no
    `app_exit` before the next `app_start` means a crash.

### CUJ 4: Sharing a log

1.  The owner wants help with a bug and sends the log file to someone.
2.  It holds model names, times, token counts, error codes and file paths
    (home as `~`; file names can name the project, D2), and no words from
    any transcript, prompt, reply or chat.

## Technical Plan (Architecture)

### Overview

```
  story / clip_finder / agent ──► applog::Step ─┐
  llm::ask_json_schema_served ──► ai_request ───┤
  agent chat loop ──────────────► ai_request ───┤
  Editor::record_route ─────────► model_route ──┼──► applog::write(level, Event)
  adapters/video (ffmpeg) ──────► ffmpeg_failed ┤      │ scrub error text
  engine/media (transcribe) ────► transcribe_* ─┤      │ serialize one line
  app shell (setup / exit) ─────► app_start ────┘      ▼
                                          ~/Library/Logs/RoughCut/roughcut.log
                                          (+ roughcut.1.log, roughcut.2.log)
```

One new core module, `core/src/applog.rs` (glossary term **App log**). It is
process-wide (a `OnceLock`, like `playable_failures()` in
`adapters/video.rs:366`) because the ffmpeg adapter has no `Editor` to hang
it on. Until the shell calls `applog::init(dir)`, events go nowhere, so
tests, examples and the CLI write nothing.

### No log crate

`log` 0.4 and `tracing` 0.1 are in `Cargo.lock` only as dependencies of
Tauri, reqwest and friends; no RoughCut `Cargo.toml` names them, and
`tracing-subscriber` / `tracing-appender` aren't there at all. They aren't a
good fit:

*   Both take free text (`info!("{}", x)`), so anyone can log a transcript
    line by accident. A typed event enum makes that impossible to write: the
    only free text is error text, and it goes through the scrubber.
*   `tracing-appender` rotates by time (hourly, daily), not by size; size
    rotation would still be hand-written.
*   A global `log`/`tracing` subscriber would also collect the debug output
    of reqwest, hyper and Tauri, which then needs filtering.

The hand-written module is about 300 lines plus tests, using crates the core
already has (`serde_json`, `chrono`, `dirs`; `tempfile` for tests).

### The file

*   `roughcut.log` in the log folder (Q1), opened for append. Each event is
    serialized to one line and written with a single `write_all` under the
    logger's own mutex (a poisoned lock is recovered, not unwrapped),
    unbuffered, so the last lines survive a crash.
*   **Rotation:** before a write that would take the file past 5 MB,
    `roughcut.1.log` → `roughcut.2.log` (replacing it), `roughcut.log` →
    `roughcut.1.log`, then a new `roughcut.log`. At most three files, 15 MB.
    The size is read once at open and counted in memory after that.
*   If the folder can't be created or the file can't be written, logging
    turns itself off for the session; it never fails or slows a user action.

### Line format (Q2: JSON lines)

```json
{"ts":"2026-11-03T10:12:01.123Z","level":"warn","event":"ai_step","step":"outline","project":"5f0c…","mode":"hosted","outcome":"fallback","reason":"inference","error":"bad JSON in outline response: invalid type: string \"…\", expected struct RawBeat","ms":41250}
```

`ts` (UTC, milliseconds), `level` (`info` | `warn` | `error`), `event`, then
the event's fields. Fields that don't apply are left out.

### Events

| `event` | Level | Fields | Written by |
| ------- | ----- | ------ | ---------- |
| `app_start` | info | `version`, `os`, `arch`, `mode` (local / hosted), `hosted_model`, `local_model`, `demo` | shell setup, after `Editor::bootstrap` |
| `app_start_failed` | error | `code`, `error` | shell setup, when `Editor::bootstrap` fails (a locked or broken database) |
| `app_exit` | info | — | `RunEvent::Exit` |
| `mcp_failed` | error | `error` | MCP server start failure (shell) and the server stopping (`mcp/server.rs:75`); replaces both `eprintln!`s |
| `ai_request` | info / warn | `step`, `served_by`, `hosted`, `ms`, `attempts`, `input_tokens`, `output_tokens`, `outcome` (ok / bad_json / error), `code`, `error` | `llm::ask_json_schema_served` (every JSON-contract call); the chat loop, one per model turn |
| `model_route` | warn / info | `effective` (hosted / local), `reason` (offline / rate_limited / server_error / key_rejected / no_key), `detail` | `Editor::record_route` (`route.rs:369`), when what the chip shows changes |
| `ai_step` | info / warn / error | `step`, `project`, `mode`, `outcome` (ok / fallback / failed / aborted), `method`, `reason`, `code`, `error`, `ms`, plus step counts | the step sites below |
| `ffmpeg_failed` | warn | `what` (e.g. `ffmpeg render`, `ffprobe container`), `exit_code`, `ms`, `stderr_tail` | `FfmpegCli::run`, `run_binary`, `render_mp4` (exit ≠ 0, or the binary couldn't start) |
| `transcribe_start` | info | `project`, `media_s`, `language`, `engine`, `whisper_model` | `Editor::transcribe` |
| `transcribe_end` | info | `project`, `ms`, `segments`, `model` | `Editor::transcribe` |
| `transcribe_failed` | error | `project`, `stage` (audio / whisper / save), `ms`, `code`, `error` | `Editor::transcribe` |

Notes:

*   **`ai_request.step`** is the call's existing label (`outline`,
    `short_scores`, `flow review`, `story plan`, `transcript cleanup`,
    `chapters`, `title/description`, `chat`). Token counts are the `Usage`
    the Anthropic client already returns, summed over a call's repair retry;
    local replies have none. `served_by` is the `ServedBy` model that
    answered. A reply that isn't valid JSON is logged as `bad_json` with
    serde's category, line and column only, never the parse message (which
    quotes the reply).
*   **`model_route`** is written where the router already decides whether
    the chip changed, so it covers every branch (offline, rate limited,
    server error, no key, a key-store read failure, a rejected key, and the
    return to Hosted) once per change, not once per request. A run stopped
    so it doesn't switch models shows up as that run's failed `ai_request`
    and `ai_step`.
*   **`ffmpeg_failed`** is a warning, not an error: some callers expect
    failures and carry on (`load_raw_peaks` at `video.rs:784`). The step that
    gave up (`transcribe_failed`, an export error) is the error.
*   **`transcribe_start.engine`** is `whisper-rs`, `whisper-cli` or `mock`
    (from `whisper_native_enabled()`, the demo flag and the CLI probe the
    `AutoTranscriber` uses); `whisper_model` is the model file's name from
    `resolve_whisper_model()`.
*   **`mode`** on a step line is the user's setting when the step began;
    whether a request ran on Claude is `ai_request.hosted` and `model_route`.

### AI steps

`applog::Step::start(step, project_id, mode)` notes the start time;
`ok(..)`, `fallback(reason, err)` and `failed(err)` write the `ai_step` line
with `ms`. A step dropped without one of them (an early `?` exit, or a
cancelled request) writes `aborted`. Functions with many exits are wrapped:
the body becomes an inner function and the outer one logs its `Result`
(`Err` → `failed`; `Ok` → `ok`, unless the body marked a fallback or a
partial result on the step first).

| `step` | Where | Outcomes |
| ------ | ----- | -------- |
| `outline` | `story::outline_using` | cache hit → `ok`, method `cache`, the model that made it; model down → `fallback` `model_unreachable`; fewer than two beats → `fallback` `too_few_beats`, `beats`; a non-fatal error → `fallback` with its code; a fatal Hosted error → `failed`; else `ok`, method `llm`, `beats`, `models` |
| `short_picks` | `clip_finder`, the `llm_rescore` decision | model down → `fallback`; an error (e.g. "model scored no clip candidates") → `fallback`, or `failed` if fatal; else `ok`; always `batches`, `batches_failed` (a failed batch keeps its rule scores) |
| `flow_review` | `story::review_flow_using`, when the model pass is asked for | model down → `fallback`; unusable reply → `fallback`; fatal → `failed`; else `ok`, `boundaries` |
| `story_plan` | `story::story_edit` (wrapped) | model down with a target length → `fallback` to the duration planner; model down without one → `failed`; the plan request failing, or either guard ("cut nearly every beat", "over 80% of the video") → `failed`; a failure after edits landed (`or_partial!`) → `failed` with `partial: true` and `actions`; else `ok`, `cut_beats`, `actions` |
| `chat` | `agent::run_instruction` (wrapped) | model down → `fallback` to the offline editor; a first-turn non-Hosted error → `fallback` with its code; a Hosted error, or a later error → `failed` (with `actions` that had landed); out of steps → `ok` with `stopped: "max_steps"`; else `ok`; always `turns`, `actions` |
| `transcript_cleanup` | `agent::clean_transcript` | model down → `fallback` (text left as transcribed); fatal → `failed`; else `ok`; `chunks`, `chunks_failed`, `changed` |
| `chapters` | `agent::generate_chapters` | model down or an error → `fallback` to silence-based chapters; fatal → `failed`; else `ok` |
| `title_description` | `agent::generate_title_description` | the same pattern |

### The no-secrets rule

Three layers:

1.  **Typed events.** `Event` is an enum whose fields are numbers, ids,
    enums, model names, step labels and error text. There is no field for
    transcript text, prompts, replies, chat messages, themes, instructions
    or project names, and no call site passes one. Project ids (UUIDs) are
    logged.
2.  **One scrubber for error text** (`applog::scrub`), applied inside
    `write` to every `error`, `detail` and `stderr_tail`, so a call site
    can't skip it. Error strings can carry model output today:
    `unexpected message shape: {e} in {msg}` (`adapters/inference.rs:155`)
    embeds the reply, `inference HTTP {status}: {body}` the server's body,
    and serde errors quote a fragment of the reply in Rust's escaped form
    (`invalid type: string "a \"quoted\" word", expected …`). The scrubber
    is one left-to-right scanner that understands strings and backslash
    escapes (the `in_string` / `escaped` logic of `llm::extract_json`):
    *   the inside of `"…"`, `“…”` and `` `…` `` spans becomes `…`
        (backtick spans that are plain identifiers, like serde's field
        names, are kept); an unclosed quote is cut to the end;
    *   then `{…}` spans (JSON bodies and replies), balanced and
        string-aware, become `{…}`;
    *   `sk-ant-…` keys become `sk-ant-…`, and any run of 40 or more
        letters, digits or `_` (a token, the MCP token) becomes `…`; `-`,
        `.` and `/` end a run, so UUIDs and file names survive;
    *   the home folder becomes `~`;
    *   `error` and `detail` are capped at 300 characters. `stderr_tail`
        keeps the last 1,000 and skips the quote and brace rules, which
        would blank ffmpeg's own messages; ffmpeg's stderr holds file paths
        and stream details, never words from the video.
3.  **A test** runs an outline, a short-picks scan and a chat turn against
    fake local and Hosted models (the `route.rs` test doubles, extended to
    return `Usage`), with a realistic `sk-ant-api03-…` key stored, a
    transcript and replies carrying unique marker words, and a reply that
    fails to parse with the marker inside escaped quotes. It fails if a
    marker, the key or a prompt fragment appears in the log, and checks that
    the expected `ai_request`, `ai_step` and `model_route` lines are there.
    The capture is per test (a thread-local buffer; `#[tokio::test]` runs
    on one thread), so tests running in parallel don't see each other's
    lines.

### Setup button (Q3)

A new Tauri command `log_path` (outside the registry, like `reveal_path`;
ADR-0004) returns the log file's path, not part of `SetupStatus`, so the
generated types don't change. `SetupPanel.tsx` gets one row, "Logs", with
the path and a "Reveal" button that calls the existing `revealPath`; the row
is hidden in the browser build. No registry tool, so `docs/tool-api.md` and
the mock parity tests don't change.

### Docs

*   `CONTEXT.md`: the **App log** term.
*   `docs/07-architecture.md`: a "Logs" subsection under §11 (Persistence
    and events) and the shell's new command in §12.
*   `docs/zero-egress-verification.md`: one line — the log is a local file,
    nothing reads it over the network.
*   This file: "As built" once implemented; `SITEMAP.md` status.

## Alternatives Considered

### Alternative A: `tracing` + `tracing-subscriber` + `tracing-appender`

The standard Rust stack: spans give step timing for free and the JSON
formatter exists. Rejected: free-text macros make the no-secrets rule a
review habit instead of a property of the code; the appender rotates by time,
not size; three new crates, and Tauri's and reqwest's own events need
filtering out. Worth revisiting if the app grows a real tracing need (spans
across the MCP server and the agent loop).

### Alternative B: `log` crate facade with a custom logger

Smaller than `tracing`, same free-text problem, still hand-written rotation.

### Alternative C: Store events in the SQLite database

The store is already there and queryable. Rejected: a log must survive a
broken or locked database, the case it most needs to explain, and a text
file is what a user can open, attach or grep.

### Alternative D: Log only at the request level

`ai_request` alone (one site in `llm.rs`, one in `agent.rs`) is the smallest
change. Rejected: it can't say that a step fell back to rules because the
model server was down (no request is made) or because the model returned
too few beats (the request succeeded), which is the question that started
this. The other way round, step lines alone would need token usage passed
up through outline chunks, scoring batches and the chat loop, since only
`ask_json_schema_served` sees `response.usage` today.

### Alternative E: Redact file names too

See D2. Possible later, by passing the paths an error site knows to the
scrubber.

## Non-Functional Requirements

*   **Privacy:** see "The no-secrets rule" and D2. The log folder is in the
    user's home, like the database.
*   **Egress:** none. The module does file I/O only.
*   **Performance:** a few lines per AI step, one per ffmpeg failure; each is
    a ~300-byte write. No effect on edit latency.
*   **Robustness:** a write failure disables the log for the session and is
    never returned to a caller; the logger takes no other lock than its own,
    and recovers it if poisoned.
*   **Disk:** at most 15 MB.

### Additional Resources

*   `docs/designs/hosted-models.md` and ADR-0007 — the router and the errors
    this log records.
*   `docs/zero-egress-verification.md` — the egress rule.

### Future Improvements

*   "Copy diagnostics" in Setup (if not chosen in Q3).
*   A panic hook that logs the panic location (message scrubbed).
*   Tool calls: `tool_call` lines (tool name, caller, ms, error code) for
    MCP and agent runs.
*   Frontend errors through a `log_frontend_error` command.
*   The semantic index (`engine/semantic.rs:77` returns `Ok(0)` when the
    embedder is down; `engine/media.rs:119` drops the result).
*   `FfmpegCli::run` and `run_binary` could keep the tail of stderr in the
    user-facing error too, as `render_mp4` does. Separate change.
*   Flow review reports `method: "llm"` when the model's reply was unusable;
    it could say `deterministic`. Separate change.

## Review

Two goldfish reviews (scope; technical accuracy), 2026-11-03. Changes made:

*   **Scrubber:** escape-aware string scanning (serde quotes reply text with
    `\"` inside), quotes before braces; `bad_json` logs serde's category and
    position only; the token rule no longer eats UUIDs and file names;
    dropped the Bearer / `x-api-key` / `user:pass@` rules, which no error
    string carries.
*   **Events cut:** `hosted_error`, `mode_change`, `hosted_key` (covered by
    `ai_request`, `ai_step` and `mode` on each line, or out of the brief).
    `model_route` moved to `Editor::record_route`, which sees every branch
    and the return to Hosted.
*   **Added:** step lines for chapters, title/description and transcript
    clean-up (they fall back without making a request); `batches_failed` /
    `chunks_failed` so a partial fallback doesn't read as a model run;
    `aborted` for steps that exit early; the 80% guard, `or_partial!` and
    the out-of-steps chat outcome; `app_start_failed`; the MCP server's
    "stopped" message.
*   **Facts fixed:** two `eprintln!`s, not three; the global's precedent is
    `playable_failures()`, not the event sink (which is per `Editor`); the
    playable copy and join already show ffmpeg's cause; ffprobe's `-v quiet`
    (D3); file paths name projects (D2).
*   **Tests:** per-test thread-local capture; reuse the `route.rs` fakes with
    `Usage` and a realistic key.

## Detailed Implementation Plan

#### 1. `core/src/applog.rs` (new), `core/src/lib.rs`

`init(dir)`, `log_dir()` (Q1), `Event` (serde, tagged by `event`),
`write(level, Event)`, `Step`, `scrub`, the rotating file, and a test-only
thread-local capture. Unit tests: rotation, scrubber.

#### 2. Request level (`core/src/llm.rs`, `core/src/agent.rs`)

`ask_json_schema_served`: time the call, sum `Usage`, count attempts, write
`ai_request`. The chat loop: one `ai_request` per model turn.

#### 3. Router (`core/src/engine/route.rs`)

`model_route` in `record_route` when `changed`.

#### 4. Steps (`core/src/story.rs`, `core/src/clip_finder.rs`, `core/src/agent.rs`)

`Step` at the eight step sites; `llm_rescore` returns its batch counts;
`clean_transcript` its chunk counts.

#### 5. ffmpeg (`core/src/adapters/video.rs`)

`ffmpeg_failed` in `FfmpegCli::run`, `run_binary` and `render_mp4`; one
stderr-tail helper shared with `render_mp4`'s existing tail; ffprobe
`-v error` (D3).

#### 6. Transcription (`core/src/engine/media.rs`)

`transcribe_start` / `_end` / `_failed` in `Editor::transcribe`, with the
stage.

#### 7. Shell (`app/src-tauri/src/lib.rs`), `core/src/mcp/server.rs`

`applog::init(applog::log_dir())` before `Editor::bootstrap`;
`app_start` / `app_start_failed`; `mcp_failed`; `app_exit`; the `log_path`
command (Q3).

#### 8. Frontend (`frontend/src/ipc/api.ts`, `frontend/src/panels/SetupPanel.tsx`)

`logPath()`; the Logs row in Setup (Q3).

#### 9. Docs

As listed under "Docs".

## As built

Built as designed, with these differences:

*   **Counts are kept where they happen.** `clean_transcript` writes its
    own `transcript_cleanup` line (chunks, chunks_failed, changed) instead of
    returning counts; `llm_rescore` fills a `Tally` (batches, failed batches,
    the models that answered) that the `short_picks` line reads.
*   **`ai_step.models`** lists the models that answered, in order, so a run
    that moved from Claude to the local model part-way shows both.
*   **`chapters`** falls back with reason `no_chapters` when the model
    returns an empty list.
*   **`bad_json`** reads `bad JSON: syntax error | wrong shape | cut short
    at line L column C`, or `bad JSON: no JSON in the reply`.
*   **ffmpeg:** `FfmpegCli::run` and `run_binary` share one `output()` that
    logs; `render_mp4` uses the same `stderr_tail` helper for its
    user-facing error. User-facing error text is unchanged.
*   **The token rule** (40+ letters, digits or `_`) also blanks a file name
    made of one 40+ character run with no space, `-` or `.`. That is the
    safe side, so it stays.
*   **Setup → Logs** shows the file's path and a "Reveal logs" button. The
    row is hidden when the log isn't open (the browser build, or a folder
    the app couldn't create).

Tests: `applog::tests` covers rotation (cap, three files, oldest dropped,
reopen), the scrubber (escaped quotes, braces in strings, keys, long tokens,
UUIDs and paths kept, home, caps), the stderr tail, line shape, the `Step`
outcomes, and the no-secrets run (outline, shorts scan, chat on the
`route.rs` fakes, which now return `Usage`).

### Verification Plan

*   `cargo test -p roughcut-core`: rotation (cap, three files, oldest dropped,
    rotation on reopen), scrubber cases (escaped quotes, braces in strings,
    keys, long tokens, UUIDs and paths kept, home), the no-secrets test, and
    the existing suites.
*   `cargo check -p roughcut-app`.
*   `cd frontend && npx tsc -b && npm run build`.
*   Manual, in the desktop app: start it, run an outline with the model
    server stopped, check `~/Library/Logs/RoughCut/roughcut.log` has
    `app_start` and an `ai_step` fallback with reason `model_unreachable`;
    Setup → Reveal logs opens Finder there.
