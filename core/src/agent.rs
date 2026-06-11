//! The local agent loop: instruction + project context + tool schemas →
//! Gemma (via llama.cpp/Ollama, OpenAI-compatible) → tool calls → execute →
//! loop until done. Every step is surfaced as an `agent-step` event and every
//! mutation is an undoable EditAction tagged `local_ai`.

use crate::detect;
use crate::engine::Editor;
use crate::error::{CoreError, Result};
use crate::events::{send, CoreEvent};
use crate::model::{ActionSource, EditAction, EditOp};
use crate::adapters::inference::{ChatMessage, ChatRequest};
use crate::tools;
use serde_json::{json, Value};
use uuid::Uuid;

const MAX_STEPS: usize = 12;

#[derive(Debug, Clone, serde::Serialize)]
pub struct InstructionOutcome {
    pub actions: Vec<EditAction>,
    pub summary: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Chapter {
    pub title: String,
    pub start: f64,
}

fn system_prompt(project_id: Uuid) -> String {
    format!(
        "You are the editing orchestrator inside a local-first video editor. \
         You edit by calling tools against the project with id {project_id}. \
         The transcript is ALREADY in your context below, with each segment's id, \
         times, and text — for most requests you can pick segment ids straight from \
         it without searching. Use find_segments at most ONCE, and only when the \
         context was truncated or you cannot find the content by reading. \
         EDIT IN ONE BATCH: collect every segment id you want to remove, then make \
         a single cut_by_transcript call with ALL the ids (its segment_ids field is \
         an array), or one apply_edits call for mixed operations. Never cut one \
         segment per call, and never search again after you have results. \
         STORY EDITS: when the user wants a cohesive narrative pass — \
         'tighten this', 'make it flow', 'cut the boring parts', 'edit this \
         into a focused video' — call story_edit ONCE with their words (add \
         target_duration_s if they named a length). It outlines the story, \
         cuts whole beats, verifies every cut point still reads, and repairs \
         the flow. Do not attempt that pipeline yourself. \
         DURATION TARGETS: when the user names a length ('make it 20 minutes', \
         'cut it in half'), make ONE call: plan_duration_cut with \
         target_duration_s and apply=true — it ranks, cuts, and reports the new \
         included_duration_s in a single undoable step. When that report is at \
         or below the target, the job is DONE: reply with your summary. Do not \
         ferry segment ids yourself, do not run generate_rough_cut before or \
         after (it only removes fillers and cannot hit a duration), and do not \
         keep cutting past the target. \
         Every change is non-destructive and undoable. When you are done, reply \
         with a one-sentence summary of what you changed instead of calling more \
         tools. \
         ACT, don't ask: never reply with a request for clarification when the \
         conversation already names the work. If the user says 'apply the edits', \
         'do it', 'yes', or similar, carry out the edits proposed earlier in this \
         conversation by calling tools now. Only answer in plain text when the work \
         is finished or genuinely impossible. \
         Always pass project_id=\"{project_id}\" to every tool."
    )
}

/// Registry-owned: see ToolSpec.mutating.
fn is_mutating(tool: &str) -> bool {
    tools::is_mutating(tool)
}

/// Compact transcript + timeline context the model can ground on.
fn project_context(editor: &Editor, project_id: Uuid) -> Result<String> {
    editor.with_project(project_id, |p, t| {
        let mut out = format!(
            "Project: {} — source duration {:.1}s, {} cuts currently, included {:.1}s.\n",
            p.name,
            p.timeline.duration,
            p.timeline.cut_count,
            p.timeline.included_duration()
        );
        if let Some(t) = t {
            out.push_str("Transcript segments (id | start-end | flags | text):\n");
            for s in &t.segments {
                if s.is_silence {
                    continue;
                }
                let mut flags = vec![];
                if s.is_filler {
                    flags.push("filler");
                }
                if s.take_group_id.is_some() {
                    flags.push(if s.is_best_take { "best-take" } else { "alt-take" });
                }
                let flags_s = if flags.is_empty() { "-".to_string() } else { flags.join(",") };
                let line =
                    format!("{} | {:.1}-{:.1} | {} | {}\n", s.id, s.start, s.end, flags_s, s.text);
                if out.len() + line.len() > 24_000 {
                    out.push_str("… (transcript truncated)\n");
                    break;
                }
                out.push_str(&line);
            }
        } else {
            out.push_str("No transcript yet — call transcribe-like tools via the UI first.\n");
        }
        Ok(out)
    })
}

#[allow(clippy::too_many_arguments)]
fn emit_step(
    editor: &Editor,
    project_id: Uuid,
    step: usize,
    kind: &str,
    tool: Option<String>,
    args: Option<Value>,
    result: Option<Value>,
    text: Option<String>,
) {
    send(
        &editor.sink(),
        CoreEvent::AgentStep { project_id, step, kind: kind.into(), tool, args, result, text },
    );
}

fn step_text(editor: &Editor, project_id: Uuid, step: usize, kind: &str, text: impl Into<String>) {
    emit_step(editor, project_id, step, kind, None, None, None, Some(text.into()));
}

/// Collect EditActions out of a tool result (single `action` or an
/// `actions` array — story_edit and apply_edits return batches).
fn harvest_actions(result: &Value, into: &mut Vec<EditAction>) {
    if let Ok(a) = serde_json::from_value::<EditAction>(result["action"].clone()) {
        into.push(a);
    }
    if let Some(arr) = result["actions"].as_array() {
        for v in arr {
            if let Ok(a) = serde_json::from_value::<EditAction>(v.clone()) {
                into.push(a);
            }
        }
    }
}

/// One prior conversation turn, oldest first. `role` is "user" or "agent".
#[derive(Debug, Clone, serde::Deserialize)]
pub struct HistoryTurn {
    pub role: String,
    pub text: String,
}

pub async fn run_instruction(
    editor: &Editor,
    project_id: Uuid,
    instruction: &str,
    history: &[HistoryTurn],
    source: ActionSource,
) -> Result<InstructionOutcome> {
    let inference = editor.inference()?;
    if !inference.healthy().await {
        // Graceful degradation: no local model server. Try the deterministic path.
        return run_instruction_offline(editor, project_id, instruction, source).await;
    }
    let prefs = editor.get_preferences()?;
    run_instruction_with(editor, project_id, instruction, history, source, inference.as_ref(), &prefs.inference_model)
        .await
}

/// The loop itself, parameterized over the model endpoint — the same code
/// drives local Gemma and (opt-in) a frontier model.
pub async fn run_instruction_with(
    editor: &Editor,
    project_id: Uuid,
    instruction: &str,
    history: &[HistoryTurn],
    source: ActionSource,
    inference: &dyn crate::adapters::InferenceClient,
    model: &str,
) -> Result<InstructionOutcome> {
    let context = project_context(editor, project_id)?;
    // Narrate the phases — a long first model call otherwise reads as a hang.
    let (seg_count, dur_min) = editor.with_project(project_id, |p, t| {
        Ok((t.map(|t| t.segments.len()).unwrap_or(0), p.timeline.duration / 60.0))
    })?;
    step_text(
        editor,
        project_id,
        0,
        "thinking",
        format!(
            "loaded the transcript — {seg_count} segments across {dur_min:.0} min; \
             handing it to the local model"
        ),
    );
    let tool_schemas: Vec<Value> = tools::agent_defs()
        .into_iter()
        .map(|d| {
            json!({
                "type": "function",
                "function": { "name": d.name, "description": d.description, "parameters": d.input_schema }
            })
        })
        .collect();

    // Project context lives in the system message so prior conversation turns
    // can sit between it and the new instruction — follow-ups like "apply the
    // edits" then resolve against what the agent said last turn.
    let mut messages = vec![ChatMessage::system(&format!(
        "{}\n\nProject context:\n{context}",
        system_prompt(project_id)
    ))];
    for turn in history.iter().rev().take(12).rev() {
        messages.push(match turn.role.as_str() {
            "user" => ChatMessage::user(&turn.text),
            _ => ChatMessage::assistant(&turn.text),
        });
    }
    messages.push(ChatMessage::user(&format!("Instruction: {instruction}")));
    let mut actions: Vec<EditAction> = vec![];
    let mut summary = String::new();
    // Small local models loop on search (paraphrase, search again, repeat)
    // and run out of steps with 0 edits. Two guards:
    //  - identical calls are answered from cache with a nudge, not re-run
    //  - after 2 read-only rounds with no edit, one steering message demands
    //    a batch edit or a final answer
    let mut seen_calls: std::collections::HashMap<String, Value> = Default::default();
    let mut readonly_rounds = 0usize;
    let mut steered = false;

    for step in 0..MAX_STEPS {
        step_text(
            editor,
            project_id,
            step,
            "thinking",
            if step == 0 {
                "waiting on the local model — first pass over a long transcript is the slow part"
                    .to_string()
            } else {
                format!("waiting on the local model (step {}/{MAX_STEPS})", step + 1)
            },
        );
        let response = match inference
            .chat(ChatRequest {
                model: model.to_string(),
                messages: messages.clone(),
                tools: Some(tool_schemas.clone()),
                temperature: 0.1,
            })
            .await
        {
            Ok(r) => r,
            // Server up but model missing/broken before anything happened:
            // degrade to the deterministic offline editor instead of failing.
            Err(e) if step == 0 => {
                step_text(editor, project_id, 0, "thinking", format!("{e} — falling back to offline editing"));
                return run_instruction_offline(editor, project_id, instruction, source).await;
            }
            Err(e) => return Err(e),
        };
        let msg = response.message;
        let tool_calls = msg.tool_calls.clone().unwrap_or_default();
        if tool_calls.is_empty() {
            summary = msg.content.clone().unwrap_or_else(|| "Done.".into());
            step_text(editor, project_id, step, "final", summary.clone());
            break;
        }
        messages.push(msg);
        let mut round_mutated = false;
        for call in tool_calls {
            let name = call.function.name.clone();
            let mut args: Value =
                serde_json::from_str(&call.function.arguments).unwrap_or_else(|_| json!({}));
            // The model sometimes forgets the project id — inject it.
            if args.get("project_id").and_then(|v| v.as_str()).is_none() {
                args["project_id"] = json!(project_id.to_string());
            }
            emit_step(editor, project_id, step, "tool_call", Some(name.clone()), Some(args.clone()), None, None);
            let call_key = format!("{name}:{args}");
            let result = if let Some(cached) = seen_calls.get(&call_key) {
                // Identical repeat: answer from cache, push toward acting.
                json!({
                    "note": "REPEAT CALL — same result as before. Stop searching; \
                             apply the edit (one batched cut_by_transcript / \
                             apply_edits) or give your final answer.",
                    "result": cached,
                })
            } else {
                let r = match tools::dispatch_basic(editor, &name, &args, source).await {
                    Ok(mut v) => {
                        harvest_actions(&v, &mut actions);
                        // Duration feedback: every edit tells the model where
                        // the cut stands, so duration targets are checkable.
                        if is_mutating(&name) {
                            if let Ok(tl) = editor.get_timeline(project_id) {
                                if let Some(obj) = v.as_object_mut() {
                                    obj.insert(
                                        "included_duration_s".into(),
                                        json!((tl.included_duration() * 10.0).round() / 10.0),
                                    );
                                    obj.insert("source_duration_s".into(), json!(tl.duration));
                                }
                            }
                        }
                        v
                    }
                    Err(e) => e.to_json(),
                };
                seen_calls.insert(call_key, truncate_json(&r));
                r
            };
            round_mutated |= is_mutating(&name);
            emit_step(
                editor,
                project_id,
                step,
                "tool_result",
                Some(name.clone()),
                None,
                Some(truncate_json(&result)),
                None,
            );
            messages.push(ChatMessage::tool_result(
                &call.id,
                serde_json::to_string(&truncate_json(&result))?,
            ));
        }
        if round_mutated {
            readonly_rounds = 0;
        } else {
            readonly_rounds += 1;
            if readonly_rounds >= 2 && !steered {
                steered = true;
                step_text(
                    editor,
                    project_id,
                    step,
                    "thinking",
                    "model keeps reading instead of editing — telling it to act now",
                );
                messages.push(ChatMessage::user(
                    "You have gathered enough. Do NOT search or read again. Either \
                     make the edit NOW — one cut_by_transcript call with ALL the \
                     segment ids (or one apply_edits batch) — or, if no edit is \
                     needed, reply with your final answer in plain text.",
                ));
            }
        }
    }
    if summary.is_empty() {
        summary = format!("Stopped after {MAX_STEPS} steps; {} change(s) applied.", actions.len());
        step_text(editor, project_id, MAX_STEPS, "final", summary.clone());
    }
    Ok(InstructionOutcome { actions, summary })
}

/// No local model server running: cover the common requests deterministically
/// (find + cut) so the app still functions fully offline.
async fn run_instruction_offline(
    editor: &Editor,
    project_id: Uuid,
    instruction: &str,
    source: ActionSource,
) -> Result<InstructionOutcome> {
    step_text(
        editor,
        project_id,
        0,
        "thinking",
        "Local model server not reachable — using the built-in offline editor (keyword search + cut).",
    );
    let lower = instruction.to_lowercase();
    let wants_cut = ["cut", "remove", "delete", "trim out", "get rid"]
        .iter()
        .any(|k| lower.contains(k));
    let ids: Vec<Uuid> = editor.with_project(project_id, |_, t| {
        let t = t.ok_or_else(|| CoreError::InvalidArg("no transcript yet".into()))?;
        Ok(detect::find_segments(t, instruction).into_iter().map(|s| s.id).collect())
    })?;
    if !wants_cut || ids.is_empty() {
        let summary = if ids.is_empty() {
            "I couldn't find matching footage for that instruction (offline keyword search). \
             Start the local model server (Ollama or llama-server) for full conversational editing."
                .to_string()
        } else {
            format!(
                "Found {} matching segment(s) but the instruction doesn't look like a cut. \
                 Start the local model server for more than find-and-cut.",
                ids.len()
            )
        };
        step_text(editor, project_id, 1, "final", summary.clone());
        return Ok(InstructionOutcome { actions: vec![], summary });
    }
    // Keep it conservative: cut only the single best match offline.
    let target = vec![ids[0]];
    emit_step(
        editor,
        project_id,
        1,
        "tool_call",
        Some("cut_by_transcript".into()),
        Some(json!({ "segment_ids": target })),
        None,
        None,
    );
    let outcome =
        editor.apply_edit(project_id, EditOp::CutSegments { segment_ids: target }, source)?;
    let action = outcome.action;
    let summary = format!("Cut the best-matching segment for: “{instruction}”. Undo if it picked wrong.");
    step_text(editor, project_id, 2, "final", summary.clone());
    Ok(InstructionOutcome { actions: vec![action], summary })
}

fn truncate_json(v: &Value) -> Value {
    // Keep tool results the model sees small (timelines can be large).
    let s = serde_json::to_string(v).unwrap_or_default();
    if s.len() <= 6000 {
        return v.clone();
    }
    match v {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                if k == "timeline" {
                    out.insert(
                        "timeline".into(),
                        json!({
                            "cut_count": val["cut_count"],
                            "duration": val["duration"],
                            "clips": format!("{} clips (omitted)", val["clips"].as_array().map(|a| a.len()).unwrap_or(0)),
                        }),
                    );
                } else if k.ends_with("segment_ids") && val.as_array().map(|a| a.len() > 20).unwrap_or(false) {
                    // NEVER hand the model a silently-shortened id list — it
                    // will act on the fragment as if it were the whole plan.
                    // (This exact bug shipped a 20-of-408 cut.)
                    out.insert(
                        "segment_ids".into(),
                        json!(format!(
                            "{} ids (too many to list — use a tool that acts on them \
                             server-side instead of copying ids)",
                            val.as_array().map(|a| a.len()).unwrap_or(0)
                        )),
                    );
                } else {
                    out.insert(k.clone(), truncate_json(val));
                }
            }
            Value::Object(out)
        }
        // Recurse into elements so nested id arrays (beats[i].segment_ids,
        // issues[i].restore_segment_ids, actions[i].op.segment_ids) get the
        // same stubbing as top-level ones.
        Value::Array(arr) => {
            Value::Array(arr.iter().take(20).map(truncate_json).collect())
        }
        other => other.clone(),
    }
}

// ----------------------------------------------------- transcript cleanup

/// Gemma post-pass over whisper output: fix casing, punctuation, and obvious
/// mis-heard words — text only, timestamps untouched. A correction is
/// rejected unless its word count matches the original segment exactly, so
/// word-level timing always stays valid. Best-effort: any failure leaves the
/// whisper transcript as-is.
pub async fn clean_transcript(editor: &Editor, project_id: Uuid) -> Result<u32> {
    let inference = editor.inference()?;
    if !inference.healthy().await {
        return Ok(0);
    }
    let prefs = editor.get_preferences()?;
    let transcript = editor
        .get_transcript(project_id)?
        .ok_or_else(|| CoreError::InvalidArg("no transcript".into()))?;
    let speech: Vec<(Uuid, String)> = transcript
        .segments
        .iter()
        .filter(|s| !s.is_silence && !s.text.is_empty())
        .map(|s| (s.id, s.text.clone()))
        .collect();
    if speech.is_empty() {
        return Ok(0);
    }
    let mut corrections: Vec<(Uuid, String)> = vec![];
    for chunk in speech.chunks(40) {
        let numbered: String = chunk
            .iter()
            .enumerate()
            .map(|(i, (_, t))| format!("{i}: {t}"))
            .collect::<Vec<_>>()
            .join("\n");
        let response = match inference
            .chat(ChatRequest {
                model: prefs.inference_model.clone(),
                messages: vec![
                    ChatMessage::system(
                        "You fix automatic speech-recognition output from a talking-head video. \
                         For each numbered line, correct casing, punctuation, and mis-heard \
                         words. ASR often garbles filler sounds — short nonsense words like \
                         'Wah', 'Erm', 'Hm' at the start of a sentence are usually 'Um' or \
                         'Uh'; fix those. Also fix clearly mis-heard technical terms from \
                         context. CRITICAL: keep exactly the same number of words in every \
                         line — never merge, split, add, remove, or reorder words. Reply with \
                         ONLY a JSON object mapping line numbers (as strings) to corrected \
                         lines, including only the lines you changed. Reply {} if nothing \
                         needs fixing.",
                    ),
                    ChatMessage::user(&numbered),
                ],
                tools: None,
                temperature: 0.0,
            })
            .await
        {
            Ok(r) => r,
            Err(_) => continue,
        };
        let text = response.message.content.unwrap_or_default();
        let Some(json_str) = text.find('{').and_then(|s| text.rfind('}').map(|e| &text[s..=e]))
        else {
            continue;
        };
        let Ok(map) = serde_json::from_str::<std::collections::HashMap<String, String>>(json_str)
        else {
            continue;
        };
        for (k, corrected) in map {
            let Ok(i) = k.trim().parse::<usize>() else { continue };
            let Some((id, original)) = chunk.get(i) else { continue };
            let same_word_count =
                corrected.split_whitespace().count() == original.split_whitespace().count();
            if same_word_count && corrected != *original {
                corrections.push((*id, corrected));
            }
        }
    }
    if corrections.is_empty() {
        return Ok(0);
    }
    editor.update_segment_texts(project_id, &corrections)
}

// ------------------------------------------------------------- metadata

/// Chapters: LLM when the local server is up, deterministic fallback (silence
/// boundaries) otherwise.
pub async fn generate_chapters(editor: &Editor, project_id: Uuid) -> Result<Vec<Chapter>> {
    let inference = editor.inference()?;
    if inference.healthy().await {
        if let Ok(chapters) = llm_chapters(editor, project_id).await {
            if !chapters.is_empty() {
                return Ok(chapters);
            }
        }
    }
    heuristic_chapters(editor, project_id)
}

async fn llm_chapters(editor: &Editor, project_id: Uuid) -> Result<Vec<Chapter>> {
    let prefs = editor.get_preferences()?;
    let context = project_context(editor, project_id)?;
    let inference = editor.inference()?;
    let response = inference
        .chat(ChatRequest {
            model: prefs.inference_model,
            messages: vec![
                ChatMessage::system(
                    "Produce YouTube chapters for this video. Reply with ONLY a JSON array: \
                     [{\"title\": string, \"start\": seconds}]. 3-8 chapters, first at 0.",
                ),
                ChatMessage::user(&context),
            ],
            tools: None,
            temperature: 0.2,
        })
        .await?;
    let text = response.message.content.unwrap_or_default();
    let json_str = text
        .find('[')
        .and_then(|s| text.rfind(']').map(|e| &text[s..=e]))
        .ok_or_else(|| CoreError::Inference("no JSON in chapter response".into()))?;
    Ok(serde_json::from_str(json_str)?)
}

fn heuristic_chapters(editor: &Editor, project_id: Uuid) -> Result<Vec<Chapter>> {
    editor.with_project(project_id, |p, t| {
        let t = t.ok_or_else(|| CoreError::InvalidArg("no transcript".into()))?;
        let mut chapters = vec![];
        let mut last_chapter_at = -f64::INFINITY;
        let min_gap = (p.timeline.duration / 8.0).max(20.0);
        let mut prev_end = 0.0_f64;
        for seg in t.segments.iter().filter(|s| !s.is_silence && !s.is_filler && !s.text.is_empty()) {
            let opens_after_pause = seg.start - prev_end > 1.0 || chapters.is_empty();
            if opens_after_pause && seg.start - last_chapter_at >= min_gap {
                let title: String = seg.text.split_whitespace().take(7).collect::<Vec<_>>().join(" ");
                chapters.push(Chapter {
                    title,
                    start: if chapters.is_empty() { 0.0 } else { p.timeline.source_to_output(seg.start) },
                });
                last_chapter_at = seg.start;
            }
            prev_end = seg.end;
        }
        Ok(chapters)
    })
}

pub async fn generate_title_description(editor: &Editor, project_id: Uuid) -> Result<Value> {
    let inference = editor.inference()?;
    let prefs = editor.get_preferences()?;
    if inference.healthy().await {
        let context = project_context(editor, project_id)?;
        let response = inference
            .chat(ChatRequest {
                model: prefs.inference_model,
                messages: vec![
                    ChatMessage::system(
                        "Suggest video metadata. Reply with ONLY JSON: \
                         {\"titles\": [3 strings], \"description\": string}.",
                    ),
                    ChatMessage::user(&context),
                ],
                tools: None,
                temperature: 0.6,
            })
            .await?;
        let text = response.message.content.unwrap_or_default();
        if let Some(json_str) =
            text.find('{').and_then(|s| text.rfind('}').map(|e| &text[s..=e]))
        {
            if let Ok(v) = serde_json::from_str::<Value>(json_str) {
                return Ok(v);
            }
        }
    }
    // Fallback: first strong sentence as title, opening lines as description.
    editor.with_project(project_id, |_, t| {
        let t = t.ok_or_else(|| CoreError::InvalidArg("no transcript".into()))?;
        let sentences: Vec<&str> = t
            .segments
            .iter()
            .filter(|s| !s.is_silence && !s.is_filler && s.text.split_whitespace().count() > 5)
            .map(|s| s.text.as_str())
            .collect();
        let title = sentences.get(1).or_else(|| sentences.first()).unwrap_or(&"Untitled video");
        Ok(json!({
            "titles": [title],
            "description": sentences.iter().take(4).cloned().collect::<Vec<_>>().join(" "),
        }))
    })
}
