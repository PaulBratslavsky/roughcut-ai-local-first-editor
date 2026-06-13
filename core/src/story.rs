//! Story-level editing: outline the narrative, plan cuts against the
//! outline, verify the result still FLOWS, repair. Each step is a tool
//! (composable by frontier orchestrators over MCP); [`story_edit`] composes
//! them for the in-app chat, narrating every phase.
//!
//! Design rule learned the hard way: the local model never orchestrates a
//! long flow — the pipeline is deterministic scaffolding with the LLM
//! embedded inside each step, strict JSON contracts, index-based references
//! (small models mangle uuids), and non-LLM fallbacks everywhere.

use crate::engine::Editor;
use crate::error::{CoreError, Result};
use crate::events::{send, CoreEvent};
use crate::model::{ActionSource, EditAction, EditOp, TranscriptSegment};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// ------------------------------------------------------------------ outline

/// One narrative beat of the video.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Beat {
    pub title: String,
    pub summary: String,
    /// hook | setup | point | example | tangent | recap | outro | section
    pub role: String,
    /// 1 = the spine of the video … 5 = cut first.
    pub cut_priority: u8,
    pub segment_ids: Vec<Uuid>,
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outline {
    pub beats: Vec<Beat>,
    /// "llm" or "heuristic" (no model server reachable).
    pub method: String,
}

/// Speech segments only, in order — the shared working set for every step.
fn speech_segments(editor: &Editor, project_id: Uuid) -> Result<Vec<TranscriptSegment>> {
    editor.with_project(project_id, |_, t| {
        let t = t.ok_or_else(|| CoreError::InvalidArg("no transcript; transcribe first".into()))?;
        Ok(t.segments.iter().filter(|s| !s.is_silence && !s.text.is_empty()).cloned().collect())
    })
}

fn transcript_hash(speech: &[TranscriptSegment]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for s in speech {
        h.update(s.id.as_bytes());
        h.update(s.text.as_bytes());
    }
    format!("{:x}", h.finalize())[..16].to_string()
}

/// Outline the FULL transcript into story beats. Chunked map over the
/// transcript for long videos; cached against the transcript hash; falls
/// back to pause-boundary sections when no model server is reachable.
pub async fn outline(editor: &Editor, project_id: Uuid) -> Result<Outline> {
    outline_with(editor, project_id, false).await
}

/// `refresh` bypasses and replaces the cache — the escape hatch when an LLM
/// outline came out degenerate.
pub async fn outline_with(editor: &Editor, project_id: Uuid, refresh: bool) -> Result<Outline> {
    let speech = speech_segments(editor, project_id)?;
    if speech.is_empty() {
        return Err(CoreError::InvalidArg("transcript has no speech".into()));
    }
    let hash = transcript_hash(&speech);
    let cache_key = format!("outline:{project_id}");
    if let Ok(Some(raw)) = editor.store().get_kv(&cache_key) {
        if refresh {
            // fall through to recompute
        } else
        if let Ok((cached_hash, outline)) = serde_json::from_str::<(String, Outline)>(&raw) {
            if cached_hash == hash {
                return Ok(outline);
            }
        }
    }

    let inference = editor.inference()?;
    let outline = if inference.healthy().await {
        let prefs = editor.get_preferences()?;
        match llm_outline(&speech, inference.as_ref(), &prefs.inference_model).await {
            Ok(beats) if beats.len() >= 2 => Outline { beats, method: "llm".into() },
            _ => Outline { beats: heuristic_beats(&speech), method: "heuristic".into() },
        }
    } else {
        Outline { beats: heuristic_beats(&speech), method: "heuristic".into() }
    };

    // Only LLM outlines are worth caching; the heuristic is cheap and
    // deterministic, and caching it would shadow the LLM forever after one
    // model outage (the cache key is the transcript hash, which edits never
    // change).
    if outline.method == "llm" {
        let _ = editor
            .store()
            .set_kv(&cache_key, &serde_json::to_string(&(hash, outline.clone()))?);
    }
    Ok(outline)
}

/// ~120 segments per chunk keeps the prompt well inside a local model's
/// comfort zone; absolute indexes survive the chunking.
async fn llm_outline(
    speech: &[TranscriptSegment],
    inference: &dyn crate::adapters::InferenceClient,
    model: &str,
) -> Result<Vec<Beat>> {
    const CHUNK: usize = 120;
    let mut beats = vec![];
    for chunk_start in (0..speech.len()).step_by(CHUNK) {
        let chunk_end = (chunk_start + CHUNK).min(speech.len());
        let numbered: String = (chunk_start..chunk_end)
            .map(|i| format!("{i}| {}", speech[i].text))
            .collect::<Vec<_>>()
            .join("\n");
        let chunk_minutes =
            (speech[chunk_end - 1].end - speech[chunk_start].start).max(60.0) / 60.0;
        let target_beats = (chunk_minutes / 2.5).ceil().max(2.0) as usize;
        #[derive(Deserialize)]
        struct RawBeat {
            title: String,
            #[serde(default)]
            summary: String,
            #[serde(default)]
            role: String,
            #[serde(default = "default_priority")]
            cut_priority: u8,
            first: usize,
            last: usize,
        }
        fn default_priority() -> u8 {
            3
        }
        let beat_schema = serde_json::json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "title": {"type": "string"},
                    "summary": {"type": "string"},
                    "role": {"type": "string", "enum": ["hook","setup","point","example","tangent","recap","outro"]},
                    "cut_priority": {"type": "integer", "minimum": 1, "maximum": 5},
                    "first": {"type": "integer"},
                    "last": {"type": "integer"}
                },
                "required": ["title", "first", "last"]
            }
        });
        let mut raw: Vec<RawBeat> = crate::llm::ask_json_schema(
            inference,
            model,
            "outline",
            &format!(
                "You analyze a talking-head video transcript and split it into STORY \
                 BEATS (hook, setup, a point being made, an example, a tangent, a \
                 recap, an outro). A beat is a COMPLETE topic — typically 2-4 \
                 minutes of speech; never under a minute unless it is clearly a \
                 self-contained tangent. This chunk covers ~{chunk_minutes:.0} \
                 minutes: produce AT MOST {target_beats} beats. Lines are numbered. \
                 Reply with ONLY a JSON array: \
                 [{{\"title\": short string, \"summary\": one sentence, \"role\": \
                 \"hook\"|\"setup\"|\"point\"|\"example\"|\"tangent\"|\"recap\"|\"outro\", \
                 \"cut_priority\": 1-5 (1 = the spine of the video, 5 = cut first), \
                 \"first\": first line number, \"last\": last line number}}]. Beats \
                 must be contiguous, non-overlapping, and cover every line."
            ),
            &numbered,
            0.1,
            Some(&beat_schema),
        )
        .await?;
        // The prompt demands contiguous, non-overlapping beats; weak models
        // don't always comply. Sort and clip so a shared segment can never
        // belong to two beats (a cut "tangent" must not gut a kept "point").
        raw.sort_by_key(|b| b.first);
        let mut next_free = chunk_start;
        for b in raw {
            let first = b.first.clamp(chunk_start, chunk_end.saturating_sub(1)).max(next_free);
            let last = b.last.clamp(first, chunk_end.saturating_sub(1));
            if first > last || first >= chunk_end {
                continue; // fully clipped away by an earlier overlapping beat
            }
            next_free = last + 1;
            let segs = &speech[first..=last];
            beats.push(Beat {
                title: b.title,
                summary: b.summary,
                role: if b.role.is_empty() { "section".into() } else { b.role },
                cut_priority: b.cut_priority.clamp(1, 5),
                segment_ids: segs.iter().map(|s| s.id).collect(),
                start: segs.first().map(|s| s.start).unwrap_or(0.0),
                end: segs.last().map(|s| s.end).unwrap_or(0.0),
            });
        }
    }
    // Chunk-local outlining over-fragments (a 47-min video once came back
    // as 47 beats). Merge sub-minute beats into their calmer neighbor —
    // beat-level editing needs MINUTES-scale units or every cut severs
    // connected tissue.
    let mut merged: Vec<Beat> = vec![];
    for beat in beats {
        match merged.last_mut() {
            Some(prev) if (beat.end - beat.start) < 60.0 || (prev.end - prev.start) < 60.0 => {
                // Merge into prev: keep the longer side's identity.
                if (beat.end - beat.start) > (prev.end - prev.start) {
                    prev.title = beat.title;
                    prev.summary = beat.summary;
                    prev.role = beat.role;
                }
                prev.cut_priority = prev.cut_priority.min(beat.cut_priority);
                prev.segment_ids.extend(beat.segment_ids);
                prev.end = beat.end;
            }
            _ => merged.push(beat),
        }
    }
    Ok(merged)
}

/// No model server: sections at speech pauses — still useful for navigation
/// and as the skeleton for review.
fn heuristic_beats(speech: &[TranscriptSegment]) -> Vec<Beat> {
    let mut beats: Vec<Beat> = vec![];
    let target = (speech.len() / 8).max(5);
    let mut current: Vec<&TranscriptSegment> = vec![];
    for (i, seg) in speech.iter().enumerate() {
        let pause_before =
            i > 0 && seg.start - speech[i - 1].end > 1.0 && current.len() >= target.min(4);
        if pause_before && !current.is_empty() {
            beats.push(beat_from(&current));
            current.clear();
        }
        current.push(seg);
    }
    if !current.is_empty() {
        beats.push(beat_from(&current));
    }
    fn beat_from(segs: &[&TranscriptSegment]) -> Beat {
        let title: String =
            segs[0].text.split_whitespace().take(6).collect::<Vec<_>>().join(" ");
        Beat {
            title,
            summary: String::new(),
            role: "section".into(),
            cut_priority: 3,
            segment_ids: segs.iter().map(|s| s.id).collect(),
            start: segs[0].start,
            end: segs.last().unwrap().end,
        }
    }
    beats
}

// -------------------------------------------------------------- flow review

#[derive(Debug, Clone, Serialize)]
pub struct FlowIssue {
    /// broken_sentence | abrupt_start | flow (LLM judgment)
    pub kind: String,
    /// "high" issues gate coherence; "medium" are advisory.
    pub severity: String,
    /// The included segment just before/after the suspect boundary.
    pub at_segment: Uuid,
    pub description: String,
    /// Restoring these excluded segments is the suggested fix (may be empty).
    pub restore_segment_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FlowReview {
    pub coherent: bool,
    pub boundaries_checked: usize,
    pub issues: Vec<FlowIssue>,
    pub method: String,
}

struct Boundary {
    before: Vec<TranscriptSegment>,
    excluded: Vec<TranscriptSegment>,
    after: Vec<TranscriptSegment>,
}

/// Walk the cut boundaries of the CURRENT timeline: deterministic checks
/// (sentence sliced mid-thought, orphaned connectives) plus an LLM judgment
/// of each boundary with its before/cut/after context.
pub async fn review_flow(editor: &Editor, project_id: Uuid) -> Result<FlowReview> {
    review_flow_mode(editor, project_id, true).await
}

/// `llm=false` runs ONLY the cheap deterministic boundary checks (no model
/// call) — the lightweight advisory tier for lower-risk edits.
pub async fn review_flow_mode(editor: &Editor, project_id: Uuid, llm: bool) -> Result<FlowReview> {
    let speech = speech_segments(editor, project_id)?;
    let included: Vec<bool> = editor.with_project(project_id, |p, _| {
        Ok(speech
            .iter()
            .map(|s| {
                let overlap: f64 = p
                    .timeline
                    .included_clips()
                    .map(|c| (s.end.min(c.source_out) - s.start.max(c.source_in)).max(0.0))
                    .sum();
                overlap > (s.end - s.start) * 0.5
            })
            .collect())
    })?;

    // Collect boundaries: runs of excluded speech between included speech.
    let mut boundaries: Vec<Boundary> = vec![];
    let mut i = 0;
    while i < speech.len() {
        if !included[i] {
            let cut_start = i;
            while i < speech.len() && !included[i] {
                i += 1;
            }
            let before: Vec<_> = (0..cut_start)
                .rev()
                .filter(|&j| included[j])
                .take(2)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(|j| speech[j].clone())
                .collect();
            let after: Vec<_> =
                (i..speech.len()).filter(|&j| included[j]).take(2).map(|j| speech[j].clone()).collect();
            if !before.is_empty() && !after.is_empty() {
                boundaries.push(Boundary {
                    before,
                    excluded: speech[cut_start..i].to_vec(),
                    after,
                });
            }
        } else {
            i += 1;
        }
    }

    let mut issues: Vec<FlowIssue> = vec![];
    // Deterministic pass — cheap, always on.
    for b in &boundaries {
        let prev = b.before.last().unwrap();
        let next = b.after.first().unwrap();
        let prev_trim = prev.text.trim_end();
        if !prev_trim.ends_with(['.', '!', '?', '…']) {
            issues.push(FlowIssue {
                kind: "broken_sentence".into(),
                severity: "high".into(),
                at_segment: prev.id,
                description: format!("cut lands mid-thought after “…{}”", tail(prev_trim, 50)),
                restore_segment_ids: b.excluded.iter().map(|s| s.id).collect(),
            });
        }
        let lower = next.text.trim_start().to_lowercase();
        const CONNECTIVES: [&str; 8] =
            ["so ", "and ", "but ", "because ", "which ", "that's why", "like i said", "as i "];
        if CONNECTIVES.iter().any(|c| lower.starts_with(c)) {
            issues.push(FlowIssue {
                kind: "abrupt_start".into(),
                severity: "medium".into(),
                at_segment: next.id,
                description: format!(
                    "after the cut, speech resumes mid-argument: “{}…”",
                    head(next.text.trim(), 60)
                ),
                restore_segment_ids: vec![],
            });
        }
    }

    // LLM pass — judges each boundary in context; capped to keep one call.
    let inference = editor.inference()?;
    let mut method = "deterministic".to_string();
    const MAX_BOUNDARIES: usize = 40;
    if llm && !boundaries.is_empty() && inference.healthy().await {
        method = "llm".into();
        let prefs = editor.get_preferences()?;
        let listed: String = boundaries
            .iter()
            .take(MAX_BOUNDARIES)
            .enumerate()
            .map(|(n, b)| {
                format!(
                    "BOUNDARY {n}\nbefore: {}\n[CUT: {} segment(s), e.g. “{}”]\nafter: {}",
                    b.before.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" "),
                    b.excluded.len(),
                    head(&b.excluded[0].text, 70),
                    b.after.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" "),
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        #[derive(Deserialize)]
        struct Verdict {
            b: usize,
            ok: bool,
            #[serde(default)]
            why: String,
            #[serde(default)]
            restore: bool,
        }
        let verdict_schema = serde_json::json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "b": {"type": "integer"},
                    "ok": {"type": "boolean"},
                    "why": {"type": "string"},
                    "restore": {"type": "boolean"}
                },
                "required": ["b", "ok"]
            }
        });
        let verdicts: Result<Vec<Verdict>> = crate::llm::ask_json_schema(
            inference.as_ref(),
            &prefs.inference_model,
            "flow review",
            "You review CUT POINTS in an edited talking-head video. For each \
             numbered boundary, judge whether speech flows naturally from \
             'before' to 'after' with the middle removed. Reply with ONLY a \
             JSON array, one entry PER BOUNDARY: [{\"b\": number, \"ok\": \
             true|false, \"why\": short reason, \"restore\": true if putting \
             the cut material back is the right fix}]. Dangling references \
             (\"as I said\", unintroduced topics) and mid-sentence jumps are \
             NOT ok.",
            &listed,
            0.0,
            Some(&verdict_schema),
        )
        .await;
        {
            {
                if let Ok(verdicts) = verdicts {
                    for v in verdicts {
                        if v.ok {
                            continue;
                        }
                        let Some(b) = boundaries.get(v.b) else { continue };
                        issues.push(FlowIssue {
                            kind: "flow".into(),
                            severity: "high".into(),
                            at_segment: b.after.first().unwrap().id,
                            description: if v.why.is_empty() {
                                "the model judged this cut point unnatural".into()
                            } else {
                                v.why
                            },
                            restore_segment_ids: if v.restore {
                                b.excluded.iter().map(|s| s.id).collect()
                            } else {
                                vec![]
                            },
                        });
                    }
                }
            }
        }
    }

    let coherent = !issues.iter().any(|i| i.severity == "high");
    Ok(FlowReview { coherent, boundaries_checked: boundaries.len(), issues, method })
}

fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
fn tail(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    chars[chars.len().saturating_sub(n)..].iter().collect()
}

// --------------------------------------------------------------- story_edit

#[derive(Debug, Clone, Serialize)]
pub struct StoryEditOutcome {
    pub actions: Vec<EditAction>,
    pub summary: String,
    pub before_s: f64,
    pub after_s: f64,
    pub coherent: bool,
    pub issues_remaining: usize,
}

fn narrate(editor: &Editor, project_id: Uuid, step: usize, text: impl Into<String>) {
    send(
        &editor.sink(),
        CoreEvent::AgentStep {
            project_id,
            step,
            kind: "thinking".into(),
            tool: None,
            args: None,
            result: None,
            text: Some(text.into()),
        },
    );
}

/// The composite: outline → plan cuts against the outline → apply (one
/// undoable action) → review flow → restore what reads broken (second
/// action) → summarize. Narrated via agent-step events so the chat shows
/// every phase.
/// One story edit per project at a time: the pipeline spans several model
/// calls and three mutations; interleaving two of them scrambles intent.
fn busy_set() -> &'static std::sync::Mutex<std::collections::HashSet<Uuid>> {
    static BUSY: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<Uuid>>> =
        std::sync::OnceLock::new();
    BUSY.get_or_init(Default::default)
}

pub async fn story_edit(
    editor: &Editor,
    project_id: Uuid,
    instruction: &str,
    target_s: Option<f64>,
    source: ActionSource,
) -> Result<StoryEditOutcome> {
    if let Some(t) = target_s {
        if t <= 0.0 {
            return Err(CoreError::InvalidArg("target_duration_s must be positive".into()));
        }
    }
    let Some(_busy) = crate::inflight::try_claim(busy_set(), project_id) else {
        return Err(CoreError::InvalidArg(
            "a story edit is already running for this project".into(),
        ));
    };

    let before_s = editor.get_timeline(project_id)?.included_duration();
    let mut actions: Vec<EditAction> = vec![];
    let inference = editor.inference()?;
    if !inference.healthy().await {
        // Honest degradation: without a model there is no narrative judgment.
        if let Some(target) = target_s {
            narrate(editor, project_id, 0, "no local model — falling back to the duration planner");
            let plan = editor.plan_duration_cut(project_id, target)?;
            let mut actions = vec![];
            if !plan.segment_ids.is_empty() {
                let outcome = editor.apply_edit(
                    project_id,
                    EditOp::CutSegments { segment_ids: plan.segment_ids },
                    source,
                )?;
                actions.push(outcome.action);
            }
            let after_s = editor.get_timeline(project_id)?.included_duration();
            return Ok(StoryEditOutcome {
                actions,
                summary: format!(
                    "No local model server, so I used the semantic duration planner: \
                     {before_s:.0}s → {after_s:.0}s. Start the model server for story-aware edits."
                ),
                before_s,
                after_s,
                coherent: true,
                issues_remaining: 0,
            });
        }
        return Err(CoreError::Unavailable(
            "story editing needs the local model server (Ollama or llama-server)".into(),
        ));
    }

    // 0. Dead air first: hybrid-CONFIRMED silences are uncontroversial and
    // cutting them up front means beats are judged on their content, not
    // their pauses. Shared with `remove_silences` — same code, can't drift.
    let silence_actions = editor.cut_confirmed_silences(project_id, source).await?;
    if !silence_actions.is_empty() {
        narrate(
            editor,
            project_id,
            0,
            format!("cutting {} audio-confirmed dead-air range(s) first", silence_actions.len()),
        );
        actions.extend(silence_actions);
    }

    // 1. Outline.
    narrate(editor, project_id, 0, "reading the full transcript and outlining the story…");
    let outline = outline(editor, project_id).await?;
    narrate(
        editor,
        project_id,
        0,
        format!("outlined {} beats ({})", outline.beats.len(), outline.method),
    );

    // 2. Plan cuts against the outline.
    narrate(editor, project_id, 1, "choosing which beats serve the story — and which don't…");
    let prefs = editor.get_preferences()?;
    let listed: String = outline
        .beats
        .iter()
        .enumerate()
        .map(|(n, b)| {
            format!(
                "{n}| [{}] {} — {} ({:.0}s, cut_priority {})",
                b.role,
                b.title,
                b.summary,
                b.end - b.start,
                b.cut_priority
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let target_line = target_s
        .map(|t| format!("The user wants the result around {:.0} minutes.", t / 60.0))
        .unwrap_or_default();
    #[derive(Deserialize)]
    struct Plan {
        #[serde(default)]
        cut_beats: Vec<usize>,
        #[serde(default)]
        rationale: String,
    }
    let plan_schema = serde_json::json!({
        "type": "object",
        "properties": {
            "cut_beats": {"type": "array", "items": {"type": "integer"}},
            "rationale": {"type": "string"}
        },
        "required": ["cut_beats"]
    });
    let plan: Plan = crate::llm::ask_json_schema(
        inference.as_ref(),
        &prefs.inference_model,
        "story plan",
        "You are editing a talking-head video at the STORY level. Given the \
         user's instruction and the beat outline, choose which beats to CUT \
         ENTIRELY. Cut tangents, repeated takes, and beats that don't serve \
         the instruction; never cut the hook or the payoff unless asked. \
         PREFER CUTTING RUNS of adjacent beats — every isolated cut between \
         two kept beats creates a visible seam, so only cut a lone \
         mid-video beat when it is clearly a tangent (cut_priority 4-5). \
         Reply with ONLY JSON: {\"cut_beats\": [beat numbers], \"rationale\": \
         one sentence}.",
        &format!("Instruction: {instruction}\n{target_line}\n\nBeats:\n{listed}"),
        0.1,
        Some(&plan_schema),
    )
    .await?;
    // Guardrails: a "story edit" that deletes (almost) everything is a bug,
    // not a bold choice. Dedup first (weak models repeat indexes), refuse
    // cutting ALL beats at any outline size, and refuse on cut DURATION too
    // (beat counts lie when one beat spans most of the video).
    let mut valid: Vec<usize> =
        plan.cut_beats.into_iter().filter(|&n| n < outline.beats.len()).collect();
    valid.sort_unstable();
    valid.dedup();
    let all_but_one = valid.len() >= outline.beats.len().saturating_sub(1);
    if (valid.len() >= outline.beats.len()) || (all_but_one && outline.beats.len() > 2) {
        return Err(CoreError::Inference(
            "the plan cut nearly every beat — refusing; try a more specific instruction".into(),
        ));
    }
    // Contiguity guard: an isolated low-priority cut sandwiched between two
    // kept beats severs connected speech for marginal savings — drop those
    // from the plan (the model was told; this enforces it).
    let cut_set: std::collections::HashSet<usize> = valid.iter().copied().collect();
    let before_guard = valid.len();
    valid.retain(|&n| {
        let prev_kept = n == 0 || !cut_set.contains(&(n - 1));
        let next_kept = n + 1 >= outline.beats.len() || !cut_set.contains(&(n + 1));
        let isolated_mid = prev_kept && next_kept && n > 0 && n + 1 < outline.beats.len();
        !(isolated_mid && outline.beats[n].cut_priority <= 3)
    });
    if valid.len() < before_guard {
        narrate(
            editor,
            project_id,
            1,
            format!(
                "kept {} isolated beat(s) the plan wanted to cut — lone mid-video cuts \
                 leave seams",
                before_guard - valid.len()
            ),
        );
    }
    let cut_duration: f64 =
        valid.iter().map(|&n| outline.beats[n].end - outline.beats[n].start).sum();
    let justified = target_s.map(|t| before_s - cut_duration >= t * 0.5).unwrap_or(false);
    if before_s > 0.0 && cut_duration > before_s * 0.8 && !justified {
        return Err(CoreError::Inference(
            "the plan removes over 80% of the video with no target to justify it — refusing"
                .into(),
        ));
    }

    let cut_ids: Vec<Uuid> =
        valid.iter().flat_map(|&n| outline.beats[n].segment_ids.clone()).collect();
    if !cut_ids.is_empty() {
        let titles: Vec<&str> =
            valid.iter().map(|&n| outline.beats[n].title.as_str()).collect();
        narrate(
            editor,
            project_id,
            2,
            format!("cutting {} beat(s): {}", valid.len(), titles.join(" · ")),
        );
        let outcome =
            editor.apply_edit(project_id, EditOp::CutSegments { segment_ids: cut_ids }, source)?;
        actions.push(outcome.action);
    } else {
        narrate(editor, project_id, 2, "the plan keeps every beat — no whole-beat cuts");
    }

    // From here on, edits have LANDED: any later failure must report the
    // applied actions instead of throwing them away (an Err here would tell
    // the caller "nothing happened" while the timeline changed).
    macro_rules! or_partial {
        ($expr:expr, $what:literal) => {
            match $expr {
                Ok(v) => v,
                Err(e) => {
                    let after_s = editor
                        .get_timeline(project_id)
                        .map(|t| t.included_duration())
                        .unwrap_or(before_s);
                    return Ok(StoryEditOutcome {
                        summary: format!(
                            "Edit applied, but {} failed afterwards: {e}. The cuts so far                              are in place and undoable; run review_flow to check the result.",
                            $what
                        ),
                        actions,
                        before_s,
                        after_s,
                        coherent: false,
                        issues_remaining: 0,
                    });
                }
            }
        };
    }

    // 2b. Still over a stated target? Close the gap with the duration planner.
    if let Some(target) = target_s {
        let now = or_partial!(editor.get_timeline(project_id), "reading the timeline")
            .included_duration();
        if now > target * 1.08 {
            narrate(
                editor,
                project_id,
                3,
                format!("still {:.0}s over target — trimming least-central material", now - target),
            );
            let plan =
                or_partial!(editor.plan_duration_cut(project_id, target), "the duration planner");
            if !plan.segment_ids.is_empty() {
                let outcome = or_partial!(
                    editor.apply_edit(
                        project_id,
                        EditOp::CutSegments { segment_ids: plan.segment_ids },
                        source,
                    ),
                    "applying the duration trim"
                );
                actions.push(outcome.action);
            }
        }
    }

    // 3+4. Review → repair → RE-REVIEW, bounded: a single pass used to leave
    // coherent=false with issues outstanding. Loop until the flow is clean or
    // a round makes no progress (restoring connective material can expose a
    // NEW boundary, so re-review is required), capped so it always terminates.
    const MAX_REVIEW_ROUNDS: usize = 3;
    let speech = speech_segments(editor, project_id)?;
    let mut coherent;
    let mut issues_remaining;
    let mut prev_issue_count = usize::MAX;
    let mut round = 0;
    loop {
        let label = if round == 0 {
            "re-reading the edited transcript at every cut point…".to_string()
        } else {
            format!("re-checking the flow (round {})…", round + 1)
        };
        narrate(editor, project_id, 4, label);
        let snapshot_s = or_partial!(editor.get_timeline(project_id), "reading the timeline")
            .included_duration();
        let review = or_partial!(review_flow(editor, project_id).await, "the flow review");
        coherent = review.coherent;
        issues_remaining = review.issues.len();

        // Clean, or out of rounds, or not improving → stop.
        if coherent
            || round + 1 >= MAX_REVIEW_ROUNDS
            || review.issues.len() >= prev_issue_count
        {
            break;
        }
        prev_issue_count = review.issues.len();

        // Restore ONLY material THIS run cut (review walks every boundary,
        // including the rough cut's — not ours to undo), enough to finish a
        // sliced sentence (≤4 own-cut segments, stop at terminal punctuation).
        let own_cuts: std::collections::HashSet<Uuid> = actions
            .iter()
            .filter_map(|a| match &a.op {
                EditOp::CutSegments { segment_ids } => Some(segment_ids.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        let mut restore_ids: Vec<Uuid> = review
            .issues
            .iter()
            .filter(|i| i.severity == "high")
            .flat_map(|i| {
                let mut taken = vec![];
                for id in i.restore_segment_ids.iter().filter(|id| own_cuts.contains(id)) {
                    taken.push(*id);
                    if taken.len() >= 4 {
                        break;
                    }
                    let done = speech
                        .iter()
                        .find(|s| s.id == *id)
                        .map(|s| s.text.trim_end().ends_with(['.', '!', '?', '…']))
                        .unwrap_or(false);
                    if done {
                        break;
                    }
                }
                taken
            })
            .collect();
        restore_ids.sort_unstable();
        restore_ids.dedup();
        if restore_ids.is_empty() {
            break; // issues remain but none are ours to repair
        }
        // If the timeline moved under us (user undo, another caller) the plan
        // is stale — stop rather than "repairing" a timeline that's gone.
        let now_s = or_partial!(editor.get_timeline(project_id), "reading the timeline")
            .included_duration();
        if (now_s - snapshot_s).abs() > 0.05 {
            narrate(editor, project_id, 5, "the timeline changed during review — stopping auto-repair");
            coherent = false;
            break;
        }
        narrate(
            editor,
            project_id,
            5,
            format!("flow broke at {} point(s) — restoring the connective material", restore_ids.len()),
        );
        let outcome = or_partial!(
            editor.apply_edit(
                project_id,
                EditOp::RestoreSegments { segment_ids: restore_ids },
                source,
            ),
            "restoring the flow-repair segments"
        );
        actions.push(outcome.action);
        round += 1;
    }

    let after_s = or_partial!(editor.get_timeline(project_id), "reading the timeline")
        .included_duration();
    let kept: Vec<&str> = outline
        .beats
        .iter()
        .enumerate()
        .filter(|(n, _)| !valid.contains(n))
        .map(|(_, b)| b.title.as_str())
        .take(6)
        .collect();
    let target_note = match target_s {
        Some(t) if after_s > t * 1.08 => {
            format!(" Note: flow repairs left it {:.0}s over the target.", after_s - t)
        }
        _ => String::new(),
    };
    let summary = format!(
        "{}{} {:.1} min → {:.1} min. Kept: {}{}. Flow check: {}.{target_note}",
        if plan.rationale.is_empty() { String::new() } else { format!("{} ", plan.rationale) },
        if valid.is_empty() { "No whole beats cut;".to_string() } else { format!("Cut {} beat(s);", valid.len()) },
        before_s / 60.0,
        after_s / 60.0,
        kept.join(" · "),
        if outline.beats.len() > 6 { " …" } else { "" },
        if coherent { "coherent".to_string() } else { format!("{issues_remaining} issue(s) flagged — see review_flow") },
    );
    narrate(editor, project_id, 6, summary.clone());

    Ok(StoryEditOutcome { actions, summary, before_s, after_s, coherent, issues_remaining })
}

/// Lean JSON for tool results (beats without the id arrays are useless to
/// orchestrators, so ids stay — but the agent loop truncation protects chat).
pub fn outline_to_value(o: &Outline) -> Value {
    serde_json::to_value(o).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    /// The guardrail arithmetic the review caught failing open: cutting ALL
    /// beats refuses at ANY outline size; all-but-one refuses only for 3+.
    fn refuse(valid: &[usize], beats: usize) -> bool {
        let mut v = valid.to_vec();
        v.sort_unstable();
        v.dedup();
        let all_but_one = v.len() >= beats.saturating_sub(1);
        (v.len() >= beats) || (all_but_one && beats > 2)
    }

    #[test]
    fn guardrail_covers_small_outlines() {
        assert!(refuse(&[0], 1), "gutting a 1-beat outline must refuse");
        assert!(refuse(&[0, 1], 2), "gutting a 2-beat outline must refuse");
        assert!(!refuse(&[0], 2), "cutting 1 of 2 beats is allowed");
        assert!(refuse(&[0, 1, 2], 3));
        assert!(refuse(&[0, 1], 3), "all-but-one refuses for 3+");
        assert!(!refuse(&[1], 3));
    }

    #[test]
    fn guardrail_dedupes_repeated_indexes() {
        // A weak model emitting [2,2,2] targets ONE beat — must not refuse.
        assert!(!refuse(&[2, 2, 2], 4));
        assert!(!refuse(&[0, 0], 3));
    }
}
