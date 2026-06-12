// Model shapes are GENERATED from the Rust core (one source of truth):
//   TS_RS_EXPORT_DIR="$(pwd)/frontend/src/ipc/generated" \
//     cargo test -p roughcut-core --features ts-bindings export_bindings
// Event payloads and tool-result envelopes below follow docs/tool-api.md.

export type { Media } from "./generated/Media";
export type { Word } from "./generated/Word";
export type { TranscriptSegment } from "./generated/TranscriptSegment";
export type { Transcript } from "./generated/Transcript";
export type { Clip } from "./generated/Clip";
export type { ClipOrigin } from "./generated/ClipOrigin";
export type { Padding } from "./generated/Padding";
export type { Timeline } from "./generated/Timeline";
export type { EditOp } from "./generated/EditOp";
export type { EditAction } from "./generated/EditAction";
export type { ActionSource } from "./generated/ActionSource";
export type { Project } from "./generated/Project";
export type { ProjectSummary } from "./generated/ProjectSummary";
export type { Preferences } from "./generated/Preferences";
export type { Aggressiveness } from "./generated/Aggressiveness";
export type { ExternalConnection } from "./generated/ExternalConnection";
export type { MediaAssets } from "./generated/MediaAssets";
export type { Layout } from "./generated/Layout";
export type { ThumbnailRef } from "./generated/ThumbnailRef";

import type { Clip } from "./generated/Clip";
import type { EditAction } from "./generated/EditAction";
import type { ProjectSummary } from "./generated/ProjectSummary";
import type { Timeline } from "./generated/Timeline";

// ---------------------------------------------------------------------------
// Event payloads (docs/tool-api.md — "Events emitted by the core";
// Rust side: core/src/events.rs CoreEvent)
// ---------------------------------------------------------------------------

export type { ProgressTask } from "./generated/ProgressTask";
import type { ProgressTask } from "./generated/ProgressTask";

export interface ProgressEvent {
  task: ProgressTask;
  project_id?: string;
  fraction: number; // 0..1
  message: string;
}

export type AgentStepKind = "thinking" | "tool_call" | "tool_result" | "final";

export interface AgentStepEvent {
  project_id: string;
  step: number;
  kind: AgentStepKind;
  tool?: string;
  args?: Record<string, unknown>;
  result?: Record<string, unknown>;
  text?: string;
}

export interface TimelineChangedEvent { project_id: string }
export interface TranscriptChangedEvent { project_id: string }

export type AppEventName =
  | "progress" | "agent-step" | "timeline-changed" | "transcript-changed"
  | "projects-changed" | "media-assets-changed" | "confirm-request";

export interface ConfirmRequestEvent { id: string; summary: string }

// ---------------------------------------------------------------------------
// Common tool result envelopes
// ---------------------------------------------------------------------------

export type ExportTarget =
  | "premiere_xml" | "fcp_xml" | "resolve_xml" | "edl" | "otio" | "mp4" | "srt";

// Shell-command status shapes are GENERATED (same regime as the model
// types — ADR-0003 extended to the provisioning surface).
export type { ResolveStatus } from "./generated/ResolveStatus";
export type { RuntimeStatus } from "./generated/RuntimeStatus";
export type { SetupStatus } from "./generated/SetupStatus";
export type { TierStatus } from "./generated/TierStatus";
export type { SendToResolveOutcome } from "./generated/SendToResolveOutcome";

/** Whisper model tiers (NOT the generated ModelTier preference enum). */
export type WhisperTier = "accurate" | "compact";

// plan_duration_cut (core/src/engine/semantic.rs DurationCutPlan + the
// apply=true receipt from tools.rs)
export interface DurationCutPlan {
  segment_ids: string[];
  before_s: number;
  projected_after_s: number;
  target_s: number;
  method: "centrality" | "heuristic";
  notes: string;
}

export interface DurationCutReceipt {
  applied: number;
  action: EditAction;
  before_s: number;
  included_duration_s: number;
  target_s: number;
  method: string;
  notes: string;
}

export interface EditResult { action: EditAction; timeline: Timeline }
export interface SplitResult { clips: [Clip, Clip]; timeline: Timeline }
export interface UndoResult { action: EditAction | null; timeline: Timeline }
export interface RoughCutResult { timeline: Timeline; cut_count: number }
export interface InstructionResult { actions: EditAction[]; summary: string }
export interface ExportResult { path: string }
export interface ListProjectsResult { projects: ProjectSummary[]; trash: ProjectSummary[] }
export interface ToolError { code: string; message: string }
