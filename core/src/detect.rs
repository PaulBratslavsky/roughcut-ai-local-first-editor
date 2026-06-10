//! The AI first pass: silences, fillers, repeated takes, and the composed
//! rough cut. Deterministic (no LLM required) so it works fully offline and
//! degrades to a "fast non-LLM cut mode" on low-end machines.

use crate::model::{Aggressiveness, Timeline, Transcript, TranscriptSegment};
use uuid::Uuid;

pub const DEFAULT_FILLERS: &[&str] = &["um", "uh", "uhm", "er", "ah", "hmm", "mhm", "erm"];

#[derive(Debug, Clone, serde::Serialize)]
pub struct TimeRange {
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TakeGroup {
    pub id: Uuid,
    pub segment_ids: Vec<Uuid>,
    pub best_segment_id: Uuid,
}

/// Silence = explicit silence segments plus gaps between consecutive speech
/// segments longer than `min_duration`. (A waveform-based detector via ffmpeg
/// `silencedetect` can refine this behind the same signature later.)
pub fn detect_silences(transcript: &Transcript, duration: f64, min_duration: f64) -> Vec<TimeRange> {
    let mut out = vec![];
    let speech: Vec<&TranscriptSegment> =
        transcript.segments.iter().filter(|s| !s.is_silence).collect();
    for s in transcript.segments.iter().filter(|s| s.is_silence) {
        if s.end - s.start >= min_duration {
            out.push(TimeRange { start: s.start, end: s.end });
        }
    }
    let mut prev_end = 0.0_f64;
    for s in &speech {
        if s.start - prev_end >= min_duration {
            out.push(TimeRange { start: prev_end, end: s.start });
        }
        prev_end = prev_end.max(s.end);
    }
    if duration > 0.0 && duration - prev_end >= min_duration {
        out.push(TimeRange { start: prev_end, end: duration });
    }
    out.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());
    merge(out)
}

fn merge(mut ranges: Vec<TimeRange>) -> Vec<TimeRange> {
    ranges.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());
    let mut out: Vec<TimeRange> = vec![];
    for r in ranges {
        match out.last_mut() {
            Some(last) if r.start <= last.end + 1e-6 => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

fn normalize_word(w: &str) -> String {
    w.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect()
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct FillerFindings {
    /// Segments that are entirely filler ("um" as its own segment).
    pub segment_ids: Vec<Uuid>,
    /// Filler words inside longer sentences, as time ranges to remove.
    pub word_ranges: Vec<TimeRange>,
}

/// Find filler segments/words. PURE — flags are written only by
/// [`apply_annotations`], so every caller sees the same behavior.
pub fn detect_fillers(transcript: &Transcript, custom_words: &[String]) -> FillerFindings {
    let mut stoplist: Vec<String> = DEFAULT_FILLERS.iter().map(|s| s.to_string()).collect();
    stoplist.extend(custom_words.iter().map(|w| normalize_word(w)));
    let mut findings = FillerFindings::default();
    for seg in &transcript.segments {
        if seg.is_silence {
            continue;
        }
        let words_norm: Vec<String> = seg.words.iter().map(|w| normalize_word(&w.text)).collect();
        let all_filler = !words_norm.is_empty() && words_norm.iter().all(|w| stoplist.contains(w));
        if all_filler {
            findings.segment_ids.push(seg.id);
        } else {
            for (w, norm) in seg.words.iter().zip(&words_norm) {
                if stoplist.contains(norm) {
                    findings.word_ranges.push(TimeRange { start: w.start, end: w.end });
                }
            }
        }
    }
    findings.word_ranges = merge(std::mem::take(&mut findings.word_ranges));
    findings
}

/// Group near-duplicate consecutive speech segments as takes; the last take in
/// a group is best (creators re-record until they're happy). PURE — pass the
/// filler segment ids from [`detect_fillers`] so fillers don't break up runs.
pub fn detect_takes(transcript: &Transcript, filler_segment_ids: &[Uuid]) -> Vec<TakeGroup> {
    let idxs: Vec<usize> = transcript
        .segments
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            !s.is_silence && !s.text.is_empty() && !filler_segment_ids.contains(&s.id)
        })
        .map(|(i, _)| i)
        .collect();
    let mut groups: Vec<Vec<usize>> = vec![];
    for pair in idxs.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let sim = similarity(&transcript.segments[a].text, &transcript.segments[b].text);
        if sim >= 0.5 {
            match groups.last_mut() {
                Some(g) if *g.last().unwrap() == a => g.push(b),
                _ => groups.push(vec![a, b]),
            }
        }
    }
    groups
        .into_iter()
        .map(|g| {
            let best_idx = *g.last().unwrap();
            TakeGroup {
                id: Uuid::new_v4(),
                segment_ids: g.iter().map(|&i| transcript.segments[i].id).collect(),
                best_segment_id: transcript.segments[best_idx].id,
            }
        })
        .collect()
}

/// THE annotation write path: every flag on the transcript is written here
/// and nowhere else.
pub fn apply_annotations(
    transcript: &mut Transcript,
    fillers: &FillerFindings,
    takes: &[TakeGroup],
) {
    for seg in &mut transcript.segments {
        if fillers.segment_ids.contains(&seg.id) {
            seg.is_filler = true;
        }
        for group in takes {
            if group.segment_ids.contains(&seg.id) {
                seg.take_group_id = Some(group.id);
                seg.is_best_take = seg.id == group.best_segment_id;
            }
        }
    }
}

/// Detect fillers + takes and persist the flags — the single entry point both
/// the analysis tools and the rough cut use.
pub fn annotate(
    transcript: &mut Transcript,
    custom_words: &[String],
) -> (FillerFindings, Vec<TakeGroup>) {
    let fillers = detect_fillers(transcript, custom_words);
    let takes = detect_takes(transcript, &fillers.segment_ids);
    apply_annotations(transcript, &fillers, &takes);
    (fillers, takes)
}

/// Token-set similarity (Jaccard over normalized words) — cheap and adequate
/// for "did I just say the same sentence again".
fn similarity(a: &str, b: &str) -> f64 {
    let sa: std::collections::HashSet<String> =
        a.split_whitespace().map(normalize_word).filter(|w| !w.is_empty()).collect();
    let sb: std::collections::HashSet<String> =
        b.split_whitespace().map(normalize_word).filter(|w| !w.is_empty()).collect();
    if sa.is_empty() || sb.is_empty() {
        return 0.0;
    }
    let inter = sa.intersection(&sb).count() as f64;
    let union = sa.union(&sb).count() as f64;
    inter / union
}

pub struct RoughCutOutcome {
    pub timeline: Timeline,
    pub cut_count: u32,
}

/// Compose the first pass: start from the full source, then exclude silences,
/// fillers, and non-best takes. Non-destructive — everything is restorable.
pub fn generate_rough_cut(
    transcript: &mut Transcript,
    duration: f64,
    aggressiveness: Aggressiveness,
    custom_fillers: &[String],
    silence_min_duration: f64,
) -> RoughCutOutcome {
    let min_silence = match aggressiveness {
        Aggressiveness::Natural => silence_min_duration,
        Aggressiveness::Aggressive => (silence_min_duration * 0.5).max(0.25),
    };
    // Padding kept inside removed silence so cuts don't clip word edges.
    let breath = match aggressiveness {
        Aggressiveness::Natural => 0.12,
        Aggressiveness::Aggressive => 0.04,
    };

    let mut timeline = Timeline::new(duration);
    use crate::model::ClipOrigin::AiCut;

    for r in detect_silences(transcript, duration, min_silence) {
        let (s, e) = (r.start + breath, r.end - breath);
        if e > s {
            timeline.set_range_included(s, e, false, AiCut);
        }
    }

    let (fillers, takes) = annotate(transcript, custom_fillers);
    for id in &fillers.segment_ids {
        if let Some(seg) = transcript.segment(*id) {
            timeline.cut_linked(seg.start, seg.end, &[*id], AiCut);
        }
    }
    for r in &fillers.word_ranges {
        timeline.set_range_included(r.start, r.end, false, AiCut);
    }

    for group in &takes {
        for sid in &group.segment_ids {
            if *sid == group.best_segment_id {
                continue;
            }
            if let Some(seg) = transcript.segment(*sid) {
                timeline.cut_linked(seg.start, seg.end, &[*sid], AiCut);
            }
        }
    }

    let cut_count = timeline.cut_count;
    RoughCutOutcome { timeline, cut_count }
}

const STOPWORDS: &[&str] = &[
    "the", "and", "that", "this", "about", "with", "your", "you", "where", "when", "part",
    "section", "thing", "cut", "remove", "delete",
];

fn query_terms(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(normalize_word)
        .filter(|w| !w.is_empty() && w.len() > 2 && !STOPWORDS.contains(&w.as_str()))
        .collect()
}

/// BM25 ranking over speech segments (k1=1.2, b=0.75). The lexical half of
/// hybrid search; also the full offline fallback when no embedding index
/// exists. Scores > 0 only.
pub fn bm25_rank<'t>(
    transcript: &'t Transcript,
    query: &str,
    k: usize,
) -> Vec<(&'t TranscriptSegment, f32)> {
    const K1: f32 = 1.2;
    const B: f32 = 0.75;
    let terms = query_terms(query);
    if terms.is_empty() {
        return vec![];
    }
    let docs: Vec<(&TranscriptSegment, Vec<String>)> = transcript
        .segments
        .iter()
        .filter(|s| !s.is_silence && !s.text.is_empty())
        .map(|s| (s, s.text.split_whitespace().map(normalize_word).collect()))
        .collect();
    if docs.is_empty() {
        return vec![];
    }
    let n = docs.len() as f32;
    let avgdl = docs.iter().map(|(_, t)| t.len()).sum::<usize>() as f32 / n;
    // Document frequency per query term.
    let df: Vec<f32> = terms
        .iter()
        .map(|t| docs.iter().filter(|(_, toks)| toks.contains(t)).count() as f32)
        .collect();
    let mut scored: Vec<(&TranscriptSegment, f32)> = docs
        .iter()
        .map(|(seg, toks)| {
            let dl = toks.len() as f32;
            let mut score = 0.0f32;
            for (term, &dfi) in terms.iter().zip(&df) {
                if dfi == 0.0 {
                    continue;
                }
                let tf = toks.iter().filter(|w| *w == term).count() as f32;
                if tf == 0.0 {
                    continue;
                }
                let idf = (1.0 + (n - dfi + 0.5) / (dfi + 0.5)).ln();
                score += idf * (tf * (K1 + 1.0)) / (tf + K1 * (1.0 - B + B * dl / avgdl));
            }
            (*seg, score)
        })
        .filter(|(_, score)| *score > 0.0)
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(k);
    scored
}

/// Lexical search used by the offline agent path; thin wrapper over BM25.
pub fn find_segments<'t>(transcript: &'t Transcript, query: &str) -> Vec<&'t TranscriptSegment> {
    bm25_rank(transcript, query, 8).into_iter().map(|(s, _)| s).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::video::MockVideoEngine;
    use crate::adapters::VideoEngine;
    use crate::model::Aggressiveness;

    async fn fixture() -> (crate::model::Media, Transcript) {
        let media = MockVideoEngine.probe("/demo/footage.mp4").await.unwrap();
        let t = crate::demo::fixture_transcript(&media);
        (media, t)
    }

    #[tokio::test]
    async fn rough_cut_removes_things() {
        let (media, mut t) = fixture().await;
        let out = generate_rough_cut(&mut t, media.duration, Aggressiveness::Natural, &[], 0.8);
        assert!(out.cut_count >= 5, "expected several cuts, got {}", out.cut_count);
        assert!(out.timeline.included_duration() < media.duration);
        // Best take survived, earlier take did not.
        let best = t.segments.iter().find(|s| s.is_best_take).unwrap();
        let worse = t
            .segments
            .iter()
            .find(|s| s.take_group_id.is_some() && !s.is_best_take)
            .unwrap();
        let mid_best = (best.start + best.end) / 2.0;
        let mid_worse = (worse.start + worse.end) / 2.0;
        let included_at = |tl: &Timeline, t: f64| {
            tl.clips.iter().any(|c| c.included && c.source_in <= t && t < c.source_out)
        };
        assert!(included_at(&out.timeline, mid_best));
        assert!(!included_at(&out.timeline, mid_worse));
    }

    #[tokio::test]
    async fn find_segments_locates_the_tangent() {
        let (_m, t) = fixture().await;
        let hits = find_segments(&t, "the tangent about my weekend hiking");
        assert!(!hits.is_empty());
        assert!(hits.iter().any(|s| s.text.contains("hiking")));
    }
}
