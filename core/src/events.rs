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

/// Every long-running operation that reports progress, by name. The ONE list:
/// emitters pass a variant, the frontend imports the generated union — adding
/// an operation here is the whole registration.
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressTask {
    Transcribe,
    RoughCut,
    Export,
    ModelDownload,
    ModelPull,
    RuntimeInstall,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum CoreEvent {
    Progress {
        task: ProgressTask,
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
    /// Derived media (peaks/thumbnails/playable copy) finished generating in
    /// the background — refetch get_media_assets.
    MediaAssetsChanged {
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
    /// A destructive op driven by an EXTERNAL client wants user approval.
    ConfirmRequest {
        id: Uuid,
        summary: String,
    },
}

impl CoreEvent {
    pub fn progress(
        task: ProgressTask,
        project_id: Option<Uuid>,
        fraction: f64,
        message: impl Into<String>,
    ) -> Self {
        CoreEvent::Progress { task, project_id, fraction, message: message.into() }
    }

    /// The wire name the frontend listens on.
    pub fn name(&self) -> &'static str {
        match self {
            CoreEvent::Progress { .. } => "progress",
            CoreEvent::AgentStep { .. } => "agent-step",
            CoreEvent::TimelineChanged { .. } => "timeline-changed",
            CoreEvent::MediaAssetsChanged { .. } => "media-assets-changed",
            CoreEvent::ProjectsChanged {} => "projects-changed",
            CoreEvent::TranscriptChanged { .. } => "transcript-changed",
            CoreEvent::McpReady { .. } => "mcp-ready",
            CoreEvent::ConfirmRequest { .. } => "confirm-request",
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
