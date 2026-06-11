# ADR-0004: Provisioning commands stay outside the tool registry

**Status:** accepted (2026-06)

## Context

The Tauri shell exposes a handful of bespoke commands next to the generic
`call_tool` dispatcher: `setup_status`, `download_whisper_model`,
`ollama_pull_model`, `install_llama_server`, `download_gguf`,
`start_managed_llm`, plus shell conveniences (`reveal_path`,
`confirm_action`, `mcp_endpoint_info`, `demo_mode`). Each costs a small
amount of boilerplate (Rust wrapper + TS wrapper + browser-mock stub), and an
architecture review suggested folding the provisioning ones into the tool
registry, where adding a tool is a three-line `ToolSpec` and the TS types are
generated.

## Decision

Provisioning and shell commands deliberately do NOT join the registry.

The registry has three callers by design (docs/tool-api.md): the UI, the
local agent loop, and external MCP clients. Anything in it is therefore
reachable by a model — the local Gemma picks from `agent_defs()`, and MCP
clients see `mcp_defs()`. Provisioning is a different trust category:

- **Downloads are the app's only deliberate egress.** Zero-egress
  verification (docs/zero-egress-verification.md) rests on model downloads
  being *user-triggered, never automatic*. A `download_gguf` tool in the
  registry would let a confused agent loop — or any MCP client — pull
  gigabytes from an arbitrary URL. The confirmation prompt mitigates but
  does not justify it: there is no editing workflow that needs a model to
  install a model.
- **Runtime management is shell lifecycle, not editing.** `start_managed_llm`
  spawns a child process the Tauri exit handler owns; `reveal_path` opens
  Finder; `confirm_action` IS the approval channel (a tool that answers its
  own confirmations would be circular).

The boilerplate cost is real but small (~15 lines per command, and new
provisioning features are rare); the registry's value is that *everything in
it is safe to hand to a model*, and diluting that invariant costs more.

## Consequences

- External orchestrators cannot install models or toolchains; the setup
  screen is the only provisioning surface. This is intentional.
- The browser mock stubs provisioning commands by hand (`getSetupStatus`
  returns a fixed "everything installed" shape). The mock-parity test
  (`core/tests/mock_parity.rs`) covers registry tools only — by this ADR,
  that is the correct scope.
- If a future feature genuinely needs an agent-visible provisioning action,
  it should be added as a *meta* tool (hidden from MCP, like
  `apply_instruction`) with a confirmation gate — and this ADR revisited.
