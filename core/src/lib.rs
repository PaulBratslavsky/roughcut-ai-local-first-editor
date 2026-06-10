//! roughcut-core — the portable asset.
//!
//! Owns all state: project/timeline model, the tool registry (one tool set,
//! three callers: local agent loop, MCP clients, the UI), adapters behind
//! traits, undo/redo, export writers, and the MCP server/client.
//!
//! Principles (see docs/06-build-spec.md):
//! - Local-first / zero egress by default.
//! - Non-destructive editing: source media is never modified.
//! - Frame-exact internal time, seconds at the API edge.

pub mod adapters;
pub mod agent;
pub mod capabilities;
pub mod demo;
pub mod detect;
pub mod engine;
pub mod error;
pub mod events;
pub mod export;
pub mod llm_runtime;
pub mod mcp;
pub mod model;
pub mod setup;
pub mod store;
pub mod time;
pub mod tools;

pub use engine::Editor;
pub use error::{CoreError, Result};
