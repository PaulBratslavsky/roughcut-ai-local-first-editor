//! MCP — the "open editing engine" surface.
//!
//! Server: the tool registry exposed on a localhost HTTP JSON-RPC endpoint,
//! guarded by a per-install auth token; Claude Desktop reaches it through the
//! `mcp-shim` stdio proxy (Claude Desktop spawns stdio subprocesses, not GUIs).
//!
//! The wire format is MCP's JSON-RPC (initialize / tools/list / tools/call),
//! implemented directly over axum. The build spec names `rmcp`; this module
//! keeps the same shape so swapping the transport layer to rmcp is contained
//! here.
//!
//! (The old outbound "frontier" client is gone: Hosted mode — the user's
//! LOCAL / HOSTED switch, see `engine::route` — replaced it.)

pub mod server;
