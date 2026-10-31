# Local or hosted models

*   **Author:** Paul Bratslavsky
*   **Status:** Draft, revised after a fresh-eyes review (four reviewers);
    no open questions (D1–D3 decided by the owner 2026-10-31)
*   **Date:** 2026-10-31

--------------------------------------------------------------------------------

## Overview

Add one switch to RoughCut: **Local** or **Hosted**. Local is today's app:
every AI step runs on the user's machine through Ollama or llama-server, and
it works offline. Hosted sends the same steps to Claude through the user's
own Anthropic API key, for better judgement when the machine is online:
which moments make good shorts, what the story beats are, which cut reads
broken, what to cut when asked in chat.

Local stays the default. Only the user, in the app, can turn Hosted on, after
a notice that says exactly what leaves the machine: text (transcripts, the
user's chat, the app's prompts and tool results), never video, audio or
frames. Transcription, search and face framing stay on-device in both modes.
A run that starts Hosted and loses the network finishes on the local model
and says so; a rejected key stops with a clear error instead of quietly
falling back.

## The Problem

*   Shorts scoring, the outline, flow review and chat run on a 4–26B local
    model. On a 24-minute video a scan takes minutes, and the judgement is
    weaker than a frontier model's (titles, which moment has a payoff, which
    seam reads broken).
*   The only path to a stronger model today is hidden and narrow:
    `connect_external` + `escalate_to_frontier` (`core/src/mcp/client.rs`).
    It covers chat only, one instruction at a time, through Anthropic's
    OpenAI-compatible shim with `claude-sonnet-4-6` (or `FRONTIER_MODEL`).
    No UI calls it; only the browser mock has stubs. It stores the key by
    passing it to `security add-generic-password -w <key>` on the command
    line (readable from the process list) with a plaintext-file fallback.
*   `Preferences.inference_endpoint` can point at any OpenAI-compatible
    server, but `Editor::inference()` never sends an API key, so it can't
    reach a hosted provider; and the app's request shape (`temperature`,
    `response_format`) is the local servers'.
*   Every AI call site treats **any** model error as "no model" and falls
    back to rules: scoring keeps rule scores, the outline uses pause-based
    beats, and the chat agent runs its offline path, which **applies
    keyword cuts** (`agent.rs:296`, `:644`). That is fine for a local
    outage; for a hosted 401 it would edit the user's video under a
    misleading "model not reachable" message.

## Goals

*   One **Local / Hosted** switch: a chip in the top bar (and a line on the
    import screen) and a Models row in Setup. It routes all eight AI steps
    in the table below.
*   Hosted uses Claude through the **native Messages API** with the user's
    key in the OS's secure key store (Keychain on macOS, Credential Manager
    on Windows, Secret Service on Linux). Default model `claude-opus-5-5`;
    Setup offers `claude-sonnet-5-5` and `claude-haiku-5-5`.
*   **One model per run.** A scan, an outline, a chat instruction or a story
    edit uses one provider from start to finish, with one exception: a
    stateless batch run (scan, outline, cleanup) that loses the network
    finishes on the local model and records which items each model judged.
*   **Errors stay visible.** A rejected key, a bad request or no key stops
    the run with a clear message and changes nothing; only "unreachable"
    falls back.
*   **Zero egress by default.** A fresh install, and Local mode at any time,
    make no off-machine connections. Turning Hosted on is a user action in
    the app, outside the tool registry (ADR-0004); no MCP client or agent
    can turn it on, change its model, or read the key.
*   **Honest wording.** Every "on-device" / "local model" string in the app
    and in tool descriptions is either neutral or says which model ran.
*   Old installs load unchanged, in Local mode.

## Non-Goals

*   **Hosted transcription or embeddings.** Whisper and the embedder stay
    on-device; without a local server, search already falls back to keyword
    (BM25) only.
*   **Other providers** as a Hosted choice (D1).
*   **Per-step routing.** One switch; per-step choices are a Future
    Improvement if users ask.
*   **Showing cost in the app.** Setup links to the Anthropic Console for
    spend. Responses' `usage` is parsed so a cost line can come later.
*   **Per-step effort tuning.** Requests use the model's default effort
    until the measurement step shows a reason to change it.
*   **Streaming.** Every hosted reply here is a short JSON or a tool call.

## Open Questions

None remain. Decided by the owner on 2026-10-31:

*   **D1, providers: Claude only**, through the native Messages API. A
    custom OpenAI-compatible provider is Alternative F.
*   **D2, prominence: a first-class toggle** — the chip in the top bar.
    `docs/01-product.md` says "local by default; hosted when you choose,
    with your own key, for text only"; Local stays the default and the
    mission's proof point. Setup-only is Alternative G.
*   **D3, callers: all callers use Hosted once it is on.** Turning Hosted on
    is the user's standing consent, so runs started by MCP clients (Claude
    Code) and by the chat agent use it too. `docs/07-architecture.md` §7
    changes from "only a human action can trigger network egress" to "only
    a human action can enable egress". App-started-only is Alternative H.

## Critical User Journeys (CUJs)

### CUJ 1: Turn on Hosted

*   **Given**: Local mode, no key stored, a transcribed project.
*   **When**: the user clicks the **LOCAL** chip.
*   **Then**: Setup opens at the Models row (there is no key yet). The user
    pastes a key and clicks Save. The app checks it against Anthropic
    without spending tokens and shows "Key works · Claude Opus 5.5". The user
    clicks **Hosted**. A confirm says: "AI steps will send text to
    Anthropic: transcripts, your chat messages, and the app's prompts and
    results (which can include file names). Video, audio and frames stay on
    this machine. After each transcription, the transcript clean-up runs on
    Anthropic automatically." The user confirms; the chip reads **HOSTED ·
    Opus 5.5**. Cancel keeps Local.

### CUJ 2: Scan for shorts with Hosted

*   **Given**: Hosted mode; the project was outlined earlier by the local
    model.
*   **When**: the user clicks "Scan for shorts".
*   **Then**: the outline is recomputed (the cached one came from another
    model), then Claude scores the windows. Progress reads "scoring
    candidates with Claude Opus 5.5 — 3/15". The cards' note reads "Scored
    by Claude Opus 5.5".

### CUJ 3: Go offline mid-scan

*   **Given**: Hosted mode; a scan at batch 6 of 15; the Wi-Fi drops.
*   **When**: batch 7 can't connect.
*   **Then**: batches 7–15 run on the local model (if it is running; if not,
    they keep rule scores, as today). The chip reads **HOSTED → LOCAL
    (offline)**. The cards' note reads "Scored by Claude Opus 5.5 and the
    on-device model". The next run tries Hosted again.

### CUJ 4: A wrong key

*   **Given**: Hosted mode with a revoked key.
*   **When**: the user scans, or asks the chat "cut the tangent about my
    weekend".
*   **Then**: the run stops at the first request with "Anthropic rejected
    the API key (401). Update it in Setup." The candidate list and the
    timeline are unchanged; no rule-based scores, no offline cuts. The chip
    reads **HOSTED · key rejected**.

### CUJ 5: Back to Local mid-run

*   **Given**: a Hosted scan at batch 4 of 15.
*   **When**: the user switches the chip to Local.
*   **Then**: no further request goes to Anthropic; batches 5–15 run
    locally. A Hosted chat run that is mid-way stops after the current step
    with its partial result ("switched to Local; stopped").

### CUJ 6: Claude Code tries to switch

*   **Given**: Claude Code connected over MCP.
*   **When**: it calls `set_preferences { preferences: { model_mode:
    "hosted" } }`.
*   **Then**: the call fails: "model_mode can only be changed in the app".
    `get_preferences` shows the current mode and model.

## Technical Plan (Architecture)

### What routes through the switch

All eight AI steps get their client from `Editor::inference()` (verified:
nothing else builds an inference client except `escalate_to_frontier`,
which this design removes).

| Step | Code | Request | Run kind |
| --- | --- | --- | --- |
| Shorts scoring | `clip_finder::llm_rescore` | schema `short_scores` (array root), batches of 6 | batch |
| Story outline | `story::outline_with` → `llm_outline` | schema (array root), chunks of 120 segments | batch |
| Transcript cleanup | `agent::clean_transcript` | JSON, chunks of 40 segments; runs after every transcription | batch |
| Flow review | `story::review_flow_mode` (when `llm`) | schema (array root), one call | single |
| Chapters | `agent::llm_chapters` | JSON, one call | single |
| Title / description | `agent::generate_title_description` | JSON, one call | single |
| Story edit | `story::story_edit` | outline + plan (schema, object root) + flow review | conversation |
| Chat agent | `agent::run_instruction_with` | tool-calling loop, up to 12 steps | conversation |

Not routed: transcription, embeddings and search, silence and filler
detection, face framing.

### Overview

```text
 call site (8)                      unchanged except error handling
   │  editor.inference()  / editor.inference_conversation()
   ▼
 Local mode ─────────────► OpenAiCompatClient (as today)
 Hosted mode ────────────► HostedRouter (one per run)
                             ├─ AnthropicClient ── api.anthropic.com
                             ├─ OpenAiCompatClient (local fallback)
                             └─ RouteState (in Editor: key cache,
                                chip status, last error) ─► model-route event
```

### Data model (`core/src/model.rs`, `events.rs`, `error.rs`)

*   `enum ModelMode { #[default] Local, Hosted }` (`snake_case`).
*   `Preferences` gains:
    *   `model_mode: ModelMode` — `#[serde(default)]`.
    *   `hosted_model: String` — `#[serde(default = "default_hosted_model")]`
        returning `"claude-opus-5-5"` (the `embedding_model` pattern).
    *   `hosted_consent_at: Option<DateTime<Utc>>` — `#[serde(default)]`.
    These three are **read-only through `set_preferences`** for every
    caller (the update is rejected: "model_mode can only be changed in the
    app"); only the shell commands below write them.
*   `pub const HOSTED_MODELS: &[(&str, &str)]` — `("claude-opus-5-5",
    "Claude Opus 5.5")`, `("claude-sonnet-5-5", "Claude Sonnet 5.5")`,
    `("claude-haiku-5-5", "Claude Haiku 5.5")`; the one place IDs map to
    display names.
*   `ClipCandidate.scorer: Option<String>` — `#[serde(default)]`; the display
    name of the model that scored it, `None` for rule scores.
*   `enum RouteReason { Offline, RateLimited, ServerError, KeyRejected,
    NoKey }`.
*   `struct ModelStatus { mode, hosted_model, hosted_model_name,
    key_present: bool, consented: bool, effective: ModelMode, reason:
    Option<RouteReason>, last_ok_at: Option<DateTime>, last_error:
    Option<String> }` — ts-rs exported; in memory except the preference
    fields.
*   `CoreEvent::ModelRoute { mode, effective, reason }`, wire name
    `model-route`; sent by `RouteState` when `(effective, reason)` changes.
*   `CoreError::Hosted { kind: HostedErrorKind, message: String }` with
    `HostedErrorKind { NoKey, NotConsented, KeyRejected, BadRequest,
    Refused, Truncated }`; `code()` returns `hosted_<kind>`. Helper
    `CoreError::is_fatal_model_error()` is true for `NoKey`,
    `NotConsented`, `KeyRejected` and `BadRequest`.
*   `ChatMessage.provider_blocks: Option<Value>` and `ChatResponse.served_by:
    ServedBy { model: String, hosted: bool }` plus `ChatResponse.usage:
    Option<Usage>`; all `#[serde(skip)]`, so local requests are
    byte-identical to today's.

### Routing (`core/src/engine/route.rs`, new)

`Editor::inference()` keeps its signature. In Local mode it returns today's
client. In Hosted mode it returns a new `HostedRouter` (one per call; every
call site already calls `inference()` once per run and reuses the client
across its chunk or step loop). `Editor::inference_conversation()` returns
the same router with the conversation policy; `agent::run_instruction_with`
and `story::story_edit` use it.

`HostedRouter` holds the `AnthropicClient`, the local `OpenAiCompatClient`,
the local model name, `Arc<RouteState>`, a handle to read preferences, the
policy (`Batch` or `Conversation`), and its pin (`Unpinned | Hosted |
Local`). It **owns the model name**: it ignores `request.model` and sends
`hosted_model` to Anthropic or `inference_model` to the local server, so a
call site can never send a Claude ID to Ollama.

Before **every** request the router re-reads `model_mode` and whether a key
is cached. Then:

| Situation | Batch policy | Conversation policy |
| --- | --- | --- |
| Mode is now Local (user switched) | pin Local, run locally | if a hosted step already ran: stop the run (`CoreError::Unavailable("switched to Local; stopped")`, the agent's existing partial-result path); else run locally |
| No key cached | `Hosted{NoKey}` | same |
| No consent stored | `Hosted{NotConsented}` | same |
| Pinned Local | local | local |
| Hosted request **unreachable** (table below) | pin Local, re-run this request locally if the local server is healthy; else return the local "not reachable" error | if no hosted step succeeded yet: pin Local and run locally; else stop the run with the partial result |
| Hosted 401/403 | `Hosted{KeyRejected}`; `RouteState` records it | same |
| Hosted 4xx other | `Hosted{BadRequest}` with the API's message | same |
| `stop_reason: refusal` | `Hosted{Refused}` | same |
| `stop_reason: max_tokens` | `Hosted{Truncated}` | same |

The router never goes local → hosted within a run, so local-made turns
are never sent to Anthropic, and Anthropic's thinking blocks never reach
the local server (`provider_blocks` is not serialised there).

`healthy()`: in Hosted mode, true when a key is cached and consent is
stored, or when the local server is healthy; no network call. With no key
it returns **true**, so the call site makes a request and gets the visible
`NoKey` error instead of silently using rules.

`RouteState` (in `Inner`) holds the cached key (read once from the key
store, replaced or cleared by the commands), the chip status (`effective`,
`reason`, `last_ok_at`, `last_error`), and sends `model-route` when the
status changes. Routers update it after each request.

### Error handling at the call sites (feature code changes)

Each catch-all that turns a model error into a rule-based result first
re-raises `is_fatal_model_error()` errors:

*   `clip_finder::find_clips` (the `llm_rescore` result, `:185-188`) and
    `llm_rescore`'s per-batch `continue` (`:637-642`): fatal → the scan
    fails and the stored candidates are left as they were. `Refused` /
    `Truncated` on a batch keep that batch's rule scores, as a malformed
    reply does today.
*   `story::outline_with` (`:90-93`), `review_flow_mode` (`:494`),
    `story_edit` (plan call, `:686`).
*   `agent::run_instruction_with` step 0 (`:296`): the offline path runs
    only for a local-unreachable error, never for a `Hosted` error.
*   `agent::clean_transcript` (`:769-789`), `llm_chapters` (`:812-819`),
    `generate_title_description` (`:864-879`). Cleanup runs in the
    background after transcription: its fatal error is recorded in
    `RouteState.last_error` (shown in Setup and on the chip), and the
    transcript stays uncleaned.
*   `llm::ask_json_schema` (`:86-91`) resends a request without the schema
    when the first attempt errors; it now does so only for errors from a
    local server. Hosted requests are never resent there (structured
    outputs guarantee parseable JSON; a 400 must stay visible; a refusal
    must not be billed twice).
*   `story::review_flow_mode` resolves the client only when `llm` is true.

### The hosted client (`core/src/adapters/anthropic.rs`, new)

`AnthropicClient` implements `InferenceClient`. Rust has no official
Anthropic SDK, so it calls the Messages API over `reqwest` (already a
dependency), not the OpenAI-compatible shim. A crate-private
`with_base_url` lets tests point it at a local stub.

**Request**: `POST {base}/v1/messages` with `x-api-key`,
`anthropic-version: 2026-10-21`, `content-type: application/json`,
`anthropic-beta: server-side-fallback-2026-07-01`. Body:

*   `model` (from the router), `max_tokens: 16000`.
*   `system`: all `system` messages joined with a blank line.
*   `messages`, translated in order:
    *   `user` text → a text block; `assistant` text → a text block; empty
        text is omitted.
    *   An assistant message with `provider_blocks` → those blocks,
        unchanged (thinking blocks included). The router debug-asserts the
        blocks still match the message's text and tool calls.
    *   Assistant `tool_calls` → `tool_use` blocks (`input` parsed from the
        JSON-string `arguments`), after any text block.
    *   `tool` messages → `tool_result` blocks.
    *   Consecutive messages of the same role are merged into one message
        (tool results first, then text) — the agent's "stop searching, act
        now" nudge follows tool results, and `ask_json_schema`'s repair
        pass sends two user turns.
    *   If the first non-system message is an assistant turn (chat history
        is the last 8 text turns), a user message "Earlier in this
        conversation:" is put before it. A message left with no blocks is
        dropped.
*   `tools`: OpenAI `{type: "function", function: {name, description,
    parameters}}` → `{name, description, input_schema}`. No `tool_choice`.
*   `response_format` (json_schema) → `output_config.format: {type:
    "json_schema", schema}` after **normalisation**: recursively through
    `properties`, `items` and `anyOf`, drop `minimum`, `maximum`,
    `exclusiveMinimum`, `exclusiveMaximum`, `multipleOf`, `minLength`,
    `maxLength`, `pattern`, `minItems`, `maxItems`, `uniqueItems`; keep
    `type`, `properties`, `required`, `items`, `enum`, `const`,
    `description`, `anyOf`; add `additionalProperties: false` to every
    object. A root **array** schema is wrapped as `{type: "object",
    properties: {items: <schema>}, required: ["items"],
    additionalProperties: false}` and the reply's `items` is unwrapped
    before parsing. (Ranges are still enforced: callers clamp scores and
    check `cut_priority` after parsing.)
*   `cache_control: {type: "ephemeral"}` at the top level (automatic prompt
    caching: the chat agent resends a system prompt of up to ~24k
    characters and its tool list on each of up to 12 steps; short prompts
    just don't cache).
*   `fallbacks: "default"`: on a safety refusal the API re-runs the request
    on Anthropic's recommended fallback model in the same call.
*   Never sent: `temperature` (rejected on current models), `tool_choice`,
    `thinking` (adaptive by default; it can't be turned off on Opus 5.5),
    `effort` (default).

**Response**: text blocks joined with a newline → `content`; `tool_use`
blocks → `tool_calls` (input re-encoded as a JSON string); all blocks →
`provider_blocks`; the response's `model` field → `served_by` (it names
the fallback model when one answered); `usage` → `usage`. A reply with
only thinking blocks gives empty `content`.

**Errors and retries**:

| Condition | Retries | Result |
| --- | --- | --- |
| Connect error, connect timeout (5 s), request timeout (120 s) | none | unreachable (`Offline`) |
| 429 | 2, waiting `retry-after` (at most 10 s) or 1 s then 3 s | unreachable (`RateLimited`) |
| 500, 502, 503, 504, 529 | 2, 1 s then 3 s | unreachable (`ServerError`) |
| 401, 403 | none | `Hosted{KeyRejected}` |
| other 4xx (400, 404 model, 413, 422) | none | `Hosted{BadRequest}` with the API's message |
| 200, `stop_reason: refusal` | none | `Hosted{Refused}` |
| 200, `stop_reason: max_tokens` | none | `Hosted{Truncated}` |

Backoff delays are configurable; tests set them to zero. A connection that
stalls mid-request waits up to the 120 s timeout before falling back.

### Keys (`core/src/secrets.rs`, new)

*   `trait SecretStore { get, set, delete }`; `KeyringStore` uses the
    cross-platform `keyring` crate (no subprocess, no command-line key);
    `MemoryStore` is used by `Editor::test_instance()`, so tests never touch
    the developer's key store.
*   Service `dev.localfirst.roughcut.hosted`, account `anthropic` (distinct
    from anything the old path wrote).
*   No plaintext fallback anywhere: where the platform has no secure store
    (e.g. headless Linux), saving a key fails with "this system has no
    secure key store; Hosted needs one".
*   The key is read once into `RouteState` and replaced or cleared by the
    commands; `model_status` and the 5 s Setup poll never touch the store.
*   On macOS the item's access list is the app's code signature; builds
    signed with a stable identity (the release script's "RoughCut Local
    Signing") read it without prompting, an ad-hoc dev build may prompt once.
*   On upgrade, plaintext files the old path wrote (`<data dir>/secrets/
    roughcut-*`) are deleted; the old path is removed (below).

### Commands (`app/src-tauri`, ADR-0004)

Enabling egress and handling keys stay out of the tool registry, as Tauri
shell commands next to `setup_status`, each a thin wrapper over an Editor
method (the core owns the state):

*   `model_status() -> ModelStatus` — in memory, no network, no store read.
*   `set_hosted_key(key: Option<String>) -> HostedKeyResult { status:
    ModelStatus, check: Option<KeyCheck { ok, latency_ms, error } > }` —
    `Some`: checks the key with `GET /v1/models/{hosted_model}` (no tokens;
    proves the key and access to the model), saves it only when the check
    passes or the network is unreachable ("saved; couldn't verify while
    offline"), and caches it. `None`: deletes it and sets the mode to Local.
*   `check_hosted_key() -> KeyCheck` — the same check, for Setup's "Test".
*   `set_model_mode(mode, consent: bool) -> ModelStatus` — Hosted requires
    a cached key, and `consent: true` the first time (stores
    `hosted_consent_at`).
*   `set_hosted_model(model) -> ModelStatus` — one of `HOSTED_MODELS`.

### Tools and caller policy (`core/src/tools.rs`)

*   **Removed**: `connect_external`, `escalate_to_frontier` and
    `core/src/mcp/client.rs`. Hosted chat replaces them; nothing in the UI
    used them.
*   `get_preferences` (UI, MCP; `agent: false` as today) shows `model_mode`
    and `hosted_model`.
*   `set_preferences` (UI, MCP; `agent: false`): rejects `model_mode`,
    `hosted_model`, `hosted_consent_at` for every caller.
*   Tool descriptions that say "ON-DEVICE model" / "on-device semantic
    pass" (`tools.rs:1248-1275`) become "the configured model (on-device
    unless the user chose Hosted)".

### Outline cache (`core/src/story.rs:74-106`)

The cached tuple gains the model that made the outline (`served_by.model`
of its calls). A cached outline counts only when that model is the one the
current mode would use (`hosted_model` in Hosted, `inference_model` in
Local). An outline made partly by each (batch fallback) is not cached.

### Wording (mode-aware)

Strings that name the model take it from `Editor::model_label()` ("Claude
Opus 5.5" or "the on-device model"): `clip_finder.rs:181` ("scoring
candidates on-device"), `agent.rs:222`, `:277-280` ("the local model"),
`engine/media.rs:112` ("with the local LLM"), `story.rs:596`, `:611` ("No
local model server"). Frontend: `SetupPanel.tsx:345-347` footer
("Everything runs on this machine"), `EmptyState.tsx:78`,
`ClipsPanel.tsx:174`, `ShortsScreen.tsx:257`, `ChatPanel.tsx:274` — neutral
or mode-aware.

### Frontend

*   **Chip** (`TopBar.tsx`; a one-line equivalent on `EmptyState.tsx`, where
    imports start): reads `model_status()` at start and follows
    `model-route` events (ignoring an event whose `mode` doesn't match the
    current mode). Labels: `LOCAL`; `HOSTED · Opus 5.5`; `HOSTED → LOCAL
    (offline | rate limited | Anthropic unavailable)`; `HOSTED · key
    rejected`. Click: with no key, open Setup's Models row; otherwise
    switch modes (the CUJ 1 confirm the first time).
*   **Setup** (`SetupPanel.tsx`): a Models row above "Chat editing": Local /
    Hosted, key field (masked) with Save, Test and Forget, model picker,
    last error, and a link to the Anthropic Console for spend. In Hosted
    mode the "Chat editing" and "Semantic search" rows read "optional:
    offline fallback" and "optional: without it, search uses keywords".
*   **Shorts screen**: the note uses the candidates' `scorer` values
    ("Scored by Claude Opus 5.5", "… and the on-device model", "Scored
    without a model").
*   **Browser mock**: the commands are mocked (Hosted always works;
    `?nohosted` makes requests unreachable; `?badkey` rejects the key).

## Alternatives Considered

### Alternative A: Point `inference_endpoint` at Anthropic's OpenAI-compatible endpoint

*   **Pros**: no new request translation.
*   **Cons**: needs the key plumbed anyway; the app's `temperature` is
    rejected by current models; structured outputs, thinking and refusal
    fallbacks aren't expressed the same way.
*   **Reason for Rejection**: a hosted path should speak the API it targets.

### Alternative B: Keep "escalate to frontier"

*   **Pros**: exists.
*   **Cons**: chat only, no UI, a second key and model setting next to
    Hosted, and its key item collides with the name Hosted would use.
*   **Reason for Rejection**: Hosted chat covers it; removing it removes a
    hidden egress path.

### Alternative C: Automatic mode (hosted whenever online)

*   **Cons**: text leaves the machine whenever there is a network.
*   **Reason for Rejection**: egress must be the user's explicit choice.

### Alternative D: Fall back per request, with a 60-second reachability cache

*   **Pros**: each request picks the best available model.
*   **Cons**: one scan mixes models without saying so; a chat run can go
    hosted → local → hosted and send local-made turns (without thinking
    blocks) to Anthropic; the model name sent can disagree with the server.
*   **Reason for Rejection**: per-run pinning is simpler and honest.

### Alternative E: Mode and key as registry tools

*   **Cons**: ADR-0004 keeps provisioning and egress-enabling actions out of
    the registry ("everything in it is safe to hand to a model").
*   **Reason for Rejection**: Tauri commands, like `setup_status`.

### Alternative F: A custom OpenAI-compatible provider next to Claude (D1b)

*   **Pros**: users with another provider's key can use Hosted.
*   **Cons**: about 2 more days; the custom path sends the local request
    shape, so what works depends on the provider; a second key slot.
*   **Reason for Rejection**: owner chose Claude only (D1). Local mode can
    already point at a bigger model on the user's own network.

### Alternative G: Hosted only in Setup (D2b)

*   **Pros**: closer to the current mission wording.
*   **Cons**: the user asked for a toggle to pick quickly; Setup is two
    clicks away and the current mode isn't visible while editing.
*   **Reason for Rejection**: owner chose a first-class toggle (D2).

### Alternative H: Hosted for app-started runs only (D3b)

*   **Pros**: an MCP client can't spend the user's credits unless allowed.
*   **Cons**: about 1 more day to thread the caller into the eight steps; a
    second setting to explain; Claude Code would get weaker scoring by
    default.
*   **Reason for Rejection**: owner chose all callers (D3); spend is visible
    in the Anthropic Console linked from Setup.

## Non-Functional Requirements

*   **Privacy**: Local mode and a fresh install make no off-machine
    connections, with or without a stored key. Hosted mode connects only to
    `api.anthropic.com:443`, for the steps in the table plus the key check.
    Sent: transcript text (with segment times and ids), the project name,
    beat titles and summaries, the user's chat and the last 8 chat turns,
    the agent's tool results (timeline and transcript JSON, which can
    include the media file's path), and the app's prompts. Never: video,
    audio, frames, thumbnails.
*   **Cost (estimate; measured in Verification)**, a 24-minute video on
    Claude Opus 5.5 ($4 / $20 per million input / output tokens): outline
    $0.15–0.30; a 40-card scan $0.30–0.50 (15 calls, about 2.5k tokens in
    and under 1k out each, thinking included); cleanup $0.10–0.30; a chat
    instruction $0.05–0.40 depending on steps (its long system prompt is
    cached after the first step). Sonnet 5.5 about half; Haiku 5.5 a small
    fraction.
*   **Latency**: a hosted scan should take no longer than a local one on the
    same machine.
*   **Compatibility**: local requests are byte-identical to today's; old
    preferences load as Local with `hosted_model` = Opus 5.5.

--------------------------------------------------------------------------------

### Additional Resources

*   [`docs/01-product.md`](../01-product.md) — the local-first promise.
*   [`docs/07-architecture.md`](../07-architecture.md) §7 — egress.
*   [ADR-0004](../adr/0004-provisioning-outside-the-tool-registry.md) —
    provisioning outside the registry.
*   [`docs/zero-egress-verification.md`](../zero-egress-verification.md).
*   Code: `core/src/adapters/inference.rs`, `core/src/llm.rs`,
    `core/src/engine/mod.rs` (`inference()`, `set_embedder_for_tests`),
    `core/src/engine/projects.rs` (`update_preferences`),
    `core/src/mcp/client.rs`, `core/src/error.rs`, and the call sites in the
    routing table.

### Future Improvements

*   A cost line per run and a monthly cap (usage is already parsed).
*   Per-step effort, once measured.
*   Per-step routing, if users ask.
*   Streaming, so a stalled connection is noticed in seconds.
*   Hosted multi-moment shorts (bulk shorts Phase 3) — likely the step that
    gains most from a frontier model.

--------------------------------------------------------------------------------

## Detailed Implementation Plan

Rough effort: 1.5–2 weeks. Each step leaves `cargo test` and the frontend
build green.

#### 1. Types (`core/src/model.rs`, `events.rs`, `error.rs`, `engine/projects.rs`)

*   `ModelMode`, the three preference fields, `HOSTED_MODELS`,
    `RouteReason`, `ModelStatus`, `ClipCandidate.scorer`,
    `CoreEvent::ModelRoute` (`model-route`), `CoreError::Hosted` +
    `is_fatal_model_error`; `update_preferences` rejects the three fields
    for every caller. Regenerate types; add `model-route` to `AppEventName`
    in `frontend/src/ipc/types.ts`.
*   Tests: old preferences load as Local with `hosted_model ==
    "claude-opus-5-5"`; `set_preferences` with `model_mode` fails for UI and
    MCP sources.

#### 2. Remove the old frontier path

*   Delete `connect_external`, `escalate_to_frontier` (registry rows and
    handlers), `core/src/mcp/client.rs`, their `docs/tool-api.md` lines and
    `mockApi.ts` stubs, and the names in `core/tests/e2e.rs`' meta-tool
    lists. Parity tests stay green.

#### 3. Keys (`core/src/secrets.rs`, `core/src/lib.rs`, `core/Cargo.toml`)

*   `SecretStore`, `KeyringStore`, `MemoryStore`; deletion of old plaintext
    files. Tests: `MemoryStore` round trip; the `KeyringStore` round trip is
    `#[ignore]` (it touches the real key store).

#### 4. `AnthropicClient` (`core/src/adapters/anthropic.rs`, `adapters/inference.rs`, `adapters/mod.rs`)

*   `provider_blocks`, `served_by`, `usage`; translation, normalisation,
    errors and retries as specified. Tests against an axum stub on
    `127.0.0.1` (axum is already a dependency), asserting the exact request
    body: system joining; same-role merging (tool results then the nudge);
    a leading assistant turn; empty text; text plus tool calls;
    `provider_blocks` sent unchanged; tools; no `temperature` /
    `tool_choice`; golden normalised schemas for all four schemas
    (`clip_finder.rs:577`, `story.rs:143`, `:462`, `:678`) including array
    wrapping and unwrapping; each row of the error table, with retries and
    `retry-after`; `served_by` from the response `model`.

#### 5. Routing (`core/src/engine/route.rs`, `engine/mod.rs`)

*   `RouteState` in `Inner`, `HostedRouter`, `inference()` /
    `inference_conversation()`, `model_label()`, and
    `set_inference_for_tests(local, hosted)` (like `set_embedder_for_tests`).
*   Tests with mock clients: every row of the routing table for both
    policies; the model name sent to each client; no local → hosted within
    a run; switching to Local mid-run; `healthy()` with no key; the
    `model-route` event fires on change only.

#### 6. Call sites (`clip_finder.rs`, `story.rs`, `agent.rs`, `llm.rs`, `engine/media.rs`)

*   The fatal-error re-raises, the `ask_json_schema` retry rule,
    `inference_conversation()` in the agent and story edit, the outline
    cache's model, `scorer` on candidates, `model_label()` wording, tool
    description wording.
*   e2e (`core/tests/e2e.rs`, with a mock hosted client returning 401):
    `find_clips` fails with the CUJ 4 message and the stored candidates are
    unchanged; `apply_instruction` fails with zero `EditAction`s; a mock
    that is unreachable from batch 3 gives candidates scored by both; an MCP
    `set_preferences { preferences: { model_mode: "hosted" } }` is
    rejected.

#### 7. Commands (`app/src-tauri/src/lib.rs`, `core` Editor methods)

*   `model_status`, `set_hosted_key`, `check_hosted_key`,
    `set_model_mode`, `set_hosted_model`, with Editor methods and tests for
    each rule (consent required once; Hosted without a key refused; Forget
    switches to Local).

#### 8. Frontend (`TopBar.tsx`, `EmptyState.tsx`, `SetupPanel.tsx`, `ShortsScreen.tsx`, `ClipsPanel.tsx`, `ChatPanel.tsx`, `ipc/api.ts`, `ipc/mockApi.ts`, `styles.css`)

*   The chip, the confirm, the Models row, optional labels on the local
    rows, the scorer note, wording, command wrappers, mock switches.

#### 9. Docs

*   `docs/tool-api.md` (removed tools, read-only preference fields, the
    `model-route` event in the events table), `CONTEXT.md` (**ModelMode**,
    **HostedRouter**), `docs/01-product.md` (the D2 sentence),
    `docs/07-architecture.md` §1/§7 (the D3 wording), a Hosted section in
    `docs/zero-egress-verification.md`, a new ADR recording the egress
    change, and the core READMEs.

### Verification Plan

*   `cargo test -p roughcut-core`; regenerated types leave no diff; `cd
    frontend && npx tsc -b && npm run build`; the browser mock walks CUJs 1,
    3, 4, 5 with `?nohosted` / `?badkey`.
*   **Egress**: resolve `api.anthropic.com`'s addresses at test time
    (`dig +short`), then sample `lsof -nP -i -a -p <pid>` at 2 Hz:
    *   Local mode with a key stored, a full session (transcribe, rough cut,
        scan, chat, export) → no non-loopback peers.
    *   Hosted mode, the same session → non-loopback peers only among the
        resolved Anthropic addresses.
*   **Live check with the user's key** (the user runs it, or approves the
    spend first): CUJs 1–5 in the installed app; record time and the
    Console's spend for one outline, scan and chat instruction on Local and
    Hosted.
*   **Quality**: on two of the user's videos, refresh the outline, then scan
    with Local and with Hosted (Opus 5.5). The user grades both top-10 lists
    blind (shuffled, no labels) as "would post / wouldn't". Hosted passes if
    its list has at least 3 more "would post" than Local's on both videos.
    Optional: the same with Sonnet 5.5, to decide whether Setup should
    suggest it.

--------------------------------------------------------------------------------

*Drafted with EGM*
