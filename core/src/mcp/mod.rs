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
//! Client: explicit, opt-in outbound path (`connect_external`,
//! `escalate_to_frontier`). Never auto-invoked — local by default, frontier
//! by choice.

pub mod client;
pub mod server;
