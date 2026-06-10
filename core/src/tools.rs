//! The tool registry — KEYSTONE. One tool set, three callers: the local
//! agent loop, external MCP clients, and the UI.
//!
//! Each tool is ONE [`ToolSpec`] row: name, description, JSON schema,
//! agent-visibility, and handler live together, and everything else
//! (`all_defs`, `agent_defs`, dispatch) derives from the table. Adding a tool
//! is adding a row + a handler — nothing to keep in sync by hand.

use crate::agent;
use crate::detect;
use crate::engine::Editor;
use crate::error::{CoreError, Result};
use crate::export;
use crate::model::{ActionSource, Aggressiveness, EditOp, Preferences};
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use uuid::Uuid;

pub type HandlerFuture<'a> = Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;
pub type Handler = for<'a> fn(&'a Editor, &'a Value, ActionSource) -> HandlerFuture<'a>;

pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: fn() -> Value,
    /// Exposed to the LOCAL agent loop (meta tools and project lifecycle are not).
    pub agent: bool,
    /// Dispatches into the agent loop / external escalation — excluded from
    /// `dispatch_basic` so the loop can never recurse into itself.
    pub meta: bool,
    pub handler: Handler,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

pub fn registry() -> &'static [ToolSpec] {
    REGISTRY
}

pub fn all_defs() -> Vec<ToolDef> {
    REGISTRY
        .iter()
        .map(|s| ToolDef {
            name: s.name.into(),
            description: s.description.into(),
            input_schema: (s.schema)(),
        })
        .collect()
}

pub fn agent_defs() -> Vec<ToolDef> {
    REGISTRY
        .iter()
        .filter(|s| s.agent)
        .map(|s| ToolDef {
            name: s.name.into(),
            description: s.description.into(),
            input_schema: (s.schema)(),
        })
        .collect()
}

/// Full dispatch — UI and MCP callers come in here.
pub async fn dispatch(
    editor: &Editor,
    name: &str,
    args: &Value,
    source: ActionSource,
) -> Result<Value> {
    let spec = REGISTRY
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| CoreError::NotFound(format!("unknown tool {name}")))?;
    (spec.handler)(editor, args, source).await
}

/// Dispatch WITHOUT the meta tools — what the agent loop itself uses, which
/// makes recursion impossible by construction.
pub async fn dispatch_basic(
    editor: &Editor,
    name: &str,
    args: &Value,
    source: ActionSource,
) -> Result<Value> {
    let spec = REGISTRY
        .iter()
        .find(|s| s.name == name && !s.meta)
        .ok_or_else(|| CoreError::NotFound(format!("unknown tool {name}")))?;
    (spec.handler)(editor, args, source).await
}

// ----------------------------------------------------------- arg helpers

fn pid_schema() -> Value {
    json!({"type": "string", "description": "project id (uuid)"})
}

fn obj(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required })
}

fn arg_uuid(args: &Value, key: &str) -> Result<Uuid> {
    let s = args[key]
        .as_str()
        .ok_or_else(|| CoreError::InvalidArg(format!("missing {key}")))?;
    Uuid::parse_str(s).map_err(|_| CoreError::InvalidArg(format!("{key} is not a uuid: {s}")))
}

fn arg_uuid_opt(args: &Value, key: &str) -> Result<Option<Uuid>> {
    match args[key].as_str() {
        None | Some("") => Ok(None),
        Some(s) => Ok(Some(
            Uuid::parse_str(s).map_err(|_| CoreError::InvalidArg(format!("{key} is not a uuid")))?,
        )),
    }
}

fn arg_f64(args: &Value, key: &str) -> Result<f64> {
    args[key].as_f64().ok_or_else(|| CoreError::InvalidArg(format!("missing number {key}")))
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key].as_str().ok_or_else(|| CoreError::InvalidArg(format!("missing string {key}")))
}

fn arg_uuid_vec(args: &Value, key: &str) -> Result<Vec<Uuid>> {
    let arr = args[key]
        .as_array()
        .ok_or_else(|| CoreError::InvalidArg(format!("missing array {key}")))?;
    arr.iter()
        .map(|v| {
            v.as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| CoreError::InvalidArg(format!("{key} must contain uuids")))
        })
        .collect()
}

fn arg_aggressiveness(args: &Value) -> Option<Aggressiveness> {
    match args["aggressiveness"].as_str() {
        Some("aggressive") => Some(Aggressiveness::Aggressive),
        Some("natural") => Some(Aggressiveness::Natural),
        _ => None,
    }
}

/// LLM/MCP-friendly segment view: text + flags + times, no word arrays.
fn lean_segment(seg: &crate::model::TranscriptSegment) -> Value {
    json!({
        "id": seg.id,
        "start": seg.start,
        "end": seg.end,
        "text": seg.text,
        "is_filler": seg.is_filler,
        "is_silence": seg.is_silence,
        "take_group_id": seg.take_group_id,
        "is_best_take": seg.is_best_take,
    })
}

/// Apply an edit op and shape the standard `{ action, timeline }` result.
fn edit(editor: &Editor, project_id: Uuid, op: EditOp, source: ActionSource) -> Result<Value> {
    let outcome = editor.apply_edit(project_id, op, source)?;
    Ok(json!({ "action": outcome.action, "timeline": outcome.timeline }))
}

// -------------------------------------------------------------- handlers

macro_rules! handler {
    ($name:ident, |$editor:ident, $args:ident, $source:ident| $body:expr) => {
        fn $name<'a>(
            $editor: &'a Editor,
            $args: &'a Value,
            $source: ActionSource,
        ) -> HandlerFuture<'a> {
            let _ = &$source;
            Box::pin(async move { $body })
        }
    };
}

handler!(h_import_media, |e, a, _s| {
    let media = e.import_media(arg_uuid_opt(a, "project_id")?, arg_str(a, "file_path")?).await?;
    Ok(serde_json::to_value(media)?)
});

handler!(h_transcribe, |e, a, _s| {
    let t = e.transcribe(arg_uuid(a, "project_id")?, a["language"].as_str()).await?;
    Ok(serde_json::to_value(t)?)
});

handler!(h_detect_silences, |e, a, _s| {
    let project_id = arg_uuid(a, "project_id")?;
    let min = a["min_duration_s"].as_f64().unwrap_or(0.8);
    e.with_project(project_id, |p, t| {
        let t = t.ok_or_else(|| CoreError::InvalidArg("no transcript".into()))?;
        let ranges = detect::detect_silences(t, p.timeline.duration, min);
        Ok(json!({ "segments": ranges }))
    })
});

handler!(h_detect_fillers, |e, a, _s| {
    // Persists the flags — identical behavior to the rough-cut path.
    let (fillers, _) = e.annotate_transcript(arg_uuid(a, "project_id")?)?;
    Ok(json!({ "segment_ids": fillers.segment_ids, "word_ranges": fillers.word_ranges }))
});

handler!(h_detect_takes, |e, a, _s| {
    let (_, takes) = e.annotate_transcript(arg_uuid(a, "project_id")?)?;
    Ok(json!({ "take_groups": takes }))
});

handler!(h_generate_rough_cut, |e, a, s| {
    let (timeline, cut_count) = e
        .generate_rough_cut(arg_uuid(a, "project_id")?, arg_aggressiveness(a), s)
        .await?;
    Ok(json!({ "timeline": timeline, "cut_count": cut_count }))
});

handler!(h_cut_range, |e, a, s| {
    edit(
        e,
        arg_uuid(a, "project_id")?,
        EditOp::CutRange { start: arg_f64(a, "start")?, end: arg_f64(a, "end")? },
        s,
    )
});

handler!(h_restore_range, |e, a, s| {
    edit(
        e,
        arg_uuid(a, "project_id")?,
        EditOp::RestoreRange { start: arg_f64(a, "start")?, end: arg_f64(a, "end")? },
        s,
    )
});

handler!(h_cut_by_transcript, |e, a, s| {
    edit(
        e,
        arg_uuid(a, "project_id")?,
        EditOp::CutSegments { segment_ids: arg_uuid_vec(a, "segment_ids")? },
        s,
    )
});

handler!(h_restore_by_transcript, |e, a, s| {
    edit(
        e,
        arg_uuid(a, "project_id")?,
        EditOp::RestoreSegments { segment_ids: arg_uuid_vec(a, "segment_ids")? },
        s,
    )
});

handler!(h_trim_clip, |e, a, s| {
    edit(
        e,
        arg_uuid(a, "project_id")?,
        EditOp::TrimClip {
            clip_id: arg_uuid(a, "clip_id")?,
            new_source_in: arg_f64(a, "new_source_in")?,
            new_source_out: arg_f64(a, "new_source_out")?,
        },
        s,
    )
});

handler!(h_split_clip, |e, a, s| {
    let outcome = e.apply_edit(
        arg_uuid(a, "project_id")?,
        EditOp::SplitClip {
            clip_id: arg_uuid(a, "clip_id")?,
            at_time: arg_f64(a, "at_time")?,
        },
        s,
    )?;
    Ok(json!({ "clips": outcome.split_clips, "timeline": outcome.timeline, "action": outcome.action }))
});

handler!(h_reorder_clip, |e, a, s| {
    edit(
        e,
        arg_uuid(a, "project_id")?,
        EditOp::ReorderClip {
            clip_id: arg_uuid(a, "clip_id")?,
            new_order: a["new_order"].as_u64().unwrap_or(0) as u32,
        },
        s,
    )
});

handler!(h_set_global_padding, |e, a, s| {
    let start_s = arg_f64(a, "start_s")?;
    edit(
        e,
        arg_uuid(a, "project_id")?,
        EditOp::SetGlobalPadding {
            start_s,
            end_s: a["end_s"].as_f64().unwrap_or(start_s),
            linked: a["linked"].as_bool().unwrap_or(true),
        },
        s,
    )
});

handler!(h_read_transcript, |e, a, _s| {
    let project_id = arg_uuid(a, "project_id")?;
    let offset = a["offset"].as_u64().unwrap_or(0) as usize;
    let limit = (a["limit"].as_u64().unwrap_or(50) as usize).clamp(1, 200);
    let include_words = a["include_words"].as_bool().unwrap_or(false);
    e.with_project(project_id, |_, t| {
        let t = t.ok_or_else(|| CoreError::InvalidArg("no transcript yet".into()))?;
        let total = t.segments.len();
        let page: Vec<Value> = t
            .segments
            .iter()
            .skip(offset)
            .take(limit)
            .map(|seg| {
                if include_words {
                    serde_json::to_value(seg).unwrap_or(Value::Null)
                } else {
                    lean_segment(seg)
                }
            })
            .collect();
        Ok(json!({
            "total_segments": total,
            "offset": offset,
            "returned": page.len(),
            "language": t.language,
            "segments": page,
        }))
    })
});

handler!(h_find_segments, |e, a, _s| {
    let project_id = arg_uuid(a, "project_id")?;
    let query = arg_str(a, "query")?;
    // Semantic first (local embeddings index), keyword fallback.
    if let Some(hits) = e.semantic_find(project_id, query, 8).await? {
        let result = e.with_project(project_id, |_, t| {
            let t = t.ok_or_else(|| CoreError::InvalidArg("no transcript".into()))?;
            let segments: Vec<Value> = hits
                .iter()
                .filter_map(|(id, score)| {
                    t.segment(*id).map(|seg| {
                        let mut v = lean_segment(seg);
                        v["score"] = json!(score);
                        v
                    })
                })
                .collect();
            Ok(segments)
        })?;
        if !result.is_empty() {
            return Ok(json!({ "segments": result, "method": "semantic" }));
        }
    }
    e.with_project(project_id, |_, t| {
        let t = t.ok_or_else(|| CoreError::InvalidArg("no transcript".into()))?;
        let segments: Vec<Value> =
            detect::find_segments(t, query).iter().map(|seg| lean_segment(seg)).collect();
        Ok(json!({ "segments": segments, "method": "keyword" }))
    })
});

handler!(h_apply_instruction, |e, a, s| {
    let outcome = agent::run_instruction(e, arg_uuid(a, "project_id")?, arg_str(a, "instruction")?, s)
        .await?;
    Ok(serde_json::to_value(outcome)?)
});

handler!(h_generate_chapters, |e, a, _s| {
    let chapters = agent::generate_chapters(e, arg_uuid(a, "project_id")?).await?;
    Ok(json!({ "chapters": chapters }))
});

handler!(h_generate_title_description, |e, a, _s| {
    agent::generate_title_description(e, arg_uuid(a, "project_id")?).await
});

handler!(h_generate_captions, |e, a, _s| {
    let (project, transcript) = e.snapshot(arg_uuid(a, "project_id")?)?;
    let t = transcript.ok_or_else(|| CoreError::InvalidArg("no transcript".into()))?;
    Ok(json!({ "srt": export::srt::write(&project, &t) }))
});

handler!(h_export, |e, a, _s| {
    let path = e
        .export(arg_uuid(a, "project_id")?, arg_str(a, "target")?, arg_str(a, "out_path")?)
        .await?;
    Ok(json!({ "path": path }))
});

handler!(h_create_project, |e, a, _s| {
    let p = e.create_project(arg_str(a, "name")?, a["file_path"].as_str()).await?;
    Ok(serde_json::to_value(p)?)
});

handler!(h_delete_project, |e, a, _s| {
    e.delete_project(arg_uuid(a, "project_id")?)?;
    Ok(json!({ "deleted": true }))
});

handler!(h_open_project, |e, a, _s| {
    Ok(serde_json::to_value(e.open_project(arg_uuid(a, "project_id")?)?)?)
});

handler!(h_save_project, |e, a, _s| {
    Ok(serde_json::to_value(e.save_project(arg_uuid(a, "project_id")?)?)?)
});

handler!(h_list_projects, |e, _a, _s| Ok(json!({ "projects": e.list_projects()? })));

handler!(h_get_media_assets, |e, a, _s| {
    Ok(serde_json::to_value(e.media_assets(arg_uuid(a, "project_id")?).await?)?)
});

handler!(h_get_timeline, |e, a, _s| {
    Ok(serde_json::to_value(e.get_timeline(arg_uuid(a, "project_id")?)?)?)
});

handler!(h_get_transcript, |e, a, _s| {
    Ok(serde_json::to_value(e.get_transcript(arg_uuid(a, "project_id")?)?)?)
});

handler!(h_undo, |e, a, _s| {
    let (action, timeline) = e.undo(arg_uuid(a, "project_id")?)?;
    Ok(json!({ "action": action, "timeline": timeline }))
});

handler!(h_redo, |e, a, _s| {
    let (action, timeline) = e.redo(arg_uuid(a, "project_id")?)?;
    Ok(json!({ "action": action, "timeline": timeline }))
});

handler!(h_get_preferences, |e, _a, _s| Ok(serde_json::to_value(e.get_preferences()?)?));

handler!(h_set_preferences, |e, a, _s| {
    let prefs: Preferences = serde_json::from_value(a["preferences"].clone())?;
    Ok(serde_json::to_value(e.set_preferences(prefs)?)?)
});

handler!(h_connect_external, |e, a, _s| {
    let conn =
        crate::mcp::client::connect_external(e, arg_str(a, "provider")?, arg_str(a, "api_key")?)?;
    Ok(serde_json::to_value(conn)?)
});

handler!(h_escalate_to_frontier, |e, a, _s| {
    let outcome = crate::mcp::client::escalate_to_frontier(
        e,
        arg_uuid(a, "project_id")?,
        arg_str(a, "instruction")?,
        arg_str(a, "connection_id")?,
    )
    .await?;
    Ok(serde_json::to_value(outcome)?)
});

// ---------------------------------------------------------- the registry

macro_rules! tool {
    ($name:literal, $desc:literal, $schema:expr, agent: $agent:literal, meta: $meta:literal, $handler:ident) => {
        ToolSpec {
            name: $name,
            description: $desc,
            schema: $schema,
            agent: $agent,
            meta: $meta,
            handler: $handler,
        }
    };
}

static REGISTRY: &[ToolSpec] = &[
    tool!("import_media", "Probe and attach a local video file. Returns media metadata.",
        || obj(json!({"project_id": pid_schema(), "file_path": {"type": "string"}}), &["file_path"]),
        agent: false, meta: false, h_import_media),
    tool!("transcribe", "Transcribe the project's media on-device into time-aligned text.",
        || obj(json!({"project_id": pid_schema(), "language": {"type": "string"}}), &["project_id"]),
        agent: false, meta: false, h_transcribe),
    tool!("detect_silences", "Find dead-air ranges in the transcript/audio.",
        || obj(json!({"project_id": pid_schema(), "min_duration_s": {"type": "number"}}), &["project_id"]),
        agent: false, meta: false, h_detect_silences),
    tool!("detect_fillers", "Flag filler words (um, uh, ...) in the transcript. Persists the flags.",
        || obj(json!({"project_id": pid_schema()}), &["project_id"]),
        agent: false, meta: false, h_detect_fillers),
    tool!("detect_takes", "Group repeated takes of the same line and mark the best one. Persists the flags.",
        || obj(json!({"project_id": pid_schema()}), &["project_id"]),
        agent: false, meta: false, h_detect_takes),
    tool!("generate_rough_cut", "Run the full AI first pass: remove silences, fillers, and non-best takes. Returns the new timeline and cut count.",
        || obj(json!({"project_id": pid_schema(), "aggressiveness": {"type": "string", "enum": ["natural", "aggressive"]}}), &["project_id"]),
        agent: true, meta: false, h_generate_rough_cut),
    tool!("cut_range", "Exclude a source time range [start, end] (seconds) from the cut. Non-destructive.",
        || obj(json!({"project_id": pid_schema(), "start": {"type": "number"}, "end": {"type": "number"}}), &["project_id", "start", "end"]),
        agent: true, meta: false, h_cut_range),
    tool!("restore_range", "Re-include a previously cut source time range.",
        || obj(json!({"project_id": pid_schema(), "start": {"type": "number"}, "end": {"type": "number"}}), &["project_id", "start", "end"]),
        agent: true, meta: false, h_restore_range),
    tool!("cut_by_transcript", "Cut the video ranges covered by the given transcript segment ids.",
        || obj(json!({"project_id": pid_schema(), "segment_ids": {"type": "array", "items": {"type": "string"}}}), &["project_id", "segment_ids"]),
        agent: true, meta: false, h_cut_by_transcript),
    tool!("restore_by_transcript", "Restore the video ranges covered by the given transcript segment ids.",
        || obj(json!({"project_id": pid_schema(), "segment_ids": {"type": "array", "items": {"type": "string"}}}), &["project_id", "segment_ids"]),
        agent: true, meta: false, h_restore_by_transcript),
    tool!("trim_clip", "Move a clip's boundaries (drag-handle trim), frame-exact.",
        || obj(json!({"project_id": pid_schema(), "clip_id": {"type": "string"}, "new_source_in": {"type": "number"}, "new_source_out": {"type": "number"}}), &["project_id", "clip_id", "new_source_in", "new_source_out"]),
        agent: true, meta: false, h_trim_clip),
    tool!("split_clip", "Split a clip at a source time (playhead).",
        || obj(json!({"project_id": pid_schema(), "clip_id": {"type": "string"}, "at_time": {"type": "number"}}), &["project_id", "clip_id", "at_time"]),
        agent: true, meta: false, h_split_clip),
    tool!("reorder_clip", "Move a clip to a new position in the timeline.",
        || obj(json!({"project_id": pid_schema(), "clip_id": {"type": "string"}, "new_order": {"type": "integer"}}), &["project_id", "clip_id", "new_order"]),
        agent: false, meta: false, h_reorder_clip),
    tool!("set_global_padding", "Apply breathing room (seconds) to the start/end of all talking clips at once.",
        || obj(json!({"project_id": pid_schema(), "start_s": {"type": "number"}, "end_s": {"type": "number"}, "linked": {"type": "boolean"}}), &["project_id", "start_s", "end_s"]),
        agent: true, meta: false, h_set_global_padding),
    tool!("find_segments", "Semantic search over the transcript with a natural-language query (local embeddings; keyword fallback). Returns matching segments with ids, times, and scores — use this instead of reading the whole transcript.",
        || obj(json!({"project_id": pid_schema(), "query": {"type": "string"}}), &["project_id", "query"]),
        agent: true, meta: false, h_find_segments),
    tool!("read_transcript", "Read the transcript in pages: lean segments (text, times, flags; no word arrays unless include_words). Use offset/limit for long videos instead of get_transcript.",
        || obj(json!({"project_id": pid_schema(), "offset": {"type": "integer"}, "limit": {"type": "integer", "description": "max 200, default 50"}, "include_words": {"type": "boolean"}}), &["project_id"]),
        agent: true, meta: false, h_read_transcript),
    tool!("apply_instruction", "High-level natural-language edit; the local model expands it into tool calls.",
        || obj(json!({"project_id": pid_schema(), "instruction": {"type": "string"}}), &["project_id", "instruction"]),
        agent: false, meta: true, h_apply_instruction),
    tool!("generate_chapters", "Generate YouTube-style chapters from the transcript.",
        || obj(json!({"project_id": pid_schema()}), &["project_id"]),
        agent: true, meta: false, h_generate_chapters),
    tool!("generate_title_description", "Suggest video titles and a description from the content.",
        || obj(json!({"project_id": pid_schema()}), &["project_id"]),
        agent: false, meta: false, h_generate_title_description),
    tool!("generate_captions", "Produce SRT captions for the current cut.",
        || obj(json!({"project_id": pid_schema()}), &["project_id"]),
        agent: false, meta: false, h_generate_captions),
    tool!("export", "Export the current cut. target: premiere_xml | fcp_xml | resolve_xml | edl | otio | mp4 | srt.",
        || obj(json!({"project_id": pid_schema(), "target": {"type": "string"}, "out_path": {"type": "string"}}), &["project_id", "target", "out_path"]),
        agent: false, meta: false, h_export),
    tool!("create_project", "Create a project, optionally importing a media file immediately.",
        || obj(json!({"name": {"type": "string"}, "file_path": {"type": "string"}}), &["name"]),
        agent: false, meta: false, h_create_project),
    tool!("delete_project", "Permanently delete a project's edit state (timeline, transcript, undo history). The source video file on disk is never touched.",
        || obj(json!({"project_id": pid_schema()}), &["project_id"]),
        agent: false, meta: false, h_delete_project),
    tool!("open_project", "Open an existing project.",
        || obj(json!({"project_id": pid_schema()}), &["project_id"]),
        agent: false, meta: false, h_open_project),
    tool!("save_project", "Persist a project.",
        || obj(json!({"project_id": pid_schema()}), &["project_id"]),
        agent: false, meta: false, h_save_project),
    tool!("list_projects", "List saved projects.", || obj(json!({}), &[]),
        agent: false, meta: false, h_list_projects),
    tool!("get_media_assets", "Waveform peaks + thumbnail filmstrip file paths for the project's media (timeline view data; generated and cached on first call).",
        || obj(json!({"project_id": pid_schema()}), &["project_id"]),
        agent: false, meta: false, h_get_media_assets),
    tool!("get_timeline", "Get the current timeline (clips, padding, cut count).",
        || obj(json!({"project_id": pid_schema()}), &["project_id"]),
        agent: true, meta: false, h_get_timeline),
    tool!("get_transcript", "Get the FULL transcript including word-level timestamps — large for long videos; prefer read_transcript (paged) or find_segments (semantic search).",
        || obj(json!({"project_id": pid_schema()}), &["project_id"]),
        agent: false, meta: false, h_get_transcript),
    tool!("undo", "Undo the last edit.", || obj(json!({"project_id": pid_schema()}), &["project_id"]),
        agent: false, meta: false, h_undo),
    tool!("redo", "Redo the last undone edit.", || obj(json!({"project_id": pid_schema()}), &["project_id"]),
        agent: false, meta: false, h_redo),
    tool!("get_preferences", "Get user preferences.", || obj(json!({}), &[]),
        agent: false, meta: false, h_get_preferences),
    tool!("set_preferences", "Replace user preferences.",
        || obj(json!({"preferences": {"type": "object"}}), &["preferences"]),
        agent: false, meta: false, h_set_preferences),
    tool!("connect_external", "Opt in to an external frontier provider (stores key in OS keychain).",
        || obj(json!({"provider": {"type": "string"}, "api_key": {"type": "string"}}), &["provider", "api_key"]),
        agent: false, meta: true, h_connect_external),
    tool!("escalate_to_frontier", "Send an instruction to the connected frontier model. Explicit opt-in only; never auto-invoked.",
        || obj(json!({"project_id": pid_schema(), "instruction": {"type": "string"}, "connection_id": {"type": "string"}}), &["project_id", "instruction", "connection_id"]),
        agent: false, meta: true, h_escalate_to_frontier),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_invariants() {
        let mut names = std::collections::HashSet::new();
        for spec in REGISTRY {
            assert!(names.insert(spec.name), "duplicate tool name {}", spec.name);
            let schema = (spec.schema)();
            assert_eq!(schema["type"], "object", "{} schema must be an object", spec.name);
            assert!(!spec.description.is_empty(), "{} needs a description", spec.name);
            assert!(!(spec.agent && spec.meta), "{} cannot be both agent-visible and meta", spec.name);
        }
        // The agent subset is exactly the flagged rows.
        assert_eq!(agent_defs().len(), REGISTRY.iter().filter(|s| s.agent).count());
    }
}
