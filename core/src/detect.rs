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
    raw_peaks: Option<&[u8]>,
    pps: u32,
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

    // Hybrid: word-gap candidates refined against real energy. Confirmed
    // ranges already carry asymmetric onset/tail guards — no extra breath
    // padding. Unconfirmed (no audio / flat dynamics) keep the old breath
    // treatment. Candidates whisper invented but audio refutes are SKIPPED.
    let candidates =
        silence_candidates(transcript, duration, (min_silence * 0.6).max(0.3));
    for r in refine_silences(&candidates, raw_peaks, pps, min_silence) {
        let (s, e) = if r.source == "confirmed" {
            (r.start, r.end)
        } else {
            (r.start + breath, r.end - breath)
        };
        if e > s {
            timeline.set_range_included(s, e, false, AiCut);
        }
    }

    // TIER 1 — auto-cut only the mechanical, high-confidence stuff: silence
    // (above) + filler words. Repeated takes are NO LONGER auto-cut here;
    // they're surfaced as reviewable suggestions (see `suggest_cuts`) — the
    // judgment calls that silent auto-cutting made inconsistent.
    let (fillers, _takes) = annotate(transcript, custom_fillers);
    for id in &fillers.segment_ids {
        if let Some(seg) = transcript.segment(*id) {
            timeline.cut_linked(seg.start, seg.end, &[*id], AiCut);
        }
    }
    for r in &fillers.word_ranges {
        timeline.set_range_included(r.start, r.end, false, AiCut);
    }

    let cut_count = timeline.cut_count;
    RoughCutOutcome { timeline, cut_count }
}

/// TIER 2 — the fuzzy, judgment-call cuts the rough cut MARKS rather than
/// applies: repeated takes (a re-recording of a nearby line) and semantic
/// repetition (a passage that says the same thing as an earlier one, via
/// embedding similarity when an index exists). Returns suggestions only;
/// nothing is mutated. `annotate` must have run first (sets filler flags).
pub fn suggest_cuts(
    transcript: &Transcript,
    embeddings: Option<&std::collections::HashMap<Uuid, Vec<f32>>>,
) -> Vec<crate::model::CutSuggestion> {
    use crate::model::CutSuggestion;
    let preview = |t: &str| -> String {
        let p: String = t.trim().chars().take(80).collect();
        if t.trim().chars().count() > 80 { format!("{p}…") } else { p }
    };
    let mut out: Vec<CutSuggestion> = vec![];
    let mut flagged: std::collections::HashSet<Uuid> = std::collections::HashSet::new();

    // Repeated takes: every non-best take in a group is a suggested cut.
    let filler_ids: Vec<Uuid> =
        transcript.segments.iter().filter(|s| s.is_filler).map(|s| s.id).collect();
    for group in detect_takes(transcript, &filler_ids) {
        for sid in &group.segment_ids {
            if *sid == group.best_segment_id {
                continue;
            }
            let Some(seg) = transcript.segment(*sid) else { continue };
            flagged.insert(*sid);
            out.push(CutSuggestion {
                id: Uuid::new_v4(),
                segment_ids: vec![*sid],
                start: seg.start,
                end: seg.end,
                reason: "repeated_take".into(),
                confidence: 0.8,
                duplicate_of: Some(group.best_segment_id),
                preview: preview(&seg.text),
            });
        }
    }

    // Semantic repetition: non-adjacent segments that say nearly the same
    // thing (high cosine). Keep the EARLIER, flag the later — never
    // double-flag a segment already caught as a repeated take.
    if let Some(emb) = embeddings {
        let speech: Vec<&TranscriptSegment> = transcript
            .segments
            .iter()
            .filter(|s| !s.is_silence && !s.is_filler && !s.text.is_empty())
            .collect();
        const REPEAT_SIM: f32 = 0.93;
        for (j, later) in speech.iter().enumerate() {
            if flagged.contains(&later.id) {
                continue;
            }
            let Some(lv) = emb.get(&later.id) else { continue };
            // Compare against earlier, non-adjacent segments; strongest hit.
            let mut best: Option<(Uuid, f32)> = None;
            for earlier in speech.iter().take(j.saturating_sub(1)) {
                if flagged.contains(&earlier.id) {
                    continue;
                }
                let Some(ev) = emb.get(&earlier.id) else { continue };
                let sim = crate::adapters::embed::cosine(lv, ev);
                if sim >= REPEAT_SIM && best.map(|(_, b)| sim > b).unwrap_or(true) {
                    best = Some((earlier.id, sim));
                }
            }
            if let Some((dup_of, sim)) = best {
                flagged.insert(later.id);
                out.push(CutSuggestion {
                    id: Uuid::new_v4(),
                    segment_ids: vec![later.id],
                    start: later.start,
                    end: later.end,
                    reason: "repetition".into(),
                    confidence: (sim as f64).min(0.99),
                    duplicate_of: Some(dup_of),
                    preview: preview(&later.text),
                });
            }
        }
    }

    out.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal));
    out
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
        let out = generate_rough_cut(&mut t, media.duration, Aggressiveness::Natural, &[], 0.8, None, 50);
        assert!(out.cut_count >= 5, "expected several cuts, got {}", out.cut_count);
        assert!(out.timeline.included_duration() < media.duration);
        // TIER 2: repeated takes are now MARKED, not auto-cut. The best take
        // stays on the timeline; the worse take is NOT cut by the rough pass
        // but IS surfaced as a suggestion.
        let best = t.segments.iter().find(|s| s.is_best_take).unwrap();
        let worse = t
            .segments
            .iter()
            .find(|s| s.take_group_id.is_some() && !s.is_best_take)
            .unwrap();
        let worse_id = worse.id;
        let mid_best = (best.start + best.end) / 2.0;
        let mid_worse = (worse.start + worse.end) / 2.0;
        let included_at = |tl: &Timeline, t: f64| {
            tl.clips.iter().any(|c| c.included && c.source_in <= t && t < c.source_out)
        };
        assert!(included_at(&out.timeline, mid_best));
        assert!(included_at(&out.timeline, mid_worse), "worse take is flagged, not auto-cut");
        let suggestions = suggest_cuts(&t, None);
        assert!(
            suggestions.iter().any(|s| s.reason == "repeated_take" && s.segment_ids.contains(&worse_id)),
            "the worse take should be a repeated_take suggestion: {suggestions:?}"
        );
    }

    #[tokio::test]
    async fn find_segments_locates_the_tangent() {
        let (_m, t) = fixture().await;
        let hits = find_segments(&t, "the tangent about my weekend hiking");
        assert!(!hits.is_empty());
        assert!(hits.iter().any(|s| s.text.contains("hiking")));
    }
}

// --- hybrid silence refinement (transcript candidates ∩ audio energy) ------

/// A silence range with provenance: "confirmed" = audio energy agrees;
/// "transcript_only" = whisper's word said so but audio couldn't check
/// (no peaks, or the file's dynamics are too flat to trust).
#[derive(Debug, Clone, serde::Serialize)]
pub struct RefinedSilence {
    pub start: f64,
    pub end: f64,
    pub source: &'static str,
    pub confidence: f64,
}

/// Candidate windows from WORD gaps (not segment gaps — whisper stretches
/// segments across trailing dead air, so the segment view structurally
/// misses pauses that word timings expose).
pub fn silence_candidates(transcript: &Transcript, duration: f64, min_gap: f64) -> Vec<TimeRange> {
    let mut bounds: Vec<(f64, f64)> = vec![];
    for seg in transcript.segments.iter().filter(|s| !s.is_silence) {
        if seg.words.is_empty() {
            bounds.push((seg.start, seg.end));
        } else {
            for w in &seg.words {
                bounds.push((w.start, w.end));
            }
        }
    }
    bounds.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = vec![];
    let mut prev_end = 0.0_f64;
    for (start, end) in bounds {
        if start - prev_end >= min_gap {
            out.push(TimeRange { start: prev_end, end: start });
        }
        prev_end = prev_end.max(end);
    }
    if duration > 0.0 && duration - prev_end >= min_gap {
        out.push(TimeRange { start: prev_end, end: duration });
    }
    merge(out)
}

/// Keep this much attached to the NEXT speech onset (consonant attacks are
/// fragile) and after the previous word's tail (decay is forgiving).
const KEEP_PRE_ONSET_S: f64 = 0.12;
const KEEP_POST_TAIL_S: f64 = 0.06;
/// Loud blips shorter than this inside a quiet span are breaths/clicks.
const BLIP_S: f64 = 0.10;

/// Refine candidates against raw audio peaks. Thresholds derive from the
/// file's OWN dynamics (a MacBook mic peaks speech at 2.4% absolute — fixed
/// dB thresholds classify whole recordings as silence): noise floor = p10,
/// speech = p95, with enter/exit hysteresis between them. When the file's
/// dynamics are too flat to trust (speech < 2x floor), candidates pass
/// through as transcript_only instead of pretending precision.
pub fn refine_silences(
    candidates: &[TimeRange],
    raw_peaks: Option<&[u8]>,
    pps: u32,
    min_duration: f64,
) -> Vec<RefinedSilence> {
    let transcript_only = |c: &[TimeRange]| {
        c.iter()
            .filter(|r| r.end - r.start >= min_duration)
            .map(|r| RefinedSilence {
                start: r.start,
                end: r.end,
                source: "transcript_only",
                confidence: 0.5,
            })
            .collect::<Vec<_>>()
    };
    let Some(peaks) = raw_peaks else { return transcript_only(candidates) };
    if peaks.is_empty() || pps == 0 {
        return transcript_only(candidates);
    }

    let mut sorted: Vec<u8> = peaks.to_vec();
    sorted.sort_unstable();
    let pct = |p: f64| -> f64 {
        sorted[((sorted.len() - 1) as f64 * p) as usize] as f64
    };
    let floor = pct(0.10);
    let speech = pct(0.95);
    // Honesty gate: flat dynamics (constant fan, broken gain) make energy
    // uninformative — degrade loudly-typed rather than silently wrong.
    if speech < (floor * 2.0).max(3.0) {
        return transcript_only(candidates);
    }
    let range = speech - floor;
    let enter = floor + 0.15 * range; // below this = silent
    let exit = floor + 0.30 * range; // must exceed this to count as speech
    let blip = ((BLIP_S * pps as f64).round() as usize).max(1);

    let mut out = vec![];
    for cand in candidates {
        // Look slightly beyond whisper's sloppy boundaries.
        let w_start = ((cand.start - 0.5).max(0.0) * pps as f64) as usize;
        let w_end = (((cand.end + 0.5) * pps as f64) as usize).min(peaks.len());
        if w_start >= w_end {
            continue;
        }
        // Longest quiet run in the window, closing loud blips < BLIP_S.
        let mut best: Option<(usize, usize)> = None;
        let mut run_start: Option<usize> = None;
        let mut loud_run = 0usize;
        for i in w_start..=w_end {
            let level = if i < w_end { peaks[i] as f64 } else { f64::MAX };
            let quiet = level <= enter || (run_start.is_some() && level < exit);
            if quiet {
                if run_start.is_none() {
                    run_start = Some(i);
                }
                loud_run = 0;
            } else if let Some(rs) = run_start {
                loud_run += 1;
                if loud_run > blip || i == w_end {
                    let end = i - loud_run;
                    if best.map(|(bs, be)| be - bs).unwrap_or(0) < end - rs {
                        best = Some((rs, end));
                    }
                    run_start = None;
                    loud_run = 0;
                }
            }
        }
        let Some((qs, qe)) = best else { continue }; // audio disagrees: loud
        let mut start = qs as f64 / pps as f64 + KEEP_POST_TAIL_S;
        let mut end = qe as f64 / pps as f64 - KEEP_PRE_ONSET_S;
        // Never cut outside the candidate either (words live there).
        start = start.max(cand.start);
        end = end.min(cand.end + 0.5);
        if end - start >= min_duration {
            out.push(RefinedSilence { start, end, source: "confirmed", confidence: 0.9 });
        }
    }
    out
}

#[cfg(test)]
mod hybrid_tests {
    use super::*;

    /// 20s @ 50pps: speech at [0..5], [8..12], silence elsewhere, with a
    /// 100ms breath blip at 6.5s.
    fn synth_peaks() -> Vec<u8> {
        let pps = 50usize;
        let mut p = vec![6u8; 20 * pps];
        for i in 0..(5 * pps) {
            p[i] = 180;
        }
        for i in (8 * pps)..(12 * pps) {
            p[i] = 180;
        }
        for i in (13 * pps)..(13 * pps + 5) {
            p[i] = 120; // breath blip, 100ms
        }
        p
    }

    #[test]
    fn refines_sloppy_whisper_bounds_to_energy() {
        // Whisper thinks silence is 4.8..8.3 (±sloppy); energy says 5..8.
        let cands = vec![TimeRange { start: 4.8, end: 8.3 }];
        let out = refine_silences(&cands, Some(&synth_peaks()), 50, 0.5);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, "confirmed");
        assert!((out[0].start - 5.06).abs() < 0.06, "start {}", out[0].start);
        assert!((out[0].end - 7.88).abs() < 0.06, "end {}", out[0].end);
    }

    #[test]
    fn breath_blip_does_not_split_a_silence() {
        let cands = vec![TimeRange { start: 12.0, end: 20.0 }];
        let out = refine_silences(&cands, Some(&synth_peaks()), 50, 1.0);
        assert_eq!(out.len(), 1, "blip must not split: {out:?}");
        assert!(out[0].end - out[0].start > 6.0);
    }

    #[test]
    fn loud_candidate_is_rejected() {
        // Whisper hallucinated a gap where audio is full-volume speech.
        let cands = vec![TimeRange { start: 9.0, end: 11.0 }];
        let out = refine_silences(&cands, Some(&synth_peaks()), 50, 0.5);
        assert!(out.is_empty(), "audio disagrees -> no cut: {out:?}");
    }

    #[test]
    fn flat_dynamics_degrade_to_transcript_only() {
        let flat = vec![100u8; 1000]; // constant fan noise
        let cands = vec![TimeRange { start: 2.0, end: 4.0 }];
        let out = refine_silences(&cands, Some(&flat), 50, 0.5);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, "transcript_only");
    }

    #[test]
    fn no_peaks_means_transcript_only() {
        let cands = vec![TimeRange { start: 1.0, end: 3.0 }];
        let out = refine_silences(&cands, None, 50, 0.5);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, "transcript_only");
    }
}
