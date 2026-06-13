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
    /// Lands undoable EditActions (drives the agent loop's act/read
    /// accounting — the registry owns this, not a parallel hand list).
    pub mutating: bool,
    pub handler: Handler,
}

/// Registry lookup for the agent loop's act/read accounting.
pub fn is_mutating(name: &str) -> bool {
    REGISTRY.iter().any(|s| s.name == name && s.mutating)
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

/// What external MCP clients see: everything EXCEPT the orchestration
/// meta-tools — an external frontier model IS an orchestrator; handing it
/// apply_instruction would route editing through the weaker on-device model
/// (and long sync calls time out MCP clients).
pub fn mcp_defs() -> Vec<ToolDef> {
    REGISTRY
        .iter()
        .filter(|s| !s.meta)
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
        .ok_or_else(|| {
            let known: Vec<&str> = REGISTRY.iter().filter(|s| !s.meta).map(|s| s.name).collect();
            CoreError::NotFound(format!("unknown tool {name}; available: {}", known.join(", ")))
        })?;
    validate_args(spec, args)?;
    (spec.handler)(editor, args, source).await
}

/// Required-fields + primitive-type check against the declared schema —
/// "required" was advertised but never enforced, and small models repair
/// best from errors that echo expected vs got (research: strict validators
/// matter more than model size).
fn validate_args(spec: &ToolSpec, args: &Value) -> Result<()> {
    let schema = (spec.schema)();
    let Some(props) = schema["properties"].as_object() else { return Ok(()) };
    if !args.is_object() {
        return Err(CoreError::InvalidArg(format!(
            "{}: arguments must be a JSON object, got {}",
            spec.name,
            json_kind(args)
        )));
    }
    let mut problems: Vec<String> = vec![];
    if let Some(required) = schema["required"].as_array() {
        for r in required.iter().filter_map(|v| v.as_str()) {
            if args.get(r).map(|v| v.is_null()).unwrap_or(true) {
                let expected = props
                    .get(r)
                    .and_then(|p| p["type"].as_str())
                    .unwrap_or("value");
                problems.push(format!("missing required '{r}' ({expected})"));
            }
        }
    }
    for (key, value) in args.as_object().unwrap() {
        let Some(prop) = props.get(key) else { continue }; // extra keys: ignore
        let Some(expected) = prop["type"].as_str() else { continue };
        if value.is_null() {
            continue;
        }
        let ok = match expected {
            "string" => value.is_string(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "array" => value.is_array(),
            "object" => value.is_object(),
            _ => true,
        };
        if !ok {
            problems.push(format!(
                "'{key}' must be a {expected}, got {} ({value})",
                json_kind(value)
            ));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(CoreError::InvalidArg(format!(
            "{}: {}. Schema: {}",
            spec.name,
            problems.join("; "),
            serde_json::to_string(&schema).unwrap_or_default()
        )))
    }
}

fn json_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
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
    args[key].as_f64().ok_or_else(|| {
        CoreError::InvalidArg(format!(
            "'{key}' must be a number, got {}",
            json_kind(&args[key])
        ))
    })
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key].as_str().ok_or_else(|| {
        CoreError::InvalidArg(format!(
            "'{key}' must be a string, got {}",
            json_kind(&args[key])
        ))
    })
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
    let (transcript, duration, media) = e.with_project(project_id, |p, t| {
        let t = t.ok_or_else(|| CoreError::InvalidArg("no transcript".into()))?;
        Ok((t.clone(), p.timeline.duration, p.media.clone()))
    })?;
    // Audio pass OUTSIDE the state lock (decode can take a moment).
    let peaks = match &media {
        Some(m) => crate::adapters::video::load_raw_peaks(m).await,
        None => None,
    };
    let candidates = detect::silence_candidates(&transcript, duration, (min * 0.6).max(0.3));
    let refined = detect::refine_silences(
        &candidates,
        peaks.as_deref(),
        crate::adapters::video::PEAKS_PER_SECOND,
        min,
    );
    let method = if peaks.is_some() { "hybrid" } else { "transcript" };
    Ok(json!({ "method": method, "segments": refined }))
});

handler!(h_remove_silences, |e, a, s| {
    // Pure dead-air removal: audio-confirmed gaps only, via the SHARED
    // helper. Touches no words, fillers, takes, or order — the whole point
    // versus generate_rough_cut.
    let project_id = arg_uuid(a, "project_id")?;
    let actions = e.cut_confirmed_silences(project_id, s).await?;
    let tl = e.get_timeline(project_id)?;
    let note = if actions.is_empty() {
        "no audio-confirmed dead air found (or no audio to check against) — nothing removed"
    } else {
        "removed only waveform-confirmed silence; words, takes, and order untouched"
    };
    Ok(json!({
        "applied": actions.len(),
        "actions": actions,
        "note": note,
        "cut_count": tl.cut_count,
        "included_duration_s": (tl.included_duration() * 10.0).round() / 10.0,
        "source_duration_s": tl.duration,
    }))
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
    // `action` included so orchestrators (and the agent's undo accounting)
    // see the rough cut as the edit it is. Tier 1 (silence + fillers) is
    // applied; `suggestions` counts the Tier-2 flags now awaiting review.
    let (action, timeline, cut_count, suggestion_count) = e
        .generate_rough_cut(arg_uuid(a, "project_id")?, arg_aggressiveness(a), s)
        .await?;
    Ok(json!({
        "action": action,
        "timeline": timeline,
        "cut_count": cut_count,
        "suggestions": suggestion_count,
    }))
});

handler!(h_get_suggestions, |e, a, _s| {
    Ok(json!({ "suggestions": e.get_suggestions(arg_uuid(a, "project_id")?)? }))
});

handler!(h_accept_suggestion, |e, a, s| {
    let action = e.accept_suggestion(
        arg_uuid(a, "project_id")?,
        arg_uuid(a, "suggestion_id")?,
        s,
    )?;
    Ok(json!({ "action": action }))
});

handler!(h_accept_all_suggestions, |e, a, s| {
    let (action, accepted) = e.accept_all_suggestions(arg_uuid(a, "project_id")?, s)?;
    Ok(json!({ "action": action, "accepted": accepted }))
});

handler!(h_dismiss_suggestion, |e, a, _s| {
    e.dismiss_suggestion(arg_uuid(a, "project_id")?, arg_uuid(a, "suggestion_id")?)?;
    Ok(json!({ "dismissed": true }))
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

handler!(h_attach_screen_media, |e, a, _s| {
    e.attach_screen_media(arg_uuid(a, "project_id")?, arg_str(a, "file_path")?).await?;
    Ok(serde_json::json!({ "attached": true }))
});

handler!(h_set_layout, |e, a, _s| {
    let pid = arg_uuid(a, "project_id")?;
    let mut layout = e.open_project(pid)?.layout;
    if let Some(v) = a["mode"].as_str() {
        if !["pip", "camera", "screen"].contains(&v) {
            return Err(CoreError::InvalidArg("mode: pip | camera | screen".into()));
        }
        layout.mode = v.into();
    }
    if let Some(v) = a["shape"].as_str() {
        if !["rounded", "round"].contains(&v) {
            return Err(CoreError::InvalidArg("shape: rounded | round".into()));
        }
        layout.shape = v.into();
    }
    if let Some(v) = a["corner"].as_str() {
        if !["tl", "tr", "bl", "br"].contains(&v) {
            return Err(CoreError::InvalidArg("corner: tl | tr | bl | br".into()));
        }
        layout.corner = v.into();
    }
    if let Some(v) = a["size"].as_f64() {
        layout.size = v.clamp(0.10, 0.45);
    }
    e.set_layout(pid, layout.clone())?;
    Ok(serde_json::to_value(layout)?)
});

handler!(h_set_audio_offset, |e, a, _s| {
    let pid = arg_uuid(a, "project_id")?;
    let offset = a["offset_s"].as_f64().unwrap_or(0.0).clamp(-2.0, 2.0);
    e.set_audio_offset(pid, offset)?;
    Ok(serde_json::json!({ "audio_offset_s": offset }))
});

handler!(h_append_media, |e, a, _s| {
    let media = e.append_media(arg_uuid(a, "project_id")?, arg_str(a, "file_path")?).await?;
    Ok(serde_json::json!({ "media": media, "note": "re-run transcribe to caption the appended span; existing cuts kept their timestamps" }))
});

handler!(h_outline_transcript, |e, a, _s| {
    let outline = crate::story::outline_with(
        e,
        arg_uuid(a, "project_id")?,
        a["refresh"].as_bool().unwrap_or(false),
    )
    .await?;
    Ok(crate::story::outline_to_value(&outline))
});

handler!(h_review_flow, |e, a, _s| {
    let review = crate::story::review_flow(e, arg_uuid(a, "project_id")?).await?;
    Ok(serde_json::to_value(review)?)
});

handler!(h_story_edit, |e, a, s| {
    let outcome = crate::story::story_edit(
        e,
        arg_uuid(a, "project_id")?,
        arg_str(a, "instruction")?,
        match &a["target_duration_s"] {
            Value::Null => None,
            v => Some(v.as_f64().ok_or_else(|| {
                CoreError::InvalidArg(format!(
                    "'target_duration_s' must be a number (seconds), got {}",
                    json_kind(v)
                ))
            })?),
        },
        s,
    )
    .await?;
    Ok(serde_json::to_value(outcome)?)
});

handler!(h_plan_duration_cut, |e, a, s| {
    let project_id = arg_uuid(a, "project_id")?;
    let plan = e.plan_duration_cut(
        project_id,
        a["target_duration_s"]
            .as_f64()
            .ok_or_else(|| CoreError::InvalidArg("target_duration_s (seconds) required".into()))?,
    )?;
    // apply: plan AND cut in one step — one undoable action, small receipt.
    // Crucial for small local models: a 400-id plan must never round-trip
    // through the model (it WILL get truncated or mangled).
    if a["apply"].as_bool().unwrap_or(false) && !plan.segment_ids.is_empty() {
        let outcome = e.apply_edit(
            project_id,
            EditOp::CutSegments { segment_ids: plan.segment_ids.clone() },
            s,
        )?;
        return Ok(json!({
            "applied": plan.segment_ids.len(),
            "action": outcome.action,
            "before_s": plan.before_s,
            "included_duration_s": outcome.timeline.included_duration(),
            "target_s": plan.target_s,
            "method": plan.method,
            "notes": plan.notes,
        }));
    }
    Ok(serde_json::to_value(plan)?)
});

handler!(h_find_segments, |e, a, _s| {
    let project_id = arg_uuid(a, "project_id")?;
    let query = arg_str(a, "query")?;
    let limit = (a["limit"].as_u64().unwrap_or(8) as usize).clamp(1, 50);
    // Hybrid retrieval: BM25 (lexical) fused with embedding cosine (semantic)
    // by reciprocal-rank fusion; BM25 alone when no index exists.
    let semantic = e.semantic_find(project_id, query, limit * 2).await?;
    e.with_project(project_id, |_, t| {
        let t = t.ok_or_else(|| CoreError::InvalidArg("no transcript".into()))?;
        let bm25 = detect::bm25_rank(t, query, limit * 2);
        let mut rrf: std::collections::HashMap<Uuid, f64> = std::collections::HashMap::new();
        for (rank, (seg, _)) in bm25.iter().enumerate() {
            *rrf.entry(seg.id).or_default() += 1.0 / (60.0 + rank as f64);
        }
        let method = if let Some(hits) = &semantic {
            for (rank, (id, _)) in hits.iter().enumerate() {
                *rrf.entry(*id).or_default() += 1.0 / (60.0 + rank as f64);
            }
            "hybrid"
        } else {
            "bm25"
        };
        let mut fused: Vec<(Uuid, f64)> = rrf.into_iter().collect();
        fused.sort_by(|x, y| y.1.partial_cmp(&x.1).unwrap_or(std::cmp::Ordering::Equal));
        let segments: Vec<Value> = fused
            .iter()
            .take(limit)
            .filter_map(|(id, score)| {
                t.segment(*id).map(|seg| {
                    let mut v = lean_segment(seg);
                    v["score"] = json!(score);
                    v
                })
            })
            .collect();
        Ok(json!({ "segments": segments, "method": method }))
    })
});

handler!(h_apply_edits, |e, a, s| {
    let project_id = arg_uuid(a, "project_id")?;
    let ops: Vec<EditOp> = serde_json::from_value(a["edits"].clone())
        .map_err(|err| CoreError::InvalidArg(format!(
            "edits: {err}. Each edit is {{\"type\": one of cut_range|restore_range|cut_segments|\
             restore_segments|trim_clip|split_clip|reorder_clip|set_global_padding, ...fields}}"
        )))?;
    if ops.is_empty() {
        return Err(CoreError::InvalidArg("edits must be a non-empty array".into()));
    }
    if ops.len() > 100 {
        return Err(CoreError::InvalidArg("at most 100 edits per call".into()));
    }
    // Journal-internal variants are NOT public edits: a hallucinated
    // set_clips would wholesale-replace (wipe) the timeline, and rough_cut
    // through this path skips peak staging — both have real tools.
    for (i, op) in ops.iter().enumerate() {
        if matches!(op, EditOp::SetClips { .. } | EditOp::RoughCut { .. }) {
            return Err(CoreError::InvalidArg(format!(
                "edit {i}: type '{}' is not allowed in apply_edits (use generate_rough_cut \
                 for rough cuts; set_clips is journal-internal)",
                op.kind()
            )));
        }
    }
    let mut actions = vec![];
    let mut last: Option<crate::model::Timeline> = None;
    let total = ops.len();
    for (i, op) in ops.into_iter().enumerate() {
        match e.apply_edit(project_id, op, s) {
            Ok(outcome) => {
                actions.push(outcome.action);
                last = Some(outcome.timeline);
            }
            Err(err) => {
                // PARTIAL RECEIPT, never a bare error: edits 0..i landed and
                // are journaled — an Err would tell the model (and MCP
                // clients) "nothing happened" while the timeline changed,
                // and its natural retry would re-apply the prefix.
                let tl = e.get_timeline(project_id)?;
                return Ok(json!({
                    "applied": actions.len(),
                    "requested": total,
                    "actions": actions,
                    "failed_at": i,
                    "error": { "code": "edit_failed", "message": err.to_string() },
                    "note": format!(
                        "edits 0..{} were APPLIED and are undoable; edit {i} failed — \
                         fix it and resend ONLY the remaining edits",
                        actions.len()
                    ),
                    "cut_count": tl.cut_count,
                    "included_duration_s": tl.included_duration(),
                    "source_duration_s": tl.duration,
                }));
            }
        }
    }
    // Lean receipt: no clip dumps. Use get_timeline if you need detail.
    let tl = last.unwrap();
    Ok(json!({
        "applied": actions.len(),
        "requested": total,
        "actions": actions,
        "cut_count": tl.cut_count,
        "included_duration_s": tl.included_duration(),
        "source_duration_s": tl.duration,
    }))
});

handler!(h_apply_instruction, |e, a, s| {
    // Optional `history`: [{role: "user"|"agent", text}] — recent chat turns
    // so follow-ups ("apply the edits", "do it") resolve against them.
    let history: Vec<agent::HistoryTurn> =
        serde_json::from_value(a.get("history").cloned().unwrap_or(json!([]))).unwrap_or_default();
    let outcome = agent::run_instruction(
        e,
        arg_uuid(a, "project_id")?,
        arg_str(a, "instruction")?,
        &history,
        s,
    )
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

handler!(h_export, |e, a, s| {
    let target = arg_str(a, "target")?.to_string();
    let out_path = arg_str(a, "out_path")?.to_string();
    if s == ActionSource::McpClient
        && !e
            .request_confirmation(&format!("Export {target} to {out_path}? (requested by an external MCP client)"))
            .await
    {
        return Err(CoreError::Unavailable(
            "export denied: the user did not approve this externally-requested write".into(),
        ));
    }
    let path = e.export(arg_uuid(a, "project_id")?, &target, &out_path).await?;
    Ok(json!({ "path": path }))
});

handler!(h_create_project, |e, a, _s| {
    let p = e.create_project(arg_str(a, "name")?, a["file_path"].as_str()).await?;
    Ok(serde_json::to_value(p)?)
});

handler!(h_duplicate_project, |e, a, _s| {
    let p = e.duplicate_project(arg_uuid(a, "project_id")?, arg_str(a, "name")?)?;
    Ok(serde_json::to_value(p)?)
});

handler!(h_delete_project, |e, a, s| {
    let project_id = arg_uuid(a, "project_id")?;
    if s == ActionSource::McpClient {
        let name = e
            .with_project(project_id, |p, _| Ok(p.name.clone()))
            .unwrap_or_else(|_| project_id.to_string());
        if !e
            .request_confirmation(&format!("Delete project “{name}”? (requested by an external MCP client; the video file is never touched)"))
            .await
        {
            return Err(CoreError::Unavailable(
                "delete denied: the user did not approve this externally-requested deletion".into(),
            ));
        }
    }
    e.delete_project(project_id)?;
    Ok(json!({ "deleted": true }))
});

handler!(h_open_project, |e, a, _s| {
    Ok(serde_json::to_value(e.open_project(arg_uuid(a, "project_id")?)?)?)
});

handler!(h_save_project, |e, a, _s| {
    Ok(serde_json::to_value(e.save_project(arg_uuid(a, "project_id")?)?)?)
});

handler!(h_list_projects, |e, _a, _s| {
    let (live, trash): (Vec<_>, Vec<_>) =
        e.list_projects()?.into_iter().partition(|p| p.deleted_at.is_none());
    Ok(json!({ "projects": live, "trash": trash }))
});

handler!(h_restore_project, |e, a, _s| {
    Ok(serde_json::to_value(e.restore_project(arg_uuid(a, "project_id")?)?)?)
});

handler!(h_get_media_assets, |e, a, _s| {
    Ok(serde_json::to_value(e.media_assets(arg_uuid(a, "project_id")?).await?)?)
});

handler!(h_get_timeline, |e, a, _s| {
    Ok(serde_json::to_value(e.get_timeline(arg_uuid(a, "project_id")?)?)?)
});

handler!(h_get_transcript, |e, a, _s| {
    Ok(serde_json::to_value(e.get_transcript(arg_uuid(a, "project_id")?)?)?)
});

handler!(h_undo_actions, |e, a, _s| {
    let project_id = arg_uuid(a, "project_id")?;
    let ids = arg_uuid_vec(a, "action_ids")?;
    let (undone, timeline) = e.undo_actions(project_id, &ids)?;
    Ok(json!({ "undone": undone, "timeline": { "cut_count": timeline.cut_count, "included_duration_s": timeline.included_duration() } }))
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
        tool!($name, $desc, $schema, agent: $agent, meta: $meta, mutating: false, $handler)
    };
    ($name:literal, $desc:literal, $schema:expr, agent: $agent:literal, meta: $meta:literal, mutating: $mutating:literal, $handler:ident) => {
        ToolSpec {
            name: $name,
            description: $desc,
            schema: $schema,
            agent: $agent,
            meta: $meta,
            mutating: $mutating,
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
    tool!("detect_silences", "PREVIEW dead-air ranges (read-only): word-gap candidates confirmed and boundary-snapped against actual audio energy (per-file adaptive thresholds). Each range carries source: confirmed | transcript_only. To actually remove them, use remove_silences.",
        || obj(json!({"project_id": pid_schema(), "min_duration_s": {"type": "number"}}), &["project_id"]),
        agent: true, meta: false, h_detect_silences),
    tool!("remove_silences", "Remove ONLY dead air: deletes non-speech gaps confirmed by the actual audio waveform, in one undoable batch. Changes NOTHING about the words, their order, or which topics are kept — never touches fillers or takes. This is the silence-only tool; for filler/take removal use generate_rough_cut, for narrative restructuring use story_edit.",
        || obj(json!({"project_id": pid_schema(), "min_duration_s": {"type": "number"}}), &["project_id"]),
        agent: true, meta: false, mutating: true, h_remove_silences),
    tool!("detect_fillers", "Flag filler words (um, uh, ...) in the transcript. Persists the flags.",
        || obj(json!({"project_id": pid_schema()}), &["project_id"]),
        agent: false, meta: false, h_detect_fillers),
    tool!("detect_takes", "Group repeated takes of the same line and mark the best one. Persists the flags.",
        || obj(json!({"project_id": pid_schema()}), &["project_id"]),
        agent: false, meta: false, h_detect_takes),
    tool!("get_suggestions", "List the rough cut's pending Tier-2 SUGGESTIONS (repeated takes + repetitive passages) — flagged for review, not yet applied. Each: {id, segment_ids, start, end, reason, confidence, duplicate_of, preview}.",
        || obj(json!({"project_id": pid_schema()}), &["project_id"]),
        agent: true, meta: false, h_get_suggestions),
    tool!("accept_suggestion", "Apply ONE rough-cut suggestion (by id from get_suggestions) as a normal undoable cut, then drop it from the list.",
        || obj(json!({"project_id": pid_schema(), "suggestion_id": {"type": "string"}}), &["project_id", "suggestion_id"]),
        agent: true, meta: false, mutating: true, h_accept_suggestion),
    tool!("accept_all_suggestions", "Apply ALL pending rough-cut suggestions in one undoable batch (the 'accept all' action).",
        || obj(json!({"project_id": pid_schema()}), &["project_id"]),
        agent: true, meta: false, mutating: true, h_accept_all_suggestions),
    tool!("dismiss_suggestion", "Discard one rough-cut suggestion (by id from get_suggestions) without cutting.",
        || obj(json!({"project_id": pid_schema(), "suggestion_id": {"type": "string"}}), &["project_id", "suggestion_id"]),
        agent: true, meta: false, h_dismiss_suggestion),
    tool!("generate_rough_cut", "Full AI first pass: auto-cuts silences AND fillers (Tier 1), and FLAGS repeated takes + repetitive passages as suggestions for review (Tier 2 — see get_suggestions/accept_all_suggestions). Deterministic, no narrative restructuring. For silence ONLY use remove_silences. Returns timeline, cut_count, and suggestions (count).",
        || obj(json!({"project_id": pid_schema(), "aggressiveness": {"type": "string", "enum": ["natural", "aggressive"]}}), &["project_id"]),
        agent: true, meta: false, mutating: true, h_generate_rough_cut),
    tool!("cut_range", "Exclude a source time range [start, end] (seconds) from the cut. Non-destructive.",
        || obj(json!({"project_id": pid_schema(), "start": {"type": "number"}, "end": {"type": "number"}}), &["project_id", "start", "end"]),
        agent: true, meta: false, mutating: true, h_cut_range),
    tool!("restore_range", "Re-include a previously cut source time range.",
        || obj(json!({"project_id": pid_schema(), "start": {"type": "number"}, "end": {"type": "number"}}), &["project_id", "start", "end"]),
        agent: true, meta: false, mutating: true, h_restore_range),
    tool!("cut_by_transcript", "Cut the video ranges covered by the given transcript segment ids.",
        || obj(json!({"project_id": pid_schema(), "segment_ids": {"type": "array", "items": {"type": "string"}}}), &["project_id", "segment_ids"]),
        agent: true, meta: false, mutating: true, h_cut_by_transcript),
    tool!("restore_by_transcript", "Restore the video ranges covered by the given transcript segment ids.",
        || obj(json!({"project_id": pid_schema(), "segment_ids": {"type": "array", "items": {"type": "string"}}}), &["project_id", "segment_ids"]),
        agent: true, meta: false, mutating: true, h_restore_by_transcript),
    tool!("trim_clip", "Move a clip's boundaries (drag-handle trim), frame-exact.",
        || obj(json!({"project_id": pid_schema(), "clip_id": {"type": "string"}, "new_source_in": {"type": "number"}, "new_source_out": {"type": "number"}}), &["project_id", "clip_id", "new_source_in", "new_source_out"]),
        agent: true, meta: false, mutating: true, h_trim_clip),
    tool!("split_clip", "Split a clip at a source time (playhead).",
        || obj(json!({"project_id": pid_schema(), "clip_id": {"type": "string"}, "at_time": {"type": "number"}}), &["project_id", "clip_id", "at_time"]),
        agent: true, meta: false, mutating: true, h_split_clip),
    tool!("reorder_clip", "Move a clip to a new position in the timeline.",
        || obj(json!({"project_id": pid_schema(), "clip_id": {"type": "string"}, "new_order": {"type": "integer"}}), &["project_id", "clip_id", "new_order"]),
        agent: false, meta: false, mutating: true, h_reorder_clip),
    tool!("set_global_padding", "Apply breathing room (seconds) to the start/end of all talking clips at once.",
        || obj(json!({"project_id": pid_schema(), "start_s": {"type": "number"}, "end_s": {"type": "number"}, "linked": {"type": "boolean"}}), &["project_id", "start_s", "end_s"]),
        agent: true, meta: false, mutating: true, h_set_global_padding),
    tool!("attach_screen_media", "Attach a dual-capture screen file to a project (probed; rides the same clock as the primary media). set_layout then controls compositing.",
        || obj(json!({"project_id": pid_schema(), "file_path": {"type": "string"}}), &["project_id", "file_path"]),
        agent: false, meta: false, h_attach_screen_media),
    tool!("set_layout", "Presentation layout for dual-capture projects (screen_media): mode pip|camera|screen, shape rounded|round, corner tl|tr|bl|br, size 0.10-0.45 (camera width fraction). Composited at preview/export; source files untouched.",
        || obj(json!({"project_id": pid_schema(), "mode": {"type": "string"}, "shape": {"type": "string"}, "corner": {"type": "string"}, "size": {"type": "number"}}), &["project_id"]),
        agent: false, meta: false, h_set_layout),
    tool!("set_audio_offset", "A/V sync nudge, seconds (clamped to ±2). POSITIVE delays audio — the fix when you hear words before lips move (mics start before cameras warm up). Applied non-destructively at preview and export; not an undoable edit.",
        || obj(json!({"project_id": pid_schema(), "offset_s": {"type": "number"}}), &["project_id", "offset_s"]),
        agent: false, meta: false, h_set_audio_offset),
    tool!("append_media", "Append another video file to the END of the project's source (lossless concat to a new file; originals untouched; existing cuts keep their timestamps; timeline extends with an included clip). Inputs must match codec/resolution. Re-run transcribe afterwards to caption the appended span.",
        || obj(json!({"project_id": pid_schema(), "file_path": {"type": "string"}}), &["project_id", "file_path"]),
        agent: false, meta: false, mutating: true, h_append_media),
    tool!("outline_transcript", "Split the FULL transcript into story beats (hook/setup/point/example/tangent/recap/outro) with titles, summaries, cut_priority (1=spine, 5=cut first), and segment_ids. Cached per transcript; refresh=true recomputes (use when an outline came out degenerate). The narrative map to plan cohesive edits against.",
        || obj(json!({"project_id": pid_schema(), "refresh": {"type": "boolean"}}), &["project_id"]),
        agent: true, meta: false, h_outline_transcript),
    tool!("review_flow", "Re-read the EDITED transcript at every cut point and judge whether speech still flows: mid-sentence cuts, orphaned connectives, dangling references. Returns issues with severity and restore_segment_ids suggestions. Run after a batch of cuts; restore what reads broken.",
        || obj(json!({"project_id": pid_schema()}), &["project_id"]),
        agent: true, meta: false, h_review_flow),
    tool!("story_edit", "COHESIVE story-level edit in one call: outline the narrative -> choose whole beats to cut against the instruction -> apply (undoable) -> re-read every cut point -> restore what breaks the flow -> summarize. Long-running (several local-model calls). Frontier orchestrators may prefer composing outline_transcript + apply_edits + review_flow.",
        || obj(json!({"project_id": pid_schema(), "instruction": {"type": "string"}, "target_duration_s": {"type": "number", "description": "optional length target, seconds"}}), &["project_id", "instruction"]),
        agent: true, meta: false, mutating: true, h_story_edit),
    tool!("plan_duration_cut", "Bring the video down to a TARGET DURATION: ranks still-included segments by how central they are to the video (least-central tangents cut first; intro/outro protected). With apply=true (recommended) it plans AND cuts in one undoable step and returns a small receipt with the new included_duration_s. With apply=false it returns the full segment_ids plan for review. THE tool for 'make this 20 minutes'.",
        || obj(json!({"project_id": pid_schema(), "target_duration_s": {"type": "number", "description": "desired included duration, in seconds"}, "apply": {"type": "boolean", "description": "true: plan and cut in one step (recommended)"}}), &["project_id", "target_duration_s"]),
        agent: true, meta: false, mutating: true, h_plan_duration_cut),
    tool!("find_segments", "Hybrid search over the transcript: BM25 + local embeddings fused by reciprocal rank. Natural-language or keyword queries both work. Returns matching segments with ids, times, and scores — use this instead of reading the whole transcript.",
        || obj(json!({"project_id": pid_schema(), "query": {"type": "string"}, "limit": {"type": "integer", "description": "max 50, default 8"}}), &["project_id", "query"]),
        agent: true, meta: false, h_find_segments),
    tool!("read_transcript", "Read the transcript in pages: lean segments (text, times, flags; no word arrays unless include_words). Use offset/limit for long videos instead of get_transcript.",
        || obj(json!({"project_id": pid_schema(), "offset": {"type": "integer"}, "limit": {"type": "integer", "description": "max 200, default 50"}, "include_words": {"type": "boolean"}}), &["project_id"]),
        agent: true, meta: false, h_read_transcript),
    tool!("apply_edits", "POWER TOOL for orchestrators: apply a BATCH of edit operations in one call. Each edit is {\"type\": \"cut_range\"|\"restore_range\"|\"cut_segments\"|\"restore_segments\"|\"trim_clip\"|\"split_clip\"|\"reorder_clip\"|\"set_global_padding\", ...fields} (cut_range: start,end seconds; cut_segments: segment_ids; trim_clip: clip_id,new_source_in,new_source_out; split_clip: clip_id,at_time; set_global_padding: start_s,end_s,linked). Every edit is recorded separately and undoable. Plan with find_segments/read_transcript, then land all cuts in one call.",
        || obj(json!({"project_id": pid_schema(), "edits": {"type": "array", "items": {"oneOf": [
            {"type": "object", "properties": {"type": {"const": "cut_range"}, "start": {"type": "number"}, "end": {"type": "number"}}, "required": ["type", "start", "end"]},
            {"type": "object", "properties": {"type": {"const": "restore_range"}, "start": {"type": "number"}, "end": {"type": "number"}}, "required": ["type", "start", "end"]},
            {"type": "object", "properties": {"type": {"const": "cut_segments"}, "segment_ids": {"type": "array", "items": {"type": "string"}}}, "required": ["type", "segment_ids"]},
            {"type": "object", "properties": {"type": {"const": "restore_segments"}, "segment_ids": {"type": "array", "items": {"type": "string"}}}, "required": ["type", "segment_ids"]},
            {"type": "object", "properties": {"type": {"const": "trim_clip"}, "clip_id": {"type": "string"}, "new_source_in": {"type": "number"}, "new_source_out": {"type": "number"}}, "required": ["type", "clip_id", "new_source_in", "new_source_out"]},
            {"type": "object", "properties": {"type": {"const": "split_clip"}, "clip_id": {"type": "string"}, "at_time": {"type": "number"}}, "required": ["type", "clip_id", "at_time"]},
            {"type": "object", "properties": {"type": {"const": "reorder_clip"}, "clip_id": {"type": "string"}, "new_order": {"type": "integer"}}, "required": ["type", "clip_id", "new_order"]},
            {"type": "object", "properties": {"type": {"const": "set_global_padding"}, "start_s": {"type": "number"}, "end_s": {"type": "number"}, "linked": {"type": "boolean"}}, "required": ["type", "start_s", "end_s"]}
        ]}}}), &["project_id", "edits"]),
        agent: true, meta: false, mutating: true, h_apply_edits),
    tool!("apply_instruction", "Delegate a natural-language edit to the ON-DEVICE model's agent loop (slow; meant for the in-app chat). External orchestrators should use find_segments + apply_edits directly instead.",
        || obj(json!({"project_id": pid_schema(), "instruction": {"type": "string"}, "history": {"type": "array", "description": "recent chat turns, oldest first", "items": {"type": "object", "properties": {"role": {"type": "string", "enum": ["user", "agent"]}, "text": {"type": "string"}}}}}), &["project_id", "instruction"]),
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
    tool!("duplicate_project", "Save-as: clone a project (same media, current cut state, fresh undo history) under a new name, leaving the original untouched.",
        || obj(json!({"project_id": pid_schema(), "name": {"type": "string"}}), &["project_id", "name"]),
        agent: false, meta: false, h_duplicate_project),
    tool!("delete_project", "Move a project to the trash (restorable with restore_project for 30 days, then purged). The source video file on disk is never touched.",
        || obj(json!({"project_id": pid_schema()}), &["project_id"]),
        agent: false, meta: false, h_delete_project),
    tool!("restore_project", "Bring a trashed project back, edit state and all (see list_projects' trash field).",
        || obj(json!({"project_id": pid_schema()}), &["project_id"]),
        agent: false, meta: false, h_restore_project),
    tool!("open_project", "Open an existing project.",
        || obj(json!({"project_id": pid_schema()}), &["project_id"]),
        agent: false, meta: false, h_open_project),
    tool!("save_project", "Persist a project.",
        || obj(json!({"project_id": pid_schema()}), &["project_id"]),
        agent: false, meta: false, h_save_project),
    tool!("list_projects", "List saved projects; trashed ones come back separately in `trash`.", || obj(json!({}), &[]),
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
    tool!("undo_actions", "Undo a SPECIFIC set of recent actions (e.g. everything one chat turn applied). Succeeds only while those actions are still the newest journal entries; otherwise errors so unrelated newer edits are never reverted.",
        || obj(json!({"project_id": pid_schema(), "action_ids": {"type": "array", "items": {"type": "string"}}}), &["project_id", "action_ids"]),
        agent: false, meta: false, mutating: true, h_undo_actions),
    tool!("undo", "Undo the last edit.", || obj(json!({"project_id": pid_schema()}), &["project_id"]),
        agent: false, meta: false, mutating: true, h_undo),
    tool!("redo", "Redo the last undone edit.", || obj(json!({"project_id": pid_schema()}), &["project_id"]),
        agent: false, meta: false, mutating: true, h_redo),
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
            assert!(!(spec.mutating && spec.meta), "{} cannot be both mutating and meta", spec.name);
        }
        // The agent subset is exactly the flagged rows.
        assert_eq!(agent_defs().len(), REGISTRY.iter().filter(|s| s.agent).count());
    }
}
