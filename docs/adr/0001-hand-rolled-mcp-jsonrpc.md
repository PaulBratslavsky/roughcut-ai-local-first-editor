# ADR-0001: Hand-rolled MCP JSON-RPC layer instead of `rmcp`

**Status:** accepted (2026-06)

## Context

The build spec (docs/06) names `rmcp`, the official Rust MCP SDK, for both the
server and client roles. At build time, `rmcp`'s macro API was churning across
minor versions, while the MCP wire protocol itself (JSON-RPC 2.0; `initialize`,
`tools/list`, `tools/call`) is small and stable. Claude Desktop compatibility
comes from the stdio shim, which speaks the real protocol regardless of how the
HTTP side is implemented.

## Decision

Implement the MCP server directly over axum (`core/src/mcp/server.rs`):
JSON-RPC over a localhost-only HTTP endpoint with a per-install Bearer token.
The module keeps rmcp's conceptual shape so a swap stays contained in
`core/src/mcp/`.

## Consequences

- Verified end-to-end: the e2e suite drives initialize → tools/list →
  tools/call over HTTP, and the shim test does the same over stdio.
- We own protocol-version negotiation; revisit if MCP adds capabilities we
  need (streaming tool output, resources, prompts) — that's the trigger to
  re-evaluate `rmcp`.
- Future architecture reviews should not re-suggest "use the official SDK"
  without one of those triggers.
