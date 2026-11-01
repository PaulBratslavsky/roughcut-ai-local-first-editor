# ADR-0007: Hosted model mode — the user can choose Claude for the AI steps

**Status:** accepted (2026-10-31)

## Context

RoughCut's AI steps (shorts scoring, the story outline, flow review, story
edits, the chat agent, transcript cleanup, chapters, titles) run on a local
model through Ollama or llama-server. That keeps the app private and offline,
but a 4–26B local model's judgement is noticeably weaker than a frontier
model's, and long videos take minutes per pass.

The only way to a stronger model was a hidden pair of meta tools
(`connect_external`, `escalate_to_frontier`): chat only, no UI, through
Anthropic's OpenAI-compatible shim, with the key passed to the `security`
CLI on its command line and a plaintext-file fallback.

The owner asked for "a toggle: local when I'm offline, better models when
I'm online", and decided (`docs/designs/hosted-models.md`, D1–D3) on Claude
only, a first-class LOCAL / HOSTED switch, and that all callers use Hosted
once the user turns it on.

## Decision

1. **One switch, `Preferences.model_mode` (`local` | `hosted`), default
   Local.** In Hosted mode every AI step goes to Claude through the native
   Messages API (`adapters/anthropic.rs`); transcription, embeddings, search
   and face framing stay on-device in both modes.
2. **Egress is enabled only by the user, in the app.** The switch, the
   hosted model, the consent timestamp and the API key are changed through
   Tauri shell commands (`model_status`, `set_model_mode`,
   `set_hosted_model`, `set_hosted_key`, `check_hosted_key`), never through
   the tool registry (ADR-0004). `set_preferences` rejects changes to those
   fields for every caller. Turning Hosted on requires a stored key and the
   user's confirmation of what is sent (`hosted_consent_at`).
3. **Once on, it is standing consent.** Runs started by MCP clients (Claude
   Code calling `find_clips`, `story_edit`, `transcribe` → cleanup) and by
   the chat agent use Hosted too. `docs/07-architecture.md` principle 1
   changes from "only a human action can trigger network egress" to "only a
   human action can enable egress".
4. **One model per run; errors stay visible.** `Editor::inference()` hands
   out a per-run `HostedRouter` (`engine/route.rs`). Only "unreachable"
   (offline, rate limited, Anthropic down) falls back to the local model,
   and a run never goes from local back to Claude. A story edit's outline,
   plan and flow checks share one run. An AI tool the chat agent calls
   (`find_clips`, `story_edit`, …) is a run of its own. No key, no consent,
   a rejected key or a rejected request is a `CoreError::Hosted` that every
   AI step re-raises instead of substituting rules or offline edits; the
   only exception is the chat's optional flow note after edits have landed,
   which is dropped quietly.
5. **The key lives in the OS key store** (`secrets.rs`, the `keyring`
   crate): Keychain on macOS, Credential Manager on Windows, the kernel
   keyring on Linux (kept until reboot). Never a file, the database or a
   command line.
6. **The hidden frontier path is removed** (`connect_external`,
   `escalate_to_frontier`, `mcp/client.rs`); plaintext keys it left are
   deleted at startup.

## Consequences

- Zero egress *by default* still holds and is still verifiable: a fresh
  install, and Local mode at any time (even with a key stored), make no
  off-machine connections — except a key check the user starts in Setup
  (Save or Test: one `GET /v1/models/{model}`). In Hosted mode the app
  connects only to `api.anthropic.com` (`docs/zero-egress-verification.md`).
- What leaves the machine in Hosted mode is text: transcripts, the project
  name, the user's chat, the agent's tool results (which can include file
  paths) and the app's prompts. Never video, audio or frames.
- An MCP client can spend the user's Anthropic credits once Hosted is on.
  Spend is visible in the Anthropic Console (linked from Setup); a cap is
  future work.
- On Linux the key does not survive a reboot (the kernel keyring); a
  persistent Secret Service backend needs a system library the CI's core
  job doesn't install, and is future work.
- The product promise in `docs/01-product.md` becomes "local by default;
  hosted when you choose, with your own key, for text only".
