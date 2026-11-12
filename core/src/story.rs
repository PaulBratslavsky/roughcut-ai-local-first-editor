//! Story-level editing: outline the narrative, plan cuts against the
//! outline, verify the result still FLOWS, repair. Each step is a tool
//! (composable by frontier orchestrators over MCP); [`story_edit`] composes
//! them for the in-app chat, narrating every phase.
//!
//! Design rule learned the hard way: the local model never orchestrates a
//! long flow — the pipeline is deterministic scaffolding with the LLM
//! embedded inside each step, strict JSON contracts, index-based references
//! (small models mangle uuids), and non-LLM fallbacks everywhere.

use crate::applog;
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
    outline_using(editor, project_id, refresh, None).await
}

/// [`outline_with`] on a given client — a story edit passes its own, so the
/// whole edit runs on one model.
pub(crate) async fn outline_using(
    editor: &Editor,
    project_id: Uuid,
    refresh: bool,
    client: Option<&dyn crate::adapters::InferenceClient>,
) -> Result<Outline> {
    let speech = speech_segments(editor, project_id)?;
    if speech.is_empty() {
        return Err(CoreError::InvalidArg("transcript has no speech".into()));
    }
    let hash = transcript_hash(&speech);
    let cache_key = format!("outline:{project_id}");
    let prefs = editor.get_preferences()?;
    let mut step = applog::Step::with_mode("outline", Some(project_id), prefs.model_mode);
    // A cached outline counts only if the model that would run now made it:
    // switching to Hosted must not keep the local model's beats.
    let expected_model = match prefs.model_mode {
        crate::model::ModelMode::Hosted => prefs.hosted_model.clone(),
        crate::model::ModelMode::Local => prefs.inference_model.clone(),
    };
    if let Ok(Some(raw)) = editor.store().get_kv(&cache_key) {
        if !refresh {
            let cached = serde_json::from_str::<(String, Outline, String)>(&raw).ok().or_else(|| {
                // Written before Hosted mode existed: by the local model.
                serde_json::from_str::<(String, Outline)>(&raw).ok().map(|(h, o)| (h, o, prefs.inference_model.clone()))
            });
            if let Some((cached_hash, outline, model)) = cached {
                if cached_hash == hash && model == expected_model {
                    step.method("cache").model(&model).count("beats", outline.beats.len());
                    step.ok();
                    return Ok(outline);
                }
            }
        }
    }

    let owned;
    let inference: &dyn crate::adapters::InferenceClient = match client {
        Some(c) => c,
        None => {
            owned = editor.inference()?;
            owned.as_ref()
        }
    };
    let mut made_by: Option<String> = None;
    let heuristic = || Outline { beats: heuristic_beats(&speech), method: "heuristic".into() };
    // Each fallback says why in the app log: the three used to look the same.
    let outline = if inference.healthy().await {
        match llm_outline(&speech, inference, &prefs.inference_model).await {
            Ok((beats, served)) if beats.len() >= 2 => {
                // One model for every chunk, or nobody (a run that fell back
                // part-way isn't cached).
                let first = served.first().cloned().flatten();
                if served.iter().all(|s| s.as_ref().map(|s| &s.model) == first.as_ref().map(|s| &s.model)) {
                    made_by = first.map(|s| s.model);
                }
                step.method("llm").count("beats", beats.len());
                for s in &served {
                    step.served(s.as_ref());
                }
                step.ok();
                Outline { beats, method: "llm".into() }
            }
            Ok((beats, _)) => {
                step.count("beats", beats.len());
                step.fallback("too_few_beats", None);
                heuristic()
            }
            Err(e) if e.is_fatal_model_error() => {
                step.failed(&e);
                return Err(e);
            }
            Err(e) => {
                step.fallback(e.code(), Some(&e));
                heuristic()
            }
        }
    } else {
        step.fallback("model_unreachable", None);
        heuristic()
    };

    // Only LLM outlines are worth caching; the heuristic is cheap and
    // deterministic, and caching it would shadow the LLM forever after one
    // model outage (the cache key is the transcript hash, which edits never
    // change).
    if outline.method == "llm" && made_by.as_deref() == Some(expected_model.as_str()) {
        let _ = editor
            .store()
            .set_kv(&cache_key, &serde_json::to_string(&(hash, outline.clone(), expected_model))?);
    }
    Ok(outline)
}

/// ~120 segments per chunk keeps the prompt well inside a local model's
/// comfort zone; absolute indexes survive the chunking.
async fn llm_outline(
    speech: &[TranscriptSegment],
    inference: &dyn crate::adapters::InferenceClient,
    model: &str,
) -> Result<(Vec<Beat>, Vec<Option<crate::adapters::inference::ServedBy>>)> {
    const CHUNK: usize = 120;
    let mut beats = vec![];
    let mut served = vec![];
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
        let beat_schema = beats_schema();
        let (mut raw, by): (Vec<RawBeat>, _) = crate::llm::ask_json_schema_served(
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
        served.push(by);
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
    Ok((merged, served))
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

/// One cut point as the viewer hears it: the kept speech on each side of a
/// run of removed speech, read word by word (see [`flow_seams`]).
struct Boundary {
    /// Kept speech just before the cut, ending at its last kept word.
    before: String,
    /// The last kept word ends a sentence.
    before_ends_sentence: bool,
    /// The segment holding the last kept word before the cut.
    before_segment: Uuid,
    /// Kept speech just after the cut, from its first kept word.
    after: String,
    /// The segment holding the first kept word after the cut.
    after_segment: Uuid,
    /// The start of the removed speech, and how many segments it touches.
    cut: String,
    cut_segments: usize,
    /// The segments to restore to finish the sentence the cut broke.
    repair: Vec<Uuid>,
}

/// A word counts as kept when it sits inside an included clip, give or take
/// this much: clip edges are frame-snapped, words carry their own start/end.
const KEEP_TOLERANCE_S: f64 = 0.05;
/// About two segments of context on each side of a cut, for the model.
const CONTEXT_CHARS: usize = 240;
/// A repair restores at most this many segments: the sentence the cut broke,
/// never the whole cut section.
const MAX_REPAIR_SEGMENTS: usize = 3;

fn ends_sentence(text: &str) -> bool {
    text.trim_end().ends_with(['.', '!', '?', '…'])
}

/// Lowercase letters and digits only, as `detect` compares filler words.
fn normalize_word(w: &str) -> String {
    w.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect()
}

/// One spoken unit of the seam walk: a word, or a whole segment when the
/// segment has no word timings (the segment-level check).
struct Token {
    /// Index into the speech segments.
    seg: usize,
    /// Its share of the segment's text, with the punctuation and space after
    /// it: one segment's tokens concatenate back to its text.
    text: String,
    /// Its words, normalized (one for a word token).
    norm: Vec<String>,
    kept: bool,
    /// A filler ("um") or part of a filler segment: cutting it never breaks a
    /// sentence, and a kept one is looked past at a seam.
    filler: bool,
    /// A sentence ends after it.
    ends: bool,
}

/// Each word's share of the segment `text`: from its first letter (the
/// start of the text, for the first word) to the next word's first letter
/// (the end of the text, for the last word). Words are matched to the text
/// by counting letters and digits, which survives the cleanup pass adding
/// punctuation and capitals to `text` while the words stay raw ("ai" in
/// "use AI. Don't"); when cleanup changed the spelling the count is scaled,
/// so a share lands near its word. None when either side has no letters.
fn word_shares(text: &str, words: &[crate::model::Word]) -> Option<Vec<std::ops::Range<usize>>> {
    let letters: Vec<usize> =
        text.char_indices().filter(|(_, c)| c.is_alphanumeric()).map(|(i, _)| i).collect();
    let counts: Vec<usize> =
        words.iter().map(|w| w.text.chars().filter(|c| c.is_alphanumeric()).count()).collect();
    let total: usize = counts.iter().sum();
    if letters.is_empty() || total == 0 {
        return None;
    }
    let mut starts: Vec<usize> = Vec::with_capacity(words.len());
    let mut before = 0;
    for (k, n) in counts.iter().enumerate() {
        starts.push(if k == 0 {
            0
        } else {
            let at = (before * letters.len() + total / 2) / total;
            let mut start = letters.get(at).copied().unwrap_or(text.len());
            // A token that opens with an apostrophe ("'t") owns it in the text.
            let lead = words[k].text.chars().take_while(|c| !c.is_alphanumeric()).count();
            for _ in 0..lead {
                match text[..start].chars().next_back() {
                    Some(c) if !c.is_alphanumeric() && !c.is_whitespace() => start -= c.len_utf8(),
                    _ => break,
                }
            }
            start.max(starts[k - 1])
        });
        before += n;
    }
    Some((0..words.len()).map(|k| starts[k]..starts.get(k + 1).copied().unwrap_or(text.len())).collect())
}

/// The included clips as merged source spans (a split leaves two touching
/// included clips; a word across it still plays).
fn included_spans(timeline: &crate::model::Timeline) -> Vec<(f64, f64)> {
    let mut spans: Vec<(f64, f64)> = vec![];
    for c in timeline.included_clips() {
        match spans.last_mut() {
            Some(last) if c.source_in - last.1 < 1e-6 => last.1 = last.1.max(c.source_out),
            _ => spans.push((c.source_in, c.source_out)),
        }
    }
    spans
}

/// A word plays when it lies inside one included span, within
/// [`KEEP_TOLERANCE_S`], and its middle is inside it: a word the cut clips
/// by more than the tolerance, or mostly outside, is not kept.
fn word_kept(start: f64, end: f64, spans: &[(f64, f64)]) -> bool {
    let mid = (start + end) / 2.0;
    spans.iter().any(|&(a, b)| {
        start >= a - KEEP_TOLERANCE_S
            && end <= b + KEEP_TOLERANCE_S
            && mid >= a - 1e-6
            && mid <= b + 1e-6
    })
}

fn flow_tokens(
    speech: &[TranscriptSegment],
    timeline: &crate::model::Timeline,
    fillers: &[String],
) -> Vec<Token> {
    let spans = included_spans(timeline);
    let mut out = vec![];
    for (si, seg) in speech.iter().enumerate() {
        let shares = if seg.words.is_empty() { None } else { word_shares(&seg.text, &seg.words) };
        if let Some(shares) = shares {
            for (w, share) in seg.words.iter().zip(shares) {
                let norm = normalize_word(&w.text);
                if norm.is_empty() {
                    continue; // punctuation: its share is empty, the word before holds it
                }
                let text = seg.text[share].to_string();
                out.push(Token {
                    seg: si,
                    ends: ends_sentence(&text) || ends_sentence(&w.text),
                    filler: seg.is_filler || fillers.contains(&norm),
                    kept: word_kept(w.start, w.end, &spans),
                    norm: vec![norm],
                    text,
                });
            }
        } else {
            // No word timings: the whole segment, kept when most of it plays.
            let overlap: f64 =
                spans.iter().map(|&(a, b)| (seg.end.min(b) - seg.start.max(a)).max(0.0)).sum();
            let norm: Vec<String> =
                seg.text.split_whitespace().map(normalize_word).filter(|w| !w.is_empty()).collect();
            out.push(Token {
                seg: si,
                ends: ends_sentence(&seg.text),
                filler: seg.is_filler || (!norm.is_empty() && norm.iter().all(|w| fillers.contains(w))),
                kept: overlap > (seg.end - seg.start) * 0.5,
                norm,
                text: seg.text.clone(),
            });
        }
    }
    out
}

/// The text of `idx` (ascending token indexes), spaced where tokens aren't
/// neighbors in one segment.
fn tokens_text(tokens: &[Token], idx: &[usize]) -> String {
    let mut out = String::new();
    let mut last: Option<usize> = None;
    for &k in idx {
        if let Some(l) = last {
            let adjacent = l + 1 == k && tokens[l].seg == tokens[k].seg;
            if !adjacent && !out.is_empty() && !out.ends_with(char::is_whitespace) {
                out.push(' ');
            }
        }
        out.push_str(&tokens[k].text);
        last = Some(k);
    }
    out.trim().to_string()
}

/// The removed words say again what's kept right beside them ("the [the]
/// store", "[we should] we should go"): cutting a repeat reads cleanly.
fn is_repeat(removed: &[&str], tokens: &[Token], last_before: usize, first_after: usize) -> bool {
    let spoken = |t: &&Token| t.kept && !t.filler;
    let after: Vec<&str> = tokens[first_after..]
        .iter()
        .filter(spoken)
        .flat_map(|t| t.norm.iter().map(String::as_str))
        .take(removed.len())
        .collect();
    let mut before: Vec<&str> = tokens[..=last_before]
        .iter()
        .rev()
        .filter(spoken)
        .flat_map(|t| t.norm.iter().rev().map(String::as_str))
        .take(removed.len())
        .collect();
    before.reverse();
    after == removed || before == removed
}

/// Walk the cut points of `timeline` word by word: a seam is a run of
/// removed words between two kept ones, so a cut inside a segment reads the
/// words that actually play on each side. Segments without word timings are
/// one unit, kept when most of them play. Fillers are looked past, and a
/// run that removes only fillers or a repeat is not a seam.
fn flow_seams(
    speech: &[TranscriptSegment],
    timeline: &crate::model::Timeline,
    fillers: &[String],
) -> Vec<Boundary> {
    let tokens = flow_tokens(speech, timeline, fillers);
    let mut out = vec![];
    let mut prev: Option<usize> = None; // the last kept, non-filler token
    for (j, t) in tokens.iter().enumerate() {
        if !t.kept || t.filler {
            continue;
        }
        if let Some(i) = prev {
            let removed: Vec<usize> = (i + 1..j).filter(|&k| !tokens[k].kept).collect();
            let spoken: Vec<&str> = removed
                .iter()
                .filter(|&&k| !tokens[k].filler)
                .flat_map(|&k| tokens[k].norm.iter().map(String::as_str))
                .collect();
            if !spoken.is_empty() && !is_repeat(&spoken, &tokens, i, j) {
                out.push(seam(speech, &tokens, i, j, &removed));
            }
        }
        prev = Some(j);
    }
    out
}

/// The seam between kept tokens `i` and `j`, with `removed` between them.
fn seam(speech: &[TranscriptSegment], tokens: &[Token], i: usize, j: usize, removed: &[usize]) -> Boundary {
    let context = |range: &mut dyn Iterator<Item = usize>| -> Vec<usize> {
        let mut taken = vec![];
        let mut chars = 0;
        for k in range {
            if tokens[k].kept {
                taken.push(k);
                chars += tokens[k].text.len();
                if chars >= CONTEXT_CHARS {
                    break;
                }
            }
        }
        taken
    };
    let mut back = context(&mut (0..=i).rev());
    back.reverse();
    let ahead = context(&mut (j..tokens.len()));

    let mut cut_segments: Vec<usize> = removed.iter().map(|&k| tokens[k].seg).collect();
    cut_segments.dedup(); // tokens run in segment order
    // Restore from the first removed word to the one that ends the sentence:
    // the segments that finish what the cut broke, a few at most.
    let mut repair: Vec<Uuid> = vec![];
    for &k in removed {
        let id = speech[tokens[k].seg].id;
        if !repair.contains(&id) {
            if repair.len() == MAX_REPAIR_SEGMENTS {
                break;
            }
            repair.push(id);
        }
        if !tokens[k].filler && tokens[k].ends {
            break;
        }
    }
    Boundary {
        before: tokens_text(tokens, &back),
        before_ends_sentence: tokens[i].ends,
        before_segment: speech[tokens[i].seg].id,
        after: tokens_text(tokens, &ahead),
        after_segment: speech[tokens[j].seg].id,
        cut: tokens_text(tokens, &removed[..removed.len().min(24)]),
        cut_segments: cut_segments.len(),
        repair,
    }
}

/// The deterministic checks at each seam: the kept speech before it stops
/// mid-sentence, or the speech after it resumes on a connective or a
/// dangling reference.
fn seam_issues(boundaries: &[Boundary]) -> Vec<FlowIssue> {
    const CONNECTIVES: [&str; 8] =
        ["so ", "and ", "but ", "because ", "which ", "that's why", "like i said", "as i "];
    let mut issues = vec![];
    for b in boundaries {
        if !b.before_ends_sentence {
            issues.push(FlowIssue {
                kind: "broken_sentence".into(),
                severity: "high".into(),
                at_segment: b.before_segment,
                description: format!("cut lands mid-thought after “…{}”", tail(&b.before, 50)),
                restore_segment_ids: b.repair.clone(),
            });
        }
        let lower = b.after.to_lowercase();
        if CONNECTIVES.iter().any(|c| lower.starts_with(c)) {
            issues.push(FlowIssue {
                kind: "abrupt_start".into(),
                severity: "medium".into(),
                at_segment: b.after_segment,
                description: format!(
                    "after the cut, speech resumes mid-argument: “{}…”",
                    head(&b.after, 60)
                ),
                restore_segment_ids: vec![],
            });
        }
    }
    issues
}

/// Snap a cut range [start, end] to clean WORD or SENTENCE boundaries using
/// the transcript's word-level timings, so a cut never clips mid-word. The
/// kept content BEFORE the cut ends at a word/sentence end (we snap `start`),
/// and the kept content AFTER resumes at a word/sentence start (we snap `end`).
/// `mode`: "word" | "sentence" | "none". Falls back to the original range if
/// snapping would invert/collapse it or there are no word timings — this is the
/// deterministic boundary math an orchestrator would otherwise hand-roll.
pub fn snap_cut_range(
    editor: &Editor,
    project_id: Uuid,
    start: f64,
    end: f64,
    mode: &str,
) -> Result<(f64, f64)> {
    if !matches!(mode, "word" | "sentence") {
        return Ok((start, end)); // "none"/unknown: leave the range as given
    }
    let speech = speech_segments(editor, project_id)?;
    let sentence = mode == "sentence";
    let ends_sentence = |t: &str| t.trim_end().ends_with(['.', '!', '?', '…']);

    // `start` lands on a word END (clean end of kept-before); `end` lands on a
    // word START (clean start of kept-after). In sentence mode, restrict the
    // candidates to sentence edges.
    let mut start_cands: Vec<f64> = vec![];
    let mut end_cands: Vec<f64> = vec![];
    let mut prev_ended = true; // the first word begins a "sentence"
    for seg in &speech {
        for w in &seg.words {
            if !sentence || ends_sentence(&w.text) {
                start_cands.push(w.end);
            }
            if !sentence || prev_ended {
                end_cands.push(w.start);
            }
            prev_ended = ends_sentence(&w.text);
        }
    }

    let nearest = |cands: &[f64], t: f64| -> Option<f64> {
        cands.iter().copied().min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs()))
    };
    let s = nearest(&start_cands, start).unwrap_or(start);
    let e = nearest(&end_cands, end).unwrap_or(end);
    if s >= e {
        return Ok((start, end)); // snapping collapsed/inverted — keep original
    }
    Ok((s, e))
}

/// How far `snap_passage` may move an edge to find a clean one.
const PASSAGE_SNAP_S: f64 = 10.0;

/// Snap the edges of a PASSAGE cut so both sides of it read cleanly: the
/// kept speech before ends at a sentence end (or a clear pause), and the kept
/// speech after starts a sentence. Unlike `snap_cut_range`'s sentence mode,
/// an edge moves at most PASSAGE_SNAP_S seconds: in a long unpunctuated
/// stretch the nearest "sentence" can be minutes away, so a pause, then a
/// plain word edge, is the fallback. Returns the snapped `(start, end)`.
pub fn snap_passage(editor: &Editor, project_id: Uuid, start: f64, end: f64) -> Result<(f64, f64)> {
    let speech = speech_segments(editor, project_id)?;
    let words: Vec<&crate::model::Word> = speech.iter().flat_map(|s| s.words.iter()).collect();
    if words.is_empty() || end <= start {
        return Ok((start, end));
    }
    let ends_sentence = |t: &str| t.trim_end().ends_with(['.', '!', '?', '…']);
    // Cost of an edge: distance moved plus a penalty for how clean it is.
    let pick = |target: f64, cands: &mut dyn Iterator<Item = (f64, f64)>| -> f64 {
        cands
            .filter(|(t, _)| (t - target).abs() <= PASSAGE_SNAP_S)
            .min_by(|a, b| ((a.0 - target).abs() + a.1).total_cmp(&((b.0 - target).abs() + b.1)))
            .map(|(t, _)| t)
            .unwrap_or(target)
    };
    // Cut start = the END of the last kept word before the passage.
    let s = pick(
        start,
        &mut words.iter().enumerate().map(|(i, w)| {
            let pause = words.get(i + 1).map(|n| n.start - w.end).unwrap_or(9.0);
            let penalty = if ends_sentence(&w.text) { 0.0 } else if pause >= 0.5 { 2.0 } else { 6.0 };
            (w.end, penalty)
        }),
    );
    // Cut end = the START of the first kept word after the passage.
    let e = pick(
        end,
        &mut words.iter().enumerate().map(|(i, w)| {
            let prev = if i > 0 { Some(words[i - 1]) } else { None };
            let pause = prev.map(|p| w.start - p.end).unwrap_or(9.0);
            let starts = prev.map(|p| ends_sentence(&p.text)).unwrap_or(true);
            let penalty = if starts { 0.0 } else if pause >= 0.5 { 2.0 } else { 6.0 };
            (w.start, penalty)
        }),
    );
    if s >= e {
        return Ok((start, end));
    }
    Ok((s, e))
}

pub async fn review_flow(editor: &Editor, project_id: Uuid) -> Result<FlowReview> {
    review_flow_mode(editor, project_id, true).await
}

/// `llm=false` runs ONLY the cheap deterministic boundary checks (no model
/// call) — the lightweight advisory tier for lower-risk edits.
pub async fn review_flow_mode(editor: &Editor, project_id: Uuid, llm: bool) -> Result<FlowReview> {
    review_flow_using(editor, project_id, llm, None).await
}

/// [`review_flow_mode`] on a given client (a story edit passes its own).
pub(crate) async fn review_flow_using(
    editor: &Editor,
    project_id: Uuid,
    llm: bool,
    client: Option<&dyn crate::adapters::InferenceClient>,
) -> Result<FlowReview> {
    let speech = speech_segments(editor, project_id)?;
    // The user's own filler words count as fillers too (as in `detect`).
    let mut fillers: Vec<String> =
        crate::detect::DEFAULT_FILLERS.iter().map(|s| s.to_string()).collect();
    if let Ok(prefs) = editor.get_preferences() {
        fillers.extend(prefs.custom_filler_words.iter().map(|w| normalize_word(w)));
    }
    let boundaries =
        editor.with_project(project_id, |p, _| Ok(flow_seams(&speech, &p.timeline, &fillers)))?;

    // Deterministic pass — cheap, always on.
    let mut issues: Vec<FlowIssue> = seam_issues(&boundaries);

    // LLM pass — judges each boundary in context; capped to keep one call.
    // The client is only made for it: the default deterministic review must
    // work whatever the model settings are.
    let mut method = "deterministic".to_string();
    const MAX_BOUNDARIES: usize = 40;
    let owned;
    let inference: Option<&dyn crate::adapters::InferenceClient> = if llm && !boundaries.is_empty() {
        match client {
            Some(c) => Some(c),
            None => {
                owned = editor.inference()?;
                Some(owned.as_ref())
            }
        }
    } else {
        None
    };
    if let Some(inference) = inference {
        let prefs = editor.get_preferences()?;
        let mut step = applog::Step::with_mode("flow_review", Some(project_id), prefs.model_mode);
        step.count("boundaries", boundaries.len());
        if !inference.healthy().await {
            step.fallback("model_unreachable", None);
            let coherent = !issues.iter().any(|i| i.severity == "high");
            return Ok(FlowReview { coherent, boundaries_checked: boundaries.len(), issues, method });
        }
        method = "llm".into();
        let listed: String = boundaries
            .iter()
            .take(MAX_BOUNDARIES)
            .enumerate()
            .map(|(n, b)| {
                format!(
                    "BOUNDARY {n}\nbefore: {}\n[CUT: {} segment(s), e.g. “{}”]\nafter: {}",
                    b.before,
                    b.cut_segments,
                    head(&b.cut, 70),
                    b.after,
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
        let verdict_schema = verdicts_schema();
        let verdicts: Result<(Vec<Verdict>, _)> = crate::llm::ask_json_schema_served(
            inference,
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
        match &verdicts {
            Ok((_, served)) => {
                step.served(served.as_ref());
                step.ok();
            }
            Err(e) if e.is_fatal_model_error() => {
                step.failed(e);
                return Err(verdicts.err().unwrap());
            }
            // An unusable reply: the rule-based findings stand.
            Err(e) => step.fallback(e.code(), Some(e)),
        }
        {
            {
                if let Ok((verdicts, _)) = verdicts {
                    for v in verdicts {
                        if v.ok {
                            continue;
                        }
                        let Some(b) = boundaries.get(v.b) else { continue };
                        issues.push(FlowIssue {
                            kind: "flow".into(),
                            severity: "high".into(),
                            at_segment: b.after_segment,
                            description: if v.why.is_empty() {
                                "the model judged this cut point unnatural".into()
                            } else {
                                v.why
                            },
                            restore_segment_ids: if v.restore {
                                b.repair.clone()
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
    let text = text.into();
    crate::chatlog::record(project_id, step, "thinking", None, None, None, Some(&text));
    send(
        &editor.sink(),
        CoreEvent::AgentStep {
            project_id,
            step,
            kind: "thinking".into(),
            tool: None,
            args: None,
            result: None,
            text: Some(text),
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
    // One `story_plan` line in the app log for the whole edit.
    let mut step = applog::Step::start(editor, "story_plan", Some(project_id));
    let result = story_edit_logged(editor, project_id, instruction, target_s, source, &mut step).await;
    if let Ok(outcome) = &result {
        step.count("actions", outcome.actions.len());
    }
    step.finish(&result);
    result
}

async fn story_edit_logged(
    editor: &Editor,
    project_id: Uuid,
    instruction: &str,
    target_s: Option<f64>,
    source: ActionSource,
    step: &mut applog::Step,
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
    // A conversation: its plan and checks build on each other, so it never
    // switches models half-way.
    let inference = editor.inference_conversation()?;
    if !inference.healthy().await {
        // Honest degradation: without a model there is no narrative judgment.
        if let Some(target) = target_s {
            step.mark_fallback("model_unreachable", None);
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

    // 1. Outline.
    narrate(editor, project_id, 0, "reading the full transcript and outlining the story…");
    // On the edit's own client: the whole story edit is one model.
    let outline = outline_using(editor, project_id, false, Some(inference.as_ref())).await?;
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
    let plan_schema = story_plan_schema();
    let (plan, served): (Plan, _) = crate::llm::ask_json_schema_served(
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
    step.served(served.as_ref());
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
    step.count("cut_beats", valid.len());
    let cut_duration: f64 =
        valid.iter().map(|&n| outline.beats[n].end - outline.beats[n].start).sum();
    let justified = target_s.map(|t| before_s - cut_duration >= t * 0.5).unwrap_or(false);
    if before_s > 0.0 && cut_duration > before_s * 0.8 && !justified {
        return Err(CoreError::Inference(
            "the plan removes over 80% of the video with no target to justify it — refusing"
                .into(),
        ));
    }

    // Nothing has been changed until here: the outline and plan read the
    // transcript, not the timeline, so a model error or a refused plan above
    // leaves the video exactly as it was.
    //
    // Dead air: hybrid-CONFIRMED silences are uncontroversial. Shared with
    // `remove_silences` — same code, can't drift.
    let silence_actions = editor.cut_confirmed_silences(project_id, source).await?;
    if !silence_actions.is_empty() {
        narrate(
            editor,
            project_id,
            2,
            format!("cutting {} audio-confirmed dead-air range(s)", silence_actions.len()),
        );
        actions.extend(silence_actions);
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
                    step.mark_failed(&e).mark_partial();
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
        let review = or_partial!(
            review_flow_using(editor, project_id, true, Some(inference.as_ref())).await,
            "the flow review"
        );
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

/// The outline: one object per beat.
pub(crate) fn beats_schema() -> serde_json::Value {
    serde_json::json!({
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
    })
}

/// Flow review: one verdict per numbered cut point.
pub(crate) fn verdicts_schema() -> serde_json::Value {
    serde_json::json!({
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
    })
}

/// Story edit: which beats to cut, and why.
pub(crate) fn story_plan_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "cut_beats": {"type": "array", "items": {"type": "integer"}},
            "rationale": {"type": "string"}
        },
        "required": ["cut_beats"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cached outline is reused only when the model that would run now
    /// made it; one cached before Hosted mode existed counts as the local
    /// model's.
    #[tokio::test]
    async fn the_outline_cache_belongs_to_the_model_that_made_it() {
        let e = Editor::test_instance();
        let p = e.create_project("t", Some("/demo/footage.mp4")).await.unwrap();
        e.transcribe(p.id, None).await.unwrap();
        let speech = speech_segments(&e, p.id).unwrap();
        let hash = transcript_hash(&speech);
        let llm = Outline { beats: heuristic_beats(&speech), method: "llm".into() };
        let key = format!("outline:{}", p.id);
        let local_model = e.get_preferences().unwrap().inference_model;

        // Made by Claude: not reused in Local mode (no model in tests, so a
        // fresh outline is the heuristic one).
        e.store().set_kv(&key, &serde_json::to_string(&(&hash, &llm, "claude-opus-5-5")).unwrap()).unwrap();
        assert_eq!(outline(&e, p.id).await.unwrap().method, "heuristic");

        // Made by the local model: reused.
        e.store().set_kv(&key, &serde_json::to_string(&(&hash, &llm, &local_model)).unwrap()).unwrap();
        assert_eq!(outline(&e, p.id).await.unwrap().method, "llm");

        // The old two-part entry: the local model's.
        e.store().set_kv(&key, &serde_json::to_string(&(&hash, &llm)).unwrap()).unwrap();
        assert_eq!(outline(&e, p.id).await.unwrap().method, "llm");
    }
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

    // ---- flow review seams, word by word

    use crate::model::{ClipOrigin, Timeline, Word};

    /// Speech laid end to end, 0.31 s a word. Each line is (segment text,
    /// raw words): the text is punctuated and capitalized as after cleanup,
    /// the words stay raw whisper tokens ("didn 't", lowercase, no
    /// punctuation) — the real transcripts look like this.
    fn speech(lines: &[(&str, &str)]) -> Vec<TranscriptSegment> {
        let mut t = 1.0;
        lines
            .iter()
            .map(|(text, raw)| {
                let words: Vec<Word> = raw
                    .split_whitespace()
                    .map(|w| {
                        let word = Word { text: w.into(), start: t, end: t + 0.31, confidence: 0.9 };
                        t += 0.31;
                        word
                    })
                    .collect();
                TranscriptSegment {
                    id: Uuid::new_v4(),
                    start: words.first().map(|w| w.start).unwrap_or(t),
                    end: words.last().map(|w| w.end).unwrap_or(t),
                    text: (*text).into(),
                    words,
                    is_filler: false,
                    is_silence: false,
                    take_group_id: None,
                    is_best_take: false,
                }
            })
            .collect()
    }

    /// The `n`th word `text` of a segment.
    fn word<'a>(s: &'a TranscriptSegment, text: &str, n: usize) -> &'a Word {
        s.words.iter().filter(|w| w.text == text).nth(n).expect("word in segment")
    }

    fn timeline(speech: &[TranscriptSegment]) -> Timeline {
        Timeline::new(speech.last().unwrap().end + 1.0)
    }

    /// Cut `[from, to]` with frame-snapped edges (24 fps), as the range ops do.
    fn cut(tl: &mut Timeline, from: f64, to: f64) {
        let snap = |t| crate::time::snap_to_frame(t, 24.0);
        tl.set_range_included(snap(from), snap(to), false, ClipOrigin::Manual);
    }

    fn review(speech: &[TranscriptSegment], tl: &Timeline) -> (Vec<Boundary>, Vec<FlowIssue>) {
        let fillers: Vec<String> = crate::detect::DEFAULT_FILLERS.iter().map(|s| s.to_string()).collect();
        let seams = flow_seams(speech, tl, &fillers);
        let issues = seam_issues(&seams);
        (seams, issues)
    }

    /// The two false alarms from a real supercut: each kept segment's FULL
    /// text was read, though the cut sat right after a sentence end inside
    /// it ("…use AI. | Don't make this") or the first word of the next,
    /// mostly cut, segment was kept ("…software | engineer.").
    #[test]
    fn a_cut_after_a_sentence_end_inside_a_segment_is_not_flagged() {
        let s = speech(&[
            (
                "Folks share this story: how I got laid off from X because I didn't use AI. Don't make this",
                "folks share this story how i got laid off from x because i didn 't use ai don 't make this",
            ),
            ("mistake. And the reason I bring this up is simple.", "mistake and the reason i bring this up is simple"),
            ("Here is what I do now.", "here is what i do now"),
            (
                "and your ability to build things is your new value as a software",
                "and your ability to build things is your new value as a software",
            ),
            (
                "engineer. So I spent the weekend on something else entirely.",
                "engineer so i spent the weekend on something else entirely",
            ),
            ("That is the whole point.", "that is the whole point"),
        ]);
        let mut tl = timeline(&s);
        cut(&mut tl, word(&s[0], "ai", 0).end, s[2].start);
        cut(&mut tl, word(&s[4], "engineer", 0).end, s[5].start);

        let (seams, issues) = review(&s, &tl);
        assert_eq!(seams.len(), 2);
        assert!(seams[0].before.ends_with("because I didn't use AI."), "{}", seams[0].before);
        assert!(seams[0].after.starts_with("Here is what I do now."), "{}", seams[0].after);
        assert!(seams[1].before.ends_with("as a software engineer."), "{}", seams[1].before);
        assert!(issues.is_empty(), "clean seams flagged: {issues:?}");
    }

    /// A cut that really ends mid-sentence is flagged at word level — even
    /// inside a segment whose own text ends cleanly ("…right?"), which the
    /// segment-level check could never see — and a kept connective after
    /// the cut still reads as an abrupt start.
    #[test]
    fn a_cut_mid_sentence_is_flagged() {
        let s = speech(&[
            (
                "So the problem is that editing takes forever, right?",
                "so the problem is that editing takes forever right",
            ),
            (
                "You record twenty minutes and then you spend hours cutting.",
                "you record twenty minutes and then you spend hours cutting",
            ),
            ("And here's the key part, you stay in control.", "and here 's the key part you stay in control"),
        ]);
        let mut tl = timeline(&s);
        cut(&mut tl, word(&s[0], "editing", 0).end, s[2].start);

        let (seams, issues) = review(&s, &tl);
        assert_eq!(seams.len(), 1);
        let broken: Vec<&FlowIssue> = issues.iter().filter(|i| i.kind == "broken_sentence").collect();
        assert_eq!(broken.len(), 1, "{issues:?}");
        assert_eq!(broken[0].severity, "high");
        assert_eq!(broken[0].at_segment, s[0].id);
        assert!(broken[0].description.contains("the problem is that editing”"), "{}", broken[0].description);
        assert!(!broken[0].description.contains("forever"), "reads only the kept words");
        let abrupt: Vec<&FlowIssue> = issues.iter().filter(|i| i.kind == "abrupt_start").collect();
        assert_eq!(abrupt.len(), 1, "{issues:?}");
        assert_eq!(abrupt[0].at_segment, s[2].id);
        assert!(abrupt[0].description.contains("And here's the key part"), "{}", abrupt[0].description);
    }

    /// A long cut section that slices a sentence: the suggestion restores the
    /// segments that finish that sentence, not the section (the old check
    /// named every excluded segment — up to 105 of them).
    #[test]
    fn restore_suggestions_stay_local_to_the_seam() {
        let mut lines = vec![
            ("Welcome back.", "welcome back"),
            ("We picked the cheaper plan because the old", "we picked the cheaper plan because the old"),
            ("one cost too much. Then the bill doubled.", "one cost too much then the bill doubled"),
        ];
        for _ in 0..6 {
            lines.push(("A long tangent about something else entirely.", "a long tangent about something else entirely"));
        }
        lines.push(("Here is the fix.", "here is the fix"));
        let s = speech(&lines);
        let mut tl = timeline(&s);
        cut(&mut tl, word(&s[1], "plan", 0).end, s[9].start);

        let (_, issues) = review(&s, &tl);
        let broken: Vec<&FlowIssue> = issues.iter().filter(|i| i.kind == "broken_sentence").collect();
        assert_eq!(broken.len(), 1, "{issues:?}");
        assert_eq!(broken[0].restore_segment_ids, vec![s[1].id, s[2].id]);

        // Unpunctuated speech: nothing marks where the sentence ends, so the
        // suggestion stops after a few segments.
        let raw: Vec<(&str, &str)> = lines.iter().map(|(_, r)| (*r, *r)).collect();
        let s = speech(&raw);
        let mut tl = timeline(&s);
        cut(&mut tl, word(&s[1], "plan", 0).end, s[9].start);
        let (_, issues) = review(&s, &tl);
        let broken: Vec<&FlowIssue> = issues.iter().filter(|i| i.kind == "broken_sentence").collect();
        assert_eq!(broken.len(), 1, "{issues:?}");
        assert_eq!(broken[0].restore_segment_ids, vec![s[1].id, s[2].id, s[3].id]);
    }

    /// Cutting a filler or a repeated word inside a sentence leaves the
    /// sentence whole: no seam to judge.
    #[test]
    fn fillers_and_repeats_cut_inside_a_sentence_are_not_seams() {
        let s = speech(&[
            ("I think, um, we should we should go now.", "i think um we should we should go now"),
            ("The the plan is ready.", "the the plan is ready"),
        ]);
        let mut tl = timeline(&s);
        // "um" and the first "we should"…
        cut(&mut tl, word(&s[0], "um", 0).start, word(&s[0], "should", 0).end);
        // …and the stuttered "the".
        cut(&mut tl, word(&s[1], "the", 0).start, word(&s[1], "the", 0).end);
        let (seams, issues) = review(&s, &tl);
        assert!(seams.is_empty() && issues.is_empty(), "{issues:?}");
    }

    /// Segments without word timings keep the segment-level check: one unit,
    /// kept when most of it plays, its full text read.
    #[test]
    fn segments_without_words_are_judged_whole() {
        let mut s = speech(&[
            ("This sentence ends here.", "this sentence ends here"),
            ("and this one", "and this one"),
            ("keeps going until", "keeps going until"),
            ("the very end.", "the very end"),
        ]);
        for seg in &mut s {
            seg.words.clear();
        }
        let mut tl = timeline(&s);
        cut(&mut tl, s[2].start, s[2].end);
        // A sliver off the last segment's start: still mostly kept, no seam.
        cut(&mut tl, s[3].start, s[3].start + 0.2);

        let (seams, issues) = review(&s, &tl);
        assert_eq!(seams.len(), 1);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].kind, "broken_sentence");
        assert_eq!(issues[0].at_segment, s[1].id);
        assert!(issues[0].description.contains("and this one"));
        assert_eq!(issues[0].restore_segment_ids, vec![s[2].id]);
    }

    /// Kept words are read off the timeline with a small tolerance: a clip
    /// edge a frame inside a kept word keeps it; a word clipped by more, or
    /// mostly outside, is not kept.
    #[test]
    fn a_word_is_kept_within_a_frame_of_its_clip() {
        let spans = [(1.0, 2.0)];
        assert!(word_kept(1.7, 2.03, &spans), "a frame past the edge");
        assert!(word_kept(0.98, 1.3, &spans), "a frame before the start");
        assert!(!word_kept(1.8, 2.3, &spans), "clipped by 0.3 s");
        assert!(!word_kept(2.0, 2.04, &spans), "the next word, outside");
        assert!(word_kept(1.5, 1.5, &spans), "a zero-length word inside");
    }

    /// Word shares follow the cleaned text even where whisper split a word
    /// ("didn 't") or cleanup added punctuation the raw words lack.
    #[test]
    fn word_shares_map_raw_words_onto_the_cleaned_text() {
        let text = "I didn't use AI. Don't";
        let words: Vec<Word> = "i didn 't use ai don 't"
            .split_whitespace()
            .map(|w| Word { text: w.into(), start: 0.0, end: 0.0, confidence: 1.0 })
            .collect();
        let shares = word_shares(text, &words).unwrap();
        let parts: Vec<&str> = shares.iter().map(|r| &text[r.clone()]).collect();
        assert_eq!(parts, vec!["I ", "didn", "'t ", "use ", "AI. ", "Don", "'t"]);
    }
}
