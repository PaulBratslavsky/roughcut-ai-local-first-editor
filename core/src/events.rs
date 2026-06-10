//! Core → UI push, typed. Every event the core can emit is a [`CoreEvent`]
//! variant; the name and payload shape are serialized at exactly one choke
//! point ([`CoreEvent::name`] / [`CoreEvent::payload`]), so the contract in
//! docs/tool-api.md has a single source of truth on the Rust side.
//!
//! The Tauri layer implements [`EventSink`] with app events; the MCP server
//! and tests use [`NullSink`] or a channel.

use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum CoreEvent {
    Progress {
        /// "transcribe" | "rough_cut" | "export" | "render" | "model_download"
        task: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        project_id: Option<Uuid>,
        fraction: f64,
        message: String,
    },
    AgentStep {
        project_id: Uuid,
        step: usize,
        /// "thinking" | "tool_call" | "tool_result" | "final"
        kind: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        args: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    TimelineChanged {
        project_id: Uuid,
    },
    /// Project created / duplicated / deleted — refetch the library list.
    ProjectsChanged {},
    TranscriptChanged {
        project_id: Uuid,
    },
    McpReady {
        url: String,
        token: String,
    },
}

impl CoreEvent {
    pub fn progress(
        task: &str,
        project_id: Option<Uuid>,
        fraction: f64,
        message: impl Into<String>,
    ) -> Self {
        CoreEvent::Progress {
            task: task.to_string(),
            project_id,
            fraction,
            message: message.into(),
        }
    }

    /// The wire name the frontend listens on.
    pub fn name(&self) -> &'static str {
        match self {
            CoreEvent::Progress { .. } => "progress",
            CoreEvent::AgentStep { .. } => "agent-step",
            CoreEvent::TimelineChanged { .. } => "timeline-changed",
            CoreEvent::ProjectsChanged {} => "projects-changed",
            CoreEvent::TranscriptChanged { .. } => "transcript-changed",
            CoreEvent::McpReady { .. } => "mcp-ready",
        }
    }

    pub fn payload(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

pub trait EventSink: Send + Sync {
    fn emit(&self, event: &str, payload: Value);
}

#[derive(Default)]
pub struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _event: &str, _payload: Value) {}
}

pub type SharedSink = Arc<dyn EventSink>;

/// The one place an event turns into (name, payload) on the wire.
pub fn send(sink: &SharedSink, event: CoreEvent) {
    sink.emit(event.name(), event.payload());
}
