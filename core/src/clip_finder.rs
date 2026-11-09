//! The clip finder: find the moments of a long-form video that work as
//! standalone short-form clips (Shorts / Reels / TikTok) and rank them.
//!
//! Same design rule as `story.rs`: deterministic scaffolding with the model
//! embedded in ONE step. The transcript is re-cut into sentences first
//! (whisper's segments often stop mid-sentence). With a model, it reads each
//! topic of the outline as numbered sentences and PICKS the shorts in it —
//! the first and last sentence of each, so a short starts where its idea
//! starts and ends where it lands — rating each on a four-part rubric. The
//! code checks every pick (line numbers, length, filler edges). No model, or
//! a topic whose reply is unusable → sentence windows inside one beat,
//! ranked by a cheap heuristic.

use crate::adapters::InferenceClient;
use crate::engine::Editor;
use crate::error::{CoreError, Result};
use crate::model::{pieces_overlap, ClipCandidate, SourceRange, Timeline, TranscriptSegment};
use crate::story::Beat;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct FindOptions {
    /// How many candidates to return.
    pub count: usize,
    /// Playable length bounds, seconds.
    pub min_s: f64,
    pub max_s: f64,
    /// Only moments about this topic ("AI tools", "pricing"). Located with the
    /// hybrid transcript search, then judged by the model.
    pub theme: Option<String>,
    /// Scan the whole video: every topic is read by the model (without one,
    /// the heuristic pool is filled across every story beat).
    pub scan: bool,
    /// Drop candidates the MODEL scored below this. Ones it didn't judge (no
    /// model, an unusable reply) are kept and rank after judged ones.
    pub min_score: Option<f64>,
    /// Also propose shorts made of several pieces (Phase 3; accepted and
    /// ignored until then).
    pub multi_moment: bool,
}

impl Default for FindOptions {
    fn default() -> Self {
        Self { count: 8, min_s: 15.0, max_s: 60.0, theme: None, scan: false, min_score: None, multi_moment: false }
    }
}

/// One sentence of the transcript: whisper's segments re-cut at sentence
/// punctuation, joined where a sentence runs across segments.
#[derive(Debug, Clone)]
struct Sentence {
    start: f64,
    end: f64,
    text: String,
    /// The speech segments it draws on (indexes into `speech`).
    seg_first: usize,
    seg_last: usize,
    /// A filler ("um") or a throwaway ("Right?"): fine mid-short, never a
    /// short's first or last line.
    filler: bool,
}

/// A stretch of sentences `first..=last`.
#[derive(Debug, Clone)]
struct Window {
    first: usize,
    last: usize,
    start: f64,
    end: f64,
    /// Playable seconds in the current cut.
    duration: f64,
    score: f64,
    signals: Vec<&'static str>,
    title: Option<String>,
    reason: Option<String>,
    /// The model picked and rated it. In a model run, anything it didn't
    /// ranks after everything it did — heuristic and model scores aren't one
    /// scale.
    judged: bool,
    /// The model that judged it, when one did.
    scorer: Option<String>,
    /// The story beat it starts in.
    beat: usize,
}

/// A run of sentences the model picks shorts from in one request: a beat,
/// beats too short to hold a short joined together, or a part of a long one.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Region {
    first: usize,
    last: usize,
}

/// The heuristic pool for a plain run with no model.
const POOL_MAX: usize = 24;
/// The most candidates one run returns.
const MAX_COUNT: usize = 60;
/// Two candidates sharing more than this fraction of the SHORTER one are the
/// same moment; keep the better one. (Over the union is too lenient: two 30s
/// clips sharing 13s read as the same short but score only 0.24.)
const MAX_OVERLAP: f64 = 0.3;
/// With a theme, model-judged candidates under this are off-topic rejects.
const MIN_THEMED_SCORE: f64 = 30.0;
/// Heuristic windows in one beat this close together are slices of one idea
/// (one has the setup, the next the payoff); keep the better.
const ADJACENT_S: f64 = 2.0;
/// A pause this long ends a sentence even without punctuation.
const PAUSE_BREAK_S: f64 = 1.0;
/// Whisper sometimes drops punctuation for a while: a "sentence" this long
/// ends at the next segment edge.
const RUN_ON_S: f64 = 20.0;
/// Lead-in before a sentence's first word and tail after its last, inside
/// the gap to its neighbors (whisper's word timings are approximate).
const LEAD_S: f64 = 0.1;
const TAIL_S: f64 = 0.15;
/// The longest stretch one pick request covers (playable seconds).
const REGION_MAX_S: f64 = 300.0;
/// Pick requests in a scan, at most: about 3 hours of video in 5-minute
/// topics. A long video beyond that keeps its best-scoring topics.
const SCAN_REGIONS: usize = 40;
/// Shorts asked for per topic.
const PICKS_PER_REGION: usize = 3;

/// One run's result: the candidates written, and how they were scored.
pub struct Found {
    pub candidates: Vec<ClipCandidate>,
    /// "llm" (a model picked them) | "heuristic".
    pub method: &'static str,
}

/// Find, rank, and persist clip candidates for the project's current cut.
pub async fn find_clips(editor: &Editor, project_id: Uuid, opts: FindOptions) -> Result<Found> {
    // The main cut is set aside while a clip is open in the Editor.
    editor.require_no_clip_session(project_id, "scanning for shorts")?;
    // One run per project at a time: a second one would race the first to
    // write the list.
    let _scan = editor.begin_clip_scan(project_id)?;
    let opts = FindOptions {
        count: opts.count.clamp(1, MAX_COUNT),
        min_s: opts.min_s.max(3.0),
        max_s: opts.max_s.max(opts.min_s.max(3.0) + 1.0),
        theme: opts.theme.as_deref().map(str::trim).filter(|t| !t.is_empty()).map(str::to_string),
        min_score: opts.min_score.filter(|m| m.is_finite()),
        ..opts
    };
    let (main, speech, saved) = editor.with_project(project_id, |p, t| {
        let t = t.ok_or_else(|| CoreError::InvalidArg("no transcript; transcribe first".into()))?;
        let speech: Vec<TranscriptSegment> =
            t.segments.iter().filter(|s| !s.is_silence && !s.text.trim().is_empty()).cloned().collect();
        let saved: Vec<Vec<SourceRange>> = p.saved_clips.iter().map(|c| c.ranges.clone()).collect();
        Ok((p.timeline.clone(), speech, saved))
    })?;
    if speech.is_empty() {
        return Err(CoreError::InvalidArg("transcript has no speech".into()));
    }

    // Beats keep a short on one topic. Cached after the first LLM outline;
    // instant pause-based sections without a model.
    progress(editor, project_id, 0.05, "outlining the video…");
    let outline = crate::story::outline(editor, project_id).await?;
    let sents = sentences(&speech, &main);
    let beat_of_seg = beat_index(&speech, &outline.beats);
    let beat_of: Vec<usize> = sents.iter().map(|s| beat_of_seg[s.seg_first]).collect();

    let mut windows = candidate_windows(&sents, Some(&beat_of), &main, &opts);
    let mut by_beat = true;
    if windows.is_empty() {
        // Beats shorter than min_s (choppy pause-based sections): let a
        // window span them rather than return nothing.
        windows = candidate_windows(&sents, None, &main, &opts);
        by_beat = false;
    }
    let inference = editor.inference()?;
    let healthy = inference.healthy().await;
    if healthy && outline.method != "llm" {
        progress(editor, project_id, 0.1, "the model's outline didn't work — topics come from pauses in the speech");
    }
    // A scan doesn't offer moments already saved as clips. (A plain run
    // behaves as it always has: the chat may ask for a saved moment again.)
    let is_saved = |w: &Window| saved.iter().any(|r| pieces_overlap(&w.pieces(), r) > MAX_OVERLAP);
    if opts.scan && !windows.is_empty() {
        windows.retain(|w| !is_saved(w));
        if windows.is_empty() {
            let candidates = editor.replace_clip_candidates(project_id, vec![], None)?;
            progress(editor, project_id, 1.0, "every moment that fits is already saved as a clip");
            return Ok(Found { candidates, method: if healthy { "llm" } else { "heuristic" } });
        }
    }
    if windows.is_empty() {
        return Err(CoreError::InvalidArg(format!(
            "no {:.0}-{:.0}s stretch of speech in the current cut — widen the length range or restore cut material",
            opts.min_s, opts.max_s
        )));
    }
    for w in &mut windows {
        let (score, signals) = heuristic_score(&sents, &main, w, &outline.beats, &beat_of, &opts);
        w.score = score;
        w.signals = signals;
    }
    let mut relevance: Option<Vec<f64>> = None;
    if let Some(theme) = &opts.theme {
        progress(editor, project_id, 0.2, format!("searching for “{theme}”…"));
        // Generous recall: the model, told the theme, judges what is really
        // about it. Without a model this ranking is the answer.
        let k = (speech.len() / 6).clamp(8, 40);
        let (hits, _) = editor.hybrid_find(project_id, theme, k).await?;
        let rel = sentence_relevance(&sents, &segment_relevance(&speech, &hits));
        windows.retain_mut(|w| on_theme(&rel, w));
        if windows.is_empty() {
            return Err(CoreError::InvalidArg(format!(
                "nothing in the current cut matches the theme “{theme}” — try different words"
            )));
        }
        relevance = Some(rel);
    }

    // The app log: whether the model picked them, and how many topics it
    // couldn't (those keep their rule-scored windows).
    let mut step = crate::applog::Step::start(editor, "short_picks", Some(project_id));
    let picked = if healthy {
        let ctx = PickContext {
            sents: &sents,
            main: &main,
            beats: &outline.beats,
            beat_of: &beat_of,
            relevance: relevance.as_deref(),
            opts: &opts,
        };
        let mut tally = Tally::default();
        let picked = pick_all(editor, project_id, inference.as_ref(), &ctx, &windows, &mut tally).await;
        step.count("topics", tally.topics)
            .count("topics_failed", tally.failed)
            .count("topics_retried", tally.retried)
            .count("picks_too_long", tally.too_long)
            .count("picks_too_short", tally.too_short)
            .count("picks_bad_lines", tally.bad_lines);
        for m in &tally.models {
            step.model(m);
        }
        match picked {
            Ok(Some(picked)) => {
                step.method("llm");
                step.ok();
                Some(picked)
            }
            Ok(None) => {
                step.fallback("no_usable_picks", None);
                None
            }
            // A Hosted account problem (no key, rejected key) is shown, and
            // the stored candidates stay as they were.
            Err(e) => {
                step.failed(&e);
                return Err(e);
            }
        }
    } else {
        step.fallback("model_unreachable", None);
        None
    };
    let (mut ranked, method) = match picked {
        Some(picked) => (picked, "llm"),
        None => (build_pool(windows, &beat_of, by_beat, &opts), "heuristic"),
    };
    if opts.scan {
        ranked.retain(|w| !is_saved(w));
    }
    if opts.theme.is_some() && method == "llm" {
        // The model was told to pick only on-theme moments and score honestly:
        // trust it and return fewer, on-topic results rather than pad the list.
        let keep_best = ranked.iter().any(|w| w.judged && w.score >= MIN_THEMED_SCORE);
        if keep_best {
            ranked.retain(|w| !w.judged || w.score >= MIN_THEMED_SCORE);
        }
    }
    keep_above(&mut ranked, opts.min_score);
    let picked = diversify(ranked, opts.count);
    let candidates: Vec<ClipCandidate> =
        picked.iter().map(|w| to_candidate(&speech, &sents, w, method, opts.theme.as_deref())).collect();
    // Written in one step that (for a scan) also drops moments saved while
    // it ran.
    let drop_saved = opts.scan.then_some(MAX_OVERLAP);
    let candidates = editor.replace_clip_candidates(project_id, candidates, drop_saved)?;
    progress(editor, project_id, 1.0, format!("found {} clip candidates", candidates.len()));
    Ok(Found { candidates, method })
}

/// The heuristic pool (no model). A plain run: the best `POOL_MAX` (or
/// `count`) distinct moments. A scan: taken beat by beat (`round_robin_pool`),
/// or best-first when the windows span beats; nothing is cut off.
fn build_pool(windows: Vec<Window>, beat_of: &[usize], by_beat: bool, opts: &FindOptions) -> Vec<Window> {
    if !opts.scan {
        diversify(windows, POOL_MAX.max(opts.count))
    } else if by_beat {
        round_robin_pool(windows, beat_of, usize::MAX)
    } else {
        diversify(windows, usize::MAX)
    }
}

/// `min_score` drops what the model judged below it; unjudged windows stay.
fn keep_above(ranked: &mut Vec<Window>, min: Option<f64>) {
    if let Some(min) = min {
        ranked.retain(|w| !w.judged || w.score >= min);
    }
}

/// The scan pool: every beat's best window, then every beat's second best,
/// and so on, until `cap`. Each beat's windows are ranked by score and
/// de-duplicated first, so a beat never offers the same moment twice, and
/// every beat with a window is in the pool before any beat has two. When a
/// round doesn't fit under the cap, its best windows get in, wherever in the
/// video they are.
fn round_robin_pool(windows: Vec<Window>, beat_of: &[usize], cap: usize) -> Vec<Window> {
    let mut beats: std::collections::BTreeMap<usize, Vec<Window>> = Default::default();
    for w in windows {
        beats.entry(beat_of[w.first]).or_default().push(w);
    }
    let mut lanes: Vec<std::vec::IntoIter<Window>> =
        beats.into_values().map(|ws| diversify(ws, usize::MAX).into_iter()).collect();
    let mut pool: Vec<Window> = vec![];
    while pool.len() < cap {
        // One window from every lane: its next that isn't a moment taken.
        let mut round: Vec<Window> = vec![];
        for lane in lanes.iter_mut() {
            for w in lane.by_ref() {
                if pool.iter().chain(round.iter()).all(|p| !same_moment(p, &w)) {
                    round.push(w);
                    break;
                }
            }
        }
        if round.is_empty() {
            break;
        }
        round.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        let room = cap - pool.len();
        pool.extend(round.into_iter().take(room));
    }
    pool
}

fn progress(editor: &Editor, project_id: Uuid, fraction: f64, message: impl Into<String>) {
    crate::events::send(
        &editor.sink(),
        crate::events::CoreEvent::progress(
            crate::events::ProgressTask::FindClips,
            Some(project_id),
            fraction,
            message,
        ),
    );
}

// ------------------------------------------------------------- sentences

/// The transcript as sentences. Each segment is split at sentence
/// punctuation (each boundary in a gap between two words: `split_segment`)
/// and a sentence that runs on into the
/// next segment is joined to it — unless the edit keeps one segment and cuts
/// the other (a false start must not leak into the kept take). A filler or
/// throwaway segment is a line of its own; a pause of `PAUSE_BREAK_S` or a
/// run-on past `RUN_ON_S` also ends a sentence.
fn sentences(speech: &[TranscriptSegment], main: &Timeline) -> Vec<Sentence> {
    let seg_kept = |seg: &TranscriptSegment| {
        let mid = (seg.start + seg.end) / 2.0;
        main.clips.iter().any(|c| c.included && c.source_in <= mid && mid < c.source_out)
    };
    let mut out: Vec<Sentence> = vec![];
    let mut cur: Option<Sentence> = None;
    for (si, seg) in speech.iter().enumerate() {
        let solo = seg.is_filler || is_throwaway(&seg.text);
        let pause = si > 0 && seg.start - speech[si - 1].end >= PAUSE_BREAK_S;
        if let Some(c) = cur.take() {
            let cut_between = seg_kept(&speech[c.seg_last]) != seg_kept(seg);
            if solo || pause || cut_between || c.end - c.start >= RUN_ON_S {
                out.push(c);
            } else {
                cur = Some(c);
            }
        }
        if solo {
            let (start, end) = seg_span(seg);
            out.push(Sentence { start, end, text: seg.text.trim().to_string(), seg_first: si, seg_last: si, filler: true });
            continue;
        }
        for (start, end, text) in split_segment(seg) {
            let ends = ends_sentence(&text);
            match cur.as_mut() {
                Some(c) => {
                    c.text.push(' ');
                    c.text.push_str(&text);
                    c.end = c.end.max(end);
                    c.seg_last = si;
                }
                None => cur = Some(Sentence { start, end, text, seg_first: si, seg_last: si, filler: false }),
            }
            if ends {
                out.extend(cur.take());
            }
        }
    }
    out.extend(cur);
    for s in &mut out {
        s.filler = s.filler || is_throwaway(&s.text);
    }
    // A little lead-in and tail, inside the gap to the neighbors only.
    let raw: Vec<(f64, f64)> = out.iter().map(|s| (s.start, s.end)).collect();
    for (i, s) in out.iter_mut().enumerate() {
        let (start, end) = raw[i];
        let floor = if i > 0 { raw[i - 1].1.min(start) } else { start.min(0.0) };
        let ceil = raw.get(i + 1).map(|n| n.0.max(end)).unwrap_or(f64::INFINITY);
        s.start = (start - LEAD_S).max(floor);
        s.end = (end + TAIL_S).min(ceil);
    }
    out
}

/// A segment's span, never cutting into its own first or last word (after
/// word alignment a segment's end can sit a little before its last word's).
fn seg_span(seg: &TranscriptSegment) -> (f64, f64) {
    let words = || seg.words.iter().filter(|w| !w.text.trim().is_empty());
    let start = words().map(|w| w.start).fold(seg.start, f64::min);
    let end = words().map(|w| w.end).fold(seg.end, f64::max);
    (start, end.max(start))
}

/// One segment's pieces at sentence punctuation: (start, end, text).
///
/// Every boundary falls in the gap between two words. The sentence's last
/// word is found by letters and digits: the cleaned-up text has punctuation
/// and capitals the word list doesn't ("together. I could" vs "together i
/// could"). When the clean-up changed the words themselves, it is the word
/// nearest the same share of the letters. Without word timings, by position.
fn split_segment(seg: &TranscriptSegment) -> Vec<(f64, f64, String)> {
    let text = seg.text.trim();
    let (seg_start, seg_end) = seg_span(seg);
    let cuts = sentence_ends(text);
    if cuts.is_empty() {
        return vec![(seg_start, seg_end, text.to_string())];
    }
    let letters = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).count();
    let total = letters(text).max(1);
    let words: Vec<&crate::model::Word> = seg.words.iter().filter(|w| !w.text.trim().is_empty()).collect();
    // Letters spoken up to and including each word.
    let through: Vec<usize> = words
        .iter()
        .scan(0, |n, w| {
            *n += letters(&w.text);
            Some(*n)
        })
        .collect();
    let spelled = through.last().copied().unwrap_or(0);
    // The word sentence `i` ends on: never before the last one's, and
    // leaving a word for each sentence still to come.
    let last_word = |k: usize, i: usize, after: Option<usize>| -> usize {
        let t = if spelled == total {
            through.iter().position(|&n| n >= k).unwrap_or(words.len() - 1)
        } else {
            let want = k as f64 * spelled as f64 / total as f64;
            (0..words.len())
                .min_by(|&a, &b| {
                    (through[a] as f64 - want).abs().partial_cmp(&(through[b] as f64 - want).abs()).unwrap_or(std::cmp::Ordering::Equal)
                })
                .unwrap_or(0)
        };
        // A punctuation token after the word ("job" ".") ends with it.
        let mut t = t;
        while t + 1 < words.len() && letters(&words[t + 1].text) == 0 {
            t += 1;
        }
        let lo = after.map_or(0, |a| a + 1);
        let hi = (words.len() - 1).saturating_sub(cuts.len() - i);
        t.min(hi).max(lo).min(words.len() - 1)
    };
    let mut out = vec![];
    let (mut from, mut start) = (0usize, seg_start);
    let mut prev: Option<usize> = None;
    for (i, &c) in cuts.iter().enumerate() {
        let (end, next) = if spelled == 0 {
            let t = seg_start + (seg_end - seg_start) * letters(&text[..c]) as f64 / total as f64;
            (t, t)
        } else {
            let t = last_word(letters(&text[..c]), i, prev);
            prev = Some(t);
            (words[t].end, words.get(t + 1).map_or(seg_end, |n| n.start))
        };
        let end = end.clamp(start, seg_end);
        let piece = text[from..c].trim();
        if !piece.is_empty() {
            out.push((start, end, piece.to_string()));
        }
        start = next.clamp(end, seg_end);
        from = c;
    }
    let rest = text[from..].trim();
    if !rest.is_empty() {
        out.push((start, seg_end, rest.to_string()));
    }
    out
}

/// Byte offsets inside `text` where a sentence ends: after `.`, `!`, `?` or
/// `…` (and any closing quotes) followed by a space, unless it follows an
/// abbreviation like "Dr." or "e.g." or the next word starts lowercase
/// ("And then… it broke.").
fn sentence_ends(text: &str) -> Vec<usize> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut cuts = vec![];
    let mut k = 0;
    while k < chars.len() {
        let (i, c) = chars[k];
        if !matches!(c, '.' | '!' | '?' | '…') {
            k += 1;
            continue;
        }
        let mut j = k + 1;
        while j < chars.len() && matches!(chars[j].1, '.' | '!' | '?' | '…' | '"' | '\'' | ')' | ']' | '”' | '’') {
            j += 1;
        }
        let next_lower = chars[j..].iter().map(|(_, ch)| *ch).find(|ch| ch.is_alphabetic()).is_some_and(char::is_lowercase);
        if j < chars.len()
            && chars[j].1.is_whitespace()
            && !next_lower
            && !(c == '.' && is_abbreviation(&text[..i]))
        {
            cuts.push(chars[j].0);
        }
        k = j;
    }
    cuts
}

fn is_abbreviation(before: &str) -> bool {
    const ABBREVIATIONS: &[&str] = &["mr", "mrs", "ms", "dr", "prof", "st", "vs", "jr", "sr", "inc", "ltd", "approx"];
    let word = before.rsplit(char::is_whitespace).next().unwrap_or("");
    let word = word.trim_start_matches(|c: char| !c.is_alphanumeric());
    // Dotted initials ("e.g", "U.S") — not "Next.js" or "3.5".
    let dotted = word.contains('.')
        && word.split('.').all(|part| part.chars().count() == 1 && part.chars().all(char::is_alphabetic));
    dotted || ABBREVIATIONS.contains(&word.to_lowercase().as_str())
}

fn ends_sentence(text: &str) -> bool {
    text.trim_end().trim_end_matches(['"', '\'', ')', ']', '”', '’']).ends_with(['.', '!', '?', '…'])
}

// ---------------------------------------------------------- theme search

/// Each speech segment's relevance to the theme, 0..1 (1 = the best hit).
/// Segments the search didn't return are 0.
fn segment_relevance(speech: &[TranscriptSegment], hits: &[(Uuid, f64)]) -> Vec<f64> {
    let top = hits.iter().map(|(_, s)| *s).fold(0.0, f64::max);
    speech
        .iter()
        .map(|seg| {
            hits.iter()
                .find(|(id, _)| *id == seg.id)
                .map(|(_, s)| if top > 0.0 { s / top } else { 0.0 })
                .unwrap_or(0.0)
        })
        .collect()
}

/// Each sentence's relevance: its best segment's.
fn sentence_relevance(sents: &[Sentence], seg_relevance: &[f64]) -> Vec<f64> {
    sents.iter().map(|s| seg_relevance[s.seg_first..=s.seg_last].iter().copied().fold(0.0, f64::max)).collect()
}

/// Keep a window only if the theme search hit it, and rank it on theme first,
/// short-form craft second. (Model picks too: the model is told the theme,
/// but its ratings don't say how on-topic a pick is.)
fn on_theme(relevance: &[f64], w: &mut Window) -> bool {
    let rel = window_relevance(relevance, w);
    if rel <= 0.0 {
        return false;
    }
    w.score = 0.4 * w.score + 60.0 * rel;
    w.signals.push("on theme");
    true
}

/// How much of a window is about the theme, 0..1: its best-matching sentence
/// dominates; coverage rewards staying on topic throughout. 0 = no hit at all.
fn window_relevance(relevance: &[f64], w: &Window) -> f64 {
    let segs = &relevance[w.first..=w.last];
    let best = segs.iter().copied().fold(0.0, f64::max);
    if best <= 0.0 {
        return 0.0;
    }
    let coverage = segs.iter().filter(|r| **r > 0.0).count() as f64 / segs.len() as f64;
    0.7 * best + 0.3 * coverage
}

/// Which beat each speech segment belongs to (a segment no beat claims
/// inherits its predecessor's, so boundaries stay monotonic).
fn beat_index(speech: &[TranscriptSegment], beats: &[Beat]) -> Vec<usize> {
    let mut out = Vec::with_capacity(speech.len());
    let mut last = 0;
    for seg in speech {
        if let Some(b) = beats.iter().position(|b| b.segment_ids.contains(&seg.id)) {
            last = b;
        }
        out.push(last);
    }
    out
}

/// Is a sentence kept in the current cut? (Its midpoint decides, the same
/// rule the SRT export uses.)
fn kept(main: &Timeline, s: &Sentence) -> bool {
    let mid = (s.start + s.end) / 2.0;
    main.clips.iter().any(|c| c.included && c.source_in <= mid && mid < c.source_out)
}

// ------------------------------------------------- heuristic windows

/// Every sentence window whose playable length fits the bounds. A window
/// starts and ends on a kept, non-filler sentence, and (when `beat_of` is
/// given) never crosses a beat boundary.
fn candidate_windows(sents: &[Sentence], beat_of: Option<&[usize]>, main: &Timeline, opts: &FindOptions) -> Vec<Window> {
    let usable: Vec<bool> = sents.iter().map(|s| !s.filler && kept(main, s)).collect();
    let mut out = vec![];
    for first in 0..sents.len() {
        if !usable[first] {
            continue;
        }
        for last in first..sents.len() {
            if let Some(beats) = beat_of {
                if beats[last] != beats[first] {
                    break;
                }
            }
            let (start, end) = (sents[first].start, sents[last].end);
            let duration = main.included_within(start, end);
            if duration > opts.max_s {
                break;
            }
            if usable[last] && duration >= opts.min_s {
                out.push(Window {
                    first,
                    last,
                    start,
                    end,
                    duration,
                    score: 0.0,
                    signals: vec![],
                    title: None,
                    reason: None,
                    judged: false,
                    scorer: None,
                    beat: beat_of.map(|b| b[first]).unwrap_or(0),
                });
            }
        }
    }
    out
}

/// Acknowledgements and tag questions ("Right?", "Okay.", "So yeah.") —
/// fine mid-clip, never the first or last thing a viewer hears.
const THROWAWAY: &[&str] = &[
    "right", "okay", "ok", "yeah", "yes", "so", "alright", "anyway", "well", "cool", "sure",
    "um", "uh", "and", "but",
];

fn is_throwaway(text: &str) -> bool {
    let words: Vec<String> = text.split_whitespace().map(first_word).collect();
    !words.is_empty() && words.len() <= 2 && words.iter().all(|w| THROWAWAY.contains(&w.as_str()))
}

/// Openers that need the previous sentence to make sense.
const DANGLING: &[&str] = &[
    "and", "but", "so", "because", "which", "also", "anyway", "then", "or", "plus", "though",
];
/// Pronouns with no antecedent when the clip starts cold.
const COLD_REFERENCE: &[&str] = &["it", "that", "they", "these", "those", "he", "she"];
const BACKREF: &[&str] = &[
    "as i said", "like i said", "as i mentioned", "i mentioned", "earlier", "going back to",
    "back to the",
];
const HOOK_OPENERS: &[&str] = &[
    "here's", "here is", "the problem", "the key", "the secret", "the truth", "the biggest",
    "the best", "the worst", "stop", "never", "most people", "if you", "what if", "why", "how",
    "nobody", "everyone", "you", "imagine", "today",
];
/// A next sentence opening with one of these finishes the thought a window
/// stops on ("…is it dying?" / "Or is the fear…").
const CONTINUES: &[&str] = &["or", "because", "which", "unless", "except"];
/// "One, …" right after "…two things that could happen." starts mid-list.
const LIST_OPENERS: &[&str] = &["one", "first", "firstly"];
const LIST_NOUNS: &[&str] = &[
    "things", "ways", "reasons", "steps", "options", "scenarios", "possibilities", "lessons", "rules", "mistakes",
];

/// Hesitations that can lead a whisper segment ("Um, this is…").
const OPENING_FILLERS: &[&str] = &["um", "uh", "er", "erm", "ah", "hmm", "mm"];

/// The line with leading hesitations removed (not re-capitalised).
fn after_fillers(text: &str) -> &str {
    let mut rest = text.trim();
    loop {
        let word = first_word(rest);
        if word.is_empty() || !OPENING_FILLERS.contains(&word.as_str()) {
            break;
        }
        let after = rest.split_once(char::is_whitespace).map(|(_, r)| r).unwrap_or("");
        rest = after.trim_start_matches([',', '.', ' ', '…', '-']).trim_start();
    }
    rest
}

/// The line with leading hesitations removed and re-capitalised — what a
/// viewer actually hears once the filler is cut, and what a title should say.
fn strip_leading_fillers(text: &str) -> String {
    let mut chars = after_fillers(text).chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn first_word(text: &str) -> String {
    text.split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
        .to_lowercase()
}

fn starts_lowercase(text: &str) -> bool {
    text.chars().find(|c| c.is_alphabetic()).is_some_and(char::is_lowercase)
}

/// The nearest kept, non-filler sentence before (or after) `i`.
fn neighbor<'a>(sents: &'a [Sentence], main: &Timeline, i: usize, forward: bool) -> Option<&'a Sentence> {
    let usable = |s: &&Sentence| kept(main, s) && !s.filler;
    if forward {
        sents[i + 1..].iter().find(usable)
    } else {
        sents[..i].iter().rev().find(usable)
    }
}

/// 0..100 short-form potential without a model, plus the signals behind it
/// (they become the human-readable reason).
fn heuristic_score(
    sents: &[Sentence],
    main: &Timeline,
    w: &Window,
    beats: &[Beat],
    beat_of: &[usize],
    opts: &FindOptions,
) -> (f64, Vec<&'static str>) {
    let mut score: f64 = 50.0;
    let mut signals = vec![];
    let raw_open = sents[w.first].text.trim();
    if OPENING_FILLERS.contains(&first_word(raw_open).as_str()) {
        score -= 10.0;
        signals.push("opens on a filler");
    }
    let body = after_fillers(raw_open);
    let open_lc = body.to_lowercase();
    let close = sents[w.last].text.trim();

    let opener = first_word(body);
    if starts_lowercase(body) {
        score -= 20.0;
        signals.push("opens mid-sentence");
    } else if DANGLING.contains(&opener.as_str()) {
        score -= 20.0;
        signals.push("opens mid-thought");
    } else if COLD_REFERENCE.contains(&opener.as_str()) {
        score -= 8.0;
        signals.push("opens on an unexplained reference");
    }
    if BACKREF.iter().any(|p| open_lc.contains(p)) {
        score -= 15.0;
        signals.push("refers back to earlier");
    }
    // What came just before: a setup ("two things could happen." → "One,
    // …") or a question this opens by answering means the short starts
    // after its own beginning.
    if let Some(prev) = neighbor(sents, main, w.first, false) {
        let prev = prev.text.trim_end();
        let prev_lc = prev.to_lowercase();
        let lists = LIST_OPENERS.contains(&opener.as_str()) && LIST_NOUNS.iter().any(|n| prev_lc.contains(n));
        if prev.ends_with(':') || lists {
            score -= 15.0;
            signals.push("starts after its setup");
        } else if prev.ends_with('?') && !body.contains('?') {
            score -= 10.0;
            signals.push("answers a question it leaves out");
        }
    }
    if body.contains('?') && body.split_whitespace().count() >= 4 {
        score += 12.0;
        signals.push("opens with a question");
    } else if HOOK_OPENERS.iter().any(|h| open_lc.starts_with(h)) {
        score += 10.0;
        signals.push("strong opener");
    }
    if body.chars().any(|c| c.is_ascii_digit()) {
        score += 4.0;
    }
    let next_continues = neighbor(sents, main, w.last, true)
        .is_some_and(|n| starts_lowercase(&n.text) || CONTINUES.contains(&first_word(&n.text).as_str()));
    if !ends_sentence(close) {
        score -= 8.0;
        signals.push("trails off");
    } else if next_continues {
        score -= 8.0;
        signals.push("stops before the thought ends");
    } else {
        score += 8.0;
        signals.push("ends on a complete thought");
    }

    if let Some(beat) = beats.get(beat_of[w.first]) {
        score += match beat.role.as_str() {
            "hook" => 12.0,
            "point" | "example" => 8.0,
            "recap" | "section" => 4.0,
            "setup" => 2.0,
            "tangent" => -4.0,
            "outro" => -12.0,
            _ => 0.0,
        };
        if beat.role == "hook" {
            signals.push("from the video's hook");
        }
        score += (3.0 - f64::from(beat.cut_priority)) * 3.0;
    }

    // Sweet spot: ~25-45s holds attention and leaves room for a payoff.
    let ideal_lo = 25.0_f64.clamp(opts.min_s, opts.max_s);
    let ideal_hi = 45.0_f64.clamp(opts.min_s, opts.max_s);
    let off = if w.duration < ideal_lo {
        ideal_lo - w.duration
    } else if w.duration > ideal_hi {
        w.duration - ideal_hi
    } else {
        0.0
    };
    score += 10.0 - (off / 3.0).min(10.0);

    let lines = &sents[w.first..=w.last];
    let fillers = lines.iter().filter(|s| s.filler).count();
    score -= 15.0 * fillers as f64 / lines.len() as f64;

    (score.clamp(0.0, 100.0), signals)
}

fn overlap(a: &Window, b: &Window) -> f64 {
    pieces_overlap(&a.pieces(), &b.pieces())
}

/// The same moment: sharing over `MAX_OVERLAP`, or — unless the model picked
/// both — heuristic slices touching inside one beat.
fn same_moment(a: &Window, b: &Window) -> bool {
    if overlap(a, b) > MAX_OVERLAP {
        return true;
    }
    let gap = (a.start - b.end).max(b.start - a.end);
    !(a.judged && b.judged) && a.beat == b.beat && gap < ADJACENT_S
}

impl Window {
    /// The pieces it plays (a single stretch: one).
    fn pieces(&self) -> [SourceRange; 1] {
        [SourceRange { start: self.start, end: self.end }]
    }
}

/// Best-first, dropping anything that is substantially the same moment as a
/// better pick.
fn diversify(mut windows: Vec<Window>, k: usize) -> Vec<Window> {
    windows.sort_by(|a, b| {
        b.judged
            .cmp(&a.judged)
            .then(b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal))
            // Ties: the earlier moment first (stable, readable order).
            .then(a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal))
    });
    let mut picked: Vec<Window> = vec![];
    for w in windows {
        if picked.len() >= k {
            break;
        }
        if picked.iter().all(|p| !same_moment(p, &w)) {
            picked.push(w);
        }
    }
    picked
}

// ------------------------------------------------------ model picks

/// What a pick request needs about the project.
struct PickContext<'a> {
    sents: &'a [Sentence],
    main: &'a Timeline,
    beats: &'a [Beat],
    beat_of: &'a [usize],
    /// Each sentence's relevance to the theme, when there is one.
    relevance: Option<&'a [f64]>,
    opts: &'a FindOptions,
}

impl Region {
    fn contains(&self, w: &Window) -> bool {
        self.first <= w.first && w.last <= self.last
    }
}

/// What a pick run did, for the app log.
#[derive(Default)]
struct Tally {
    /// Topics the model was asked about.
    topics: usize,
    /// Topics whose request failed or whose picks were all unusable, even
    /// on the second try: they keep their rule-scored windows.
    failed: usize,
    /// Topics asked a second time (the first answer failed or was unusable).
    retried: usize,
    /// Picks rejected, by why.
    too_long: usize,
    too_short: usize,
    bad_lines: usize,
    /// The models that answered.
    models: Vec<String>,
}

/// The model picks the shorts topic by topic. A scan reads every topic; a
/// plain run the ones whose best heuristic window scores highest. A topic
/// whose request fails or whose picks are all unusable keeps its heuristic
/// windows, and so do topics a plain run didn't ask about; they rank after
/// the picks. `None`: the model picked nothing usable anywhere (the caller
/// falls back to the heuristic list rather than show nothing).
async fn pick_all(
    editor: &Editor,
    project_id: Uuid,
    inference: &dyn InferenceClient,
    ctx: &PickContext<'_>,
    windows: &[Window],
    tally: &mut Tally,
) -> Result<Option<Vec<Window>>> {
    let model = editor.get_preferences()?.inference_model;
    let label = editor.model_label();
    let regions = choose_regions(pick_regions(ctx.sents, ctx.beat_of, ctx.main, ctx.opts), windows, ctx.opts);
    let total = regions.len();
    tally.topics = total;
    let mut out: Vec<Window> = vec![];
    for (n, region) in regions.iter().enumerate() {
        progress(
            editor,
            project_id,
            0.3 + 0.65 * n as f64 / total.max(1) as f64,
            format!("picking shorts with {label} — topic {}/{total}", n + 1),
        );
        let fallback = || windows.iter().filter(|w| region.contains(w)).cloned();
        let mut answer = pick_shorts(inference, &model, ctx, region, None, tally).await;
        // One more try, told what was wrong with the first answer.
        let note = match &answer {
            Ok(Picked::Unusable(note)) => Some(note.clone()),
            Err(e) if !e.is_fatal_model_error() => Some(String::new()),
            _ => None,
        };
        if let Some(note) = note {
            tally.retried += 1;
            answer = pick_shorts(inference, &model, ctx, region, Some(&note).filter(|n| !n.is_empty()).map(String::as_str), tally).await;
        }
        match answer {
            Ok(Picked::Usable(picks)) => out.extend(picks),
            // An account problem stops the scan (the stored candidates stay
            // as they were); anything else about this topic keeps its
            // heuristic windows and the rest still count.
            Err(e) if e.is_fatal_model_error() => return Err(e),
            Ok(Picked::Unusable(_)) | Err(_) => {
                tally.failed += 1;
                out.extend(fallback());
            }
        }
    }
    if !out.iter().any(|w| w.judged) {
        return Ok(None);
    }
    out.extend(windows.iter().filter(|w| !regions.iter().any(|r| r.contains(w))).cloned());
    Ok(Some(out))
}

/// The longest region one request covers, and how much consecutive parts of
/// a long one overlap: a short's length, so any short fits whole in some
/// part — but never more than half a region, so each part moves on.
fn region_bounds(opts: &FindOptions) -> (f64, f64) {
    let max = REGION_MAX_S.max(2.0 * opts.max_s);
    (max, opts.max_s.min(max / 2.0))
}

/// The pick requests: runs of sentences in one beat, joined while shorter
/// than a max-length short, and long ones split into overlapping parts
/// (`region_bounds`).
fn pick_regions(sents: &[Sentence], beat_of: &[usize], main: &Timeline, opts: &FindOptions) -> Vec<Region> {
    let (region_max, overlap) = region_bounds(opts);
    let len = |a: usize, b: usize| main.included_within(sents[a].start, sents[b].end);
    let mut runs: Vec<Region> = vec![];
    for i in 0..sents.len() {
        match runs.last_mut() {
            Some(r) if beat_of[r.last] == beat_of[i] => r.last = i,
            _ => runs.push(Region { first: i, last: i }),
        }
    }
    let mut merged: Vec<Region> = vec![];
    for r in runs {
        match merged.last_mut() {
            Some(m) if len(m.first, m.last) < opts.max_s => m.last = r.last,
            _ => merged.push(r),
        }
    }
    if merged.len() >= 2 {
        let last = merged[merged.len() - 1];
        if len(last.first, last.last) < opts.max_s {
            merged.pop();
            merged.last_mut().unwrap().last = last.last;
        }
    }
    let mut out = vec![];
    for r in merged {
        let mut a = r.first;
        loop {
            let mut b = a;
            while b < r.last && len(a, b + 1) <= region_max {
                b += 1;
            }
            out.push(Region { first: a, last: b });
            if b >= r.last {
                break;
            }
            let mut next = b;
            while next > a + 1 && len(next - 1, b) <= overlap {
                next -= 1;
            }
            a = next.max(a + 1);
        }
    }
    out
}

/// The regions worth a request: ones holding at least one usable window
/// (on theme, not already saved). A scan takes them all (best-scoring
/// first past `SCAN_REGIONS`); a plain run the best few, skipping a region
/// whose best moment a better one already covers (the overlapping parts of
/// a long beat). In video order.
fn choose_regions(regions: Vec<Region>, windows: &[Window], opts: &FindOptions) -> Vec<Region> {
    let best = |r: &Region| {
        windows
            .iter()
            .filter(|w| r.contains(w))
            .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal))
    };
    let mut scored: Vec<(Region, &Window)> = regions.into_iter().filter_map(|r| best(&r).map(|w| (r, w))).collect();
    let cap = if opts.scan { SCAN_REGIONS } else { opts.count.div_ceil(2).clamp(4, 12) };
    if scored.len() > cap {
        scored.sort_by(|a, b| b.1.score.partial_cmp(&a.1.score).unwrap_or(std::cmp::Ordering::Equal));
        let mut chosen: Vec<(Region, &Window)> = vec![];
        for (r, w) in scored {
            if chosen.len() < cap && !chosen.iter().any(|(c, _)| c.contains(w)) {
                chosen.push((r, w));
            }
        }
        scored = chosen;
    }
    scored.sort_by_key(|(r, _)| r.first);
    scored.into_iter().map(|(r, _)| r).collect()
}

fn clock(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// The request text for one region: two lines of context before it, then
/// its kept sentences numbered from 1 with the playable time each starts
/// at, a marker where the edit cut speech, and the region's end time.
/// Returns the sentence index behind each line number.
fn pick_prompt(sents: &[Sentence], main: &Timeline, r: &Region) -> (Vec<usize>, String) {
    let mut text = String::new();
    let mut before: Vec<&Sentence> =
        sents[..r.first].iter().rev().filter(|s| kept(main, s) && !s.filler).take(2).collect();
    before.reverse();
    if !before.is_empty() {
        text.push_str("Just before this topic (context only — not pickable):\n");
        for s in before {
            text.push_str(&format!("   {}\n", s.text));
        }
        text.push('\n');
    }
    text.push_str("This topic:\n");
    let origin = sents[r.first].start;
    let mut lines = vec![];
    let mut cut_marked = false;
    for (i, s) in sents.iter().enumerate().take(r.last + 1).skip(r.first) {
        if !kept(main, s) {
            if !s.filler && !cut_marked {
                text.push_str("   — cut in the edit —\n");
                cut_marked = true;
            }
            continue;
        }
        cut_marked = false;
        lines.push(i);
        text.push_str(&format!("{}| [{}] {}\n", lines.len(), clock(main.included_within(origin, s.start)), s.text));
    }
    text.push_str(&format!("[{}] end of topic\n", clock(main.included_within(origin, sents[r.last].end))));
    (lines, text)
}

fn pick_system(opts: &FindOptions, theme: Option<&str>) -> String {
    let mut system = format!(
        "You are a short-form video editor (YouTube Shorts, Reels, TikTok). Below is one topic \
         from a longer talking-head video, one sentence per numbered line, each with the time it \
         starts at. Pick up to {PICKS_PER_REGION} moments that work as STANDALONE shorts of \
         {min:.0}-{max:.0} seconds (a short runs from its first line's time to the time of the \
         line after its last). Choose each short's FIRST and LAST line with care. The first line \
         must make sense to someone who saw nothing before it: start where the idea starts, with \
         any setup it needs — if a line answers a question, continues a list (\"two things…\"), \
         or carries on the line before it, start earlier. The last line must land the idea (the \
         payoff, punchline or conclusion); never stop on a line the next one finishes. A short is \
         a run of consecutive lines; never split one idea across two shorts, and don't overlap \
         them. Lines under \"Just before this topic\" are context only. Rate each pick 1-5 on: \
         hook (the first line stops the scroll), standalone (makes sense with zero context), \
         payoff (an insight, surprise, strong opinion or laugh), ending (ends on a complete, \
         satisfying thought). Be harsh: most moments are not worth posting, so return fewer \
         picks, or [] when nothing here works. Reply with ONLY a JSON array: [{{\"first\": line \
         number, \"last\": line number, \"hook\": 1-5, \"standalone\": 1-5, \"payoff\": 1-5, \
         \"ending\": 1-5, \"title\": a catchy title under 60 characters, \"reason\": one short \
         sentence on why it works or doesn't}}].",
        min = opts.min_s,
        max = opts.max_s,
    );
    if let Some(theme) = theme {
        system.push_str(&format!(
            " The user wants shorts ABOUT: {theme}. Pick only moments substantially about that; \
             return [] if this topic has none."
        ));
    }
    system
}

/// The rubric's four 1-5 ratings as one 0-100 score (all 1s → 0, all 5s → 100).
fn rubric_score(ratings: [f64; 4]) -> f64 {
    let sum: f64 = ratings.iter().map(|r| r.clamp(1.0, 5.0)).sum();
    (sum - 4.0) / 16.0 * 100.0
}

/// A pick's sentences once checked: line numbers in range and in order,
/// filler/throwaway edges moved inward, the playable length within bounds
/// (a second under `min_s` is let through). `None`: unusable.
fn check_pick(
    sents: &[Sentence],
    main: &Timeline,
    lines: &[usize],
    first: usize,
    last: usize,
    opts: &FindOptions,
) -> std::result::Result<(usize, usize, f64), Reject> {
    if first == 0 || first > last || last > lines.len() {
        return Err(Reject::Lines);
    }
    let (mut a, mut b) = (first - 1, last - 1);
    while a < b && sents[lines[a]].filler {
        a += 1;
    }
    while b > a && sents[lines[b]].filler {
        b -= 1;
    }
    let (a, b) = (lines[a], lines[b]);
    if sents[a].filler || sents[b].filler {
        return Err(Reject::Filler);
    }
    let duration = main.included_within(sents[a].start, sents[b].end);
    if duration > opts.max_s {
        Err(Reject::TooLong(duration))
    } else if duration < opts.min_s - 1.0 {
        Err(Reject::TooShort(duration))
    } else {
        Ok((a, b, duration))
    }
}

/// Why a pick can't be used: told to the model when it tries again, and
/// counted in the app log.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Reject {
    /// Line numbers out of range or backwards.
    Lines,
    /// Nothing but filler.
    Filler,
    TooLong(f64),
    TooShort(f64),
}

/// One topic's answer.
enum Picked {
    Usable(Vec<Window>),
    /// Every pick was rejected: what to tell the model on its second try.
    Unusable(String),
}

impl Picked {
    #[cfg(test)]
    fn usable(self) -> Option<Vec<Window>> {
        match self {
            Picked::Usable(p) => Some(p),
            Picked::Unusable(_) => None,
        }
    }
}

/// What to tell the model about picks that couldn't be used.
fn retry_note(rejected: &[(usize, usize, Reject)], lines: usize, opts: &FindOptions) -> String {
    let what: Vec<String> = rejected
        .iter()
        .map(|&(first, last, why)| {
            let picked = format!("lines {first}-{last}");
            match why {
                Reject::Lines => format!("{picked}: not a run of this topic's lines 1-{lines}"),
                Reject::Filler => format!("{picked}: only filler"),
                Reject::TooLong(s) => format!("{picked}: runs {s:.0} s, too long"),
                Reject::TooShort(s) => format!("{picked}: runs {s:.0} s, too short"),
            }
        })
        .collect();
    format!(
        "\nYour previous answer for this topic couldn't be used: {}. A short runs from its first \
         line's time to the time of the line after its last, and must be {:.0}-{:.0} seconds. Pick \
         again, or reply [] if nothing here works.",
        what.join("; "),
        opts.min_s,
        opts.max_s
    )
}

/// A line number from a reply: an integer, or a whole number written as a
/// float or a string ("3.0", "3"). Anything else (negative, fractional,
/// missing) is no line.
fn line_number(v: &serde_json::Value) -> Option<usize> {
    let n = match v {
        serde_json::Value::Number(n) => n.as_f64()?,
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    (n >= 0.0 && n.fract() == 0.0 && n <= u32::MAX as f64).then_some(n as usize)
}

/// A 1-5 rating from a reply (a number or a numeric string); 3 when missing.
fn rating(v: Option<&serde_json::Value>) -> f64 {
    match v {
        Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(3.0),
        Some(serde_json::Value::String(s)) => s.trim().parse().unwrap_or(3.0),
        _ => 3.0,
    }
}

/// One region's picks, checked, at most `PICKS_PER_REGION` (the best by
/// score). Each pick is read on its own, so one malformed pick doesn't cost
/// the rest. `Ok(None)`: the model picked things but none were usable (the
/// caller keeps the region's heuristic windows). `[]` from the model is an
/// answer: nothing in this topic is worth posting.
async fn pick_shorts(
    inference: &dyn InferenceClient,
    model: &str,
    ctx: &PickContext<'_>,
    region: &Region,
    retry: Option<&str>,
    tally: &mut Tally,
) -> Result<Picked> {
    let PickContext { sents, main, beats, beat_of, relevance, opts } = *ctx;
    let (lines, mut user) = pick_prompt(sents, main, region);
    if let Some(note) = retry {
        user.push_str(note);
    }
    let system = pick_system(opts, opts.theme.as_deref());
    let schema = short_picks_schema();
    let (replies, served): (Vec<serde_json::Value>, _) =
        crate::llm::ask_json_schema_served(inference, model, "short_picks", &system, &user, 0.2, Some(&schema)).await?;
    if let Some(s) = &served {
        if !tally.models.contains(&s.model) {
            tally.models.push(s.model.clone());
        }
    }
    let scorer = served.map(|s| scorer_name(&s));
    let mut picks = vec![];
    let mut rejected: Vec<(usize, usize, Reject)> = vec![];
    let mut off_theme = 0;
    for p in &replies {
        let (first, last) = (p.get("first").and_then(line_number), p.get("last").and_then(line_number));
        let (Some(first), Some(last)) = (first, last) else {
            if p.is_object() {
                rejected.push((first.unwrap_or(0), last.unwrap_or(0), Reject::Lines));
            }
            continue;
        };
        let (first, last, duration) = match check_pick(sents, main, &lines, first, last, opts) {
            Ok(ok) => ok,
            Err(why) => {
                rejected.push((first, last, why));
                continue;
            }
        };
        let text = |key: &str| p.get(key).and_then(|v| v.as_str()).map(str::trim).filter(|t| !t.is_empty()).map(str::to_string);
        let mut w = Window {
            first,
            last,
            start: sents[first].start,
            end: sents[last].end,
            duration,
            score: 0.0,
            signals: vec![],
            title: text("title"),
            reason: text("reason"),
            judged: true,
            scorer: scorer.clone(),
            beat: beat_of[first],
        };
        let (heuristic, signals) = heuristic_score(sents, main, &w, beats, beat_of, opts);
        let ratings = ["hook", "standalone", "payoff", "ending"].map(|k| rating(p.get(k)));
        // The model's rubric dominates; the heuristic breaks near-ties.
        w.score = 0.8 * rubric_score(ratings) + 0.2 * heuristic;
        w.signals = signals;
        if let Some(rel) = relevance {
            if !on_theme(rel, &mut w) {
                off_theme += 1;
                continue;
            }
        }
        picks.push(w);
    }
    for &(_, _, why) in &rejected {
        match why {
            Reject::TooLong(_) => tally.too_long += 1,
            Reject::TooShort(_) => tally.too_short += 1,
            Reject::Lines | Reject::Filler => tally.bad_lines += 1,
        }
    }
    if picks.is_empty() && !replies.is_empty() {
        return Ok(Picked::Unusable(if !rejected.is_empty() {
            retry_note(&rejected, lines.len(), opts)
        } else if off_theme > 0 {
            "\nYour previous picks for this topic weren't about the theme: pick only moments about \
             it, or reply [] if this topic has none."
                .into()
        } else {
            "\nYour previous answer for this topic couldn't be read: reply with ONLY the JSON array.".into()
        }));
    }
    picks.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    picks.truncate(PICKS_PER_REGION);
    Ok(Picked::Usable(picks))
}

/// How a candidate names the model that picked it: "Claude Opus 5.5", or
/// the local model's own name.
fn scorer_name(s: &crate::adapters::inference::ServedBy) -> String {
    if s.hosted {
        crate::model::hosted_model_name(&s.model)
    } else {
        s.model.clone()
    }
}

/// What the model returns for one topic: its picks, each with the rubric,
/// a title and a reason.
pub(crate) fn short_picks_schema() -> serde_json::Value {
    let rating = serde_json::json!({"type": "integer", "minimum": 1, "maximum": 5});
    serde_json::json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "first": {"type": "integer"},
                "last": {"type": "integer"},
                "hook": rating,
                "standalone": rating,
                "payoff": rating,
                "ending": rating,
                "title": {"type": "string"},
                "reason": {"type": "string"}
            },
            "required": ["first", "last", "hook", "standalone", "payoff", "ending", "title", "reason"]
        }
    })
}

fn heuristic_title(open: &str) -> String {
    let words: Vec<&str> = open.split_whitespace().collect();
    let mut title = words.iter().take(8).copied().collect::<Vec<_>>().join(" ");
    title = title.trim_end_matches([',', '.', ';', ':']).to_string();
    if words.len() > 8 {
        title.push('…');
    }
    title
}

fn to_candidate(
    speech: &[TranscriptSegment],
    sents: &[Sentence],
    w: &Window,
    method: &str,
    theme: Option<&str>,
) -> ClipCandidate {
    let open = strip_leading_fillers(&sents[w.first].text);
    let reason = w.reason.clone().unwrap_or_else(|| {
        let mut parts: Vec<String> = w.signals.iter().map(|s| s.to_string()).collect();
        parts.push(format!("{:.0}s", w.duration));
        let mut r = parts.join("; ");
        if let Some(c) = r.get_mut(0..1) {
            c.make_ascii_uppercase();
        }
        r
    });
    let (seg_first, seg_last) = (sents[w.first].seg_first, sents[w.last].seg_last);
    ClipCandidate {
        id: Uuid::new_v4(),
        start: w.start,
        end: w.end,
        pieces: w.pieces().to_vec(),
        duration: w.duration,
        title: w.title.clone().unwrap_or_else(|| heuristic_title(&open)),
        hook: open,
        reason,
        score: (w.score * 10.0).round() / 10.0,
        segment_ids: speech[seg_first..=seg_last].iter().map(|s| s.id).collect(),
        method: method.to_string(),
        theme: theme.map(str::to_string),
        scorer: w.scorer.clone(),
        framing: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::inference::{ChatMessage, ChatRequest, ChatResponse};
    use crate::model::{ClipOrigin, Word};

    fn seg(start: f64, end: f64, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            id: Uuid::new_v4(),
            start,
            end,
            text: text.into(),
            words: vec![],
            is_filler: matches!(text, "um" | "uh"),
            is_silence: false,
            take_group_id: None,
            is_best_take: false,
        }
    }

    /// Ten 6-second sentences back to back over a 60s source.
    fn speech() -> Vec<TranscriptSegment> {
        let lines = [
            "Here's the biggest mistake people make when they edit.",
            "And they keep doing it for years.",
            "um",
            "The fix is to cut on the transcript, not the waveform.",
            "It sounds simple.",
            "But it saves hours every single week.",
            "As I said earlier, the timeline is a trap.",
            "Why do we still drag clips around by hand?",
            "Because nobody showed us a better way.",
            "So try it on your next video",
        ];
        lines.iter().enumerate().map(|(i, l)| seg(i as f64 * 6.0, i as f64 * 6.0 + 6.0, l)).collect()
    }

    /// Sentences of an uncut transcript.
    fn sents_of(speech: &[TranscriptSegment]) -> Vec<Sentence> {
        sentences(speech, &Timeline::new(100_000.0))
    }

    fn opts(min_s: f64, max_s: f64) -> FindOptions {
        FindOptions { count: 5, min_s, max_s, ..Default::default() }
    }

    #[test]
    fn segments_are_re_cut_into_sentences() {
        // Whisper's segments stop mid-sentence and hold several sentences.
        let mut a = seg(0.0, 6.0, "AI took my job. Because before I hired them, I had to do all that work");
        let b = seg(6.0, 10.0, "myself and I couldn't, right? That took all day.");
        // Word timings that spell the text: the first sentence ends at 1.5s.
        let tokens = [
            ("AI", 0.0, 0.4), ("took", 0.4, 0.8), ("my", 0.8, 1.0), ("job", 1.0, 1.4), (".", 1.4, 1.5),
            ("Because", 1.9, 2.3), ("before", 2.3, 2.6), ("I", 2.6, 2.7), ("hired", 2.7, 3.0),
            ("them", 3.0, 3.2), (",", 3.2, 3.2), ("I", 3.3, 3.4), ("had", 3.4, 3.6), ("to", 3.6, 3.7),
            ("do", 3.7, 3.9), ("all", 3.9, 4.2), ("that", 4.2, 4.6), ("work", 4.6, 6.0),
        ];
        a.words = tokens.iter().map(|(t, s, e)| Word { text: t.to_string(), start: *s, end: *e, confidence: 1.0 }).collect();
        let s = sents_of(&[a, b]);
        let texts: Vec<&str> = s.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "AI took my job.",
                "Because before I hired them, I had to do all that work myself and I couldn't, right?",
                "That took all day.",
            ]
        );
        // Times come from the words, with a short tail and lead-in in the gap.
        assert!((s[0].end - 1.65).abs() < 1e-9, "{}", s[0].end);
        assert!((s[1].start - 1.8).abs() < 1e-9, "{}", s[1].start);
        assert_eq!((s[1].seg_first, s[1].seg_last), (0, 1));
        // No word timings: by character position.
        let c = sents_of(&[seg(0.0, 10.0, "One two three four. Five six seven eight.")]);
        assert_eq!(c.len(), 2);
        assert!((c[0].end - 5.0).abs() < 0.6 && (c[1].start - 5.0).abs() < 0.6, "{c:?}");
        // Abbreviations, decimals and initials don't end a sentence.
        assert_eq!(sents_of(&[seg(0.0, 5.0, "Dr. Smith paid 3.5 dollars, e.g. a coffee. Done.")]).len(), 2);
    }

    fn words(list: &[(&str, f64, f64)]) -> Vec<Word> {
        list.iter().map(|(t, s, e)| Word { text: t.to_string(), start: *s, end: *e, confidence: 1.0 }).collect()
    }

    #[test]
    fn sentence_edges_fall_between_words_when_the_text_was_cleaned_up() {
        // The clean-up added punctuation and capitals the word list doesn't have.
        let mut a = seg(10.0, 14.0, "Added together. I could also, uh, ask it.");
        a.words = words(&[
            ("added", 10.0, 10.3), ("together", 10.3, 10.9), ("i", 11.4, 11.5), ("could", 11.5, 11.8),
            ("also", 11.8, 12.1), ("uh", 12.2, 12.4), ("ask", 12.5, 12.8), ("it", 12.8, 13.9),
        ]);
        let s = sents_of(&[a]);
        assert_eq!(s.len(), 2);
        // "together" ends at 10.9 and "i" starts at 11.4: both edges in that gap.
        assert!((10.9..=11.4).contains(&s[0].end) && (10.9..=11.4).contains(&s[1].start), "{s:?}");
        // The clean-up changed a word ("can 't" became "can"): still between words.
        let mut b = seg(20.0, 24.0, "What I can do now. In weeks, that took a year.");
        b.words = words(&[
            ("what", 20.0, 20.2), ("i", 20.2, 20.3), ("can", 20.3, 20.5), ("'t", 20.5, 20.6), ("do", 20.6, 20.8),
            ("now", 20.8, 21.2), ("in", 21.6, 21.7), ("weeks", 21.7, 22.1), ("that", 22.2, 22.4),
            ("took", 22.4, 22.7), ("a", 22.7, 22.8), ("year", 22.8, 23.6),
        ]);
        let s = sents_of(&[b]);
        assert!((21.2..=21.6).contains(&s[0].end) && (21.2..=21.6).contains(&s[1].start), "{s:?}");
        // A segment that ends before its last word does keeps the whole word.
        let mut c = seg(30.0, 31.0, "That's my point.");
        c.words = words(&[("that", 30.0, 30.3), ("'s", 30.3, 30.4), ("my", 30.4, 30.6), ("point", 30.6, 31.3)]);
        assert!(sents_of(&[c])[0].end >= 31.3);
    }

    #[test]
    fn fillers_pauses_and_run_ons_end_a_sentence() {
        let s = sents_of(&[
            seg(0.0, 4.0, "So the thing is"),
            seg(4.0, 5.0, "um"),
            seg(5.0, 9.0, "nobody tells you this"),
            seg(11.0, 14.0, "and then it happened."),
            seg(14.0, 15.0, "Right?"),
        ]);
        let texts: Vec<&str> = s.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["So the thing is", "um", "nobody tells you this", "and then it happened.", "Right?"]);
        assert_eq!(s.iter().map(|s| s.filler).collect::<Vec<_>>(), vec![false, true, false, false, true]);
        let long: Vec<TranscriptSegment> = (0..6).map(|i| seg(i as f64 * 5.0, i as f64 * 5.0 + 5.0, "on and on without a stop")).collect();
        let s = sents_of(&long);
        assert!(s.iter().all(|s| s.end - s.start <= RUN_ON_S + 5.0 + TAIL_S + LEAD_S), "{s:?}");
    }

    #[test]
    fn windows_are_sentence_aligned_and_skip_fillers_and_cut_material() {
        let speech = speech();
        let sents = sents_of(&speech);
        assert_eq!(sents.len(), speech.len(), "one sentence per segment here");
        let mut main = Timeline::new(60.0);
        // Cut "It sounds simple." (24-30s) in the main edit.
        main.set_range_included(24.0, 30.0, false, ClipOrigin::Manual);
        let ws = candidate_windows(&sents, None, &main, &opts(10.0, 20.0));
        assert!(!ws.is_empty());
        for w in &ws {
            assert!(!sents[w.first].filler && !sents[w.last].filler);
            assert_ne!(w.first, 4, "never starts on a sentence the main cut removed");
            assert_ne!(w.last, 4, "never ends on a sentence the main cut removed");
            assert!((10.0..=20.0).contains(&w.duration), "duration {}", w.duration);
        }
        // The cut doesn't count toward length: 18-36s spans 18s of source
        // but only 12s plays.
        let w = ws.iter().find(|w| w.first == 3 && w.last == 5).expect("window over the cut");
        assert!((w.duration - 12.0).abs() < 1e-6);
    }

    #[test]
    fn windows_stay_inside_a_beat() {
        let sents = sents_of(&speech());
        let main = Timeline::new(60.0);
        let beat_of: Vec<usize> = (0..sents.len()).map(|i| if i < 5 { 0 } else { 1 }).collect();
        let ws = candidate_windows(&sents, Some(&beat_of), &main, &opts(10.0, 40.0));
        assert!(ws.iter().all(|w| beat_of[w.first] == beat_of[w.last] && w.beat == beat_of[w.first]));
    }

    #[test]
    fn heuristic_rewards_hooks_and_punishes_cold_opens() {
        let sents = sents_of(&speech());
        let main = Timeline::new(60.0);
        let ws = candidate_windows(&sents, None, &main, &opts(10.0, 20.0));
        let score_of = |first: usize, last: usize| {
            let w = ws.iter().find(|w| w.first == first && w.last == last).unwrap();
            heuristic_score(&sents, &main, w, &[], &vec![0; sents.len()], &opts(10.0, 20.0))
        };
        let (hook, hook_signals) = score_of(0, 1); // "Here's the biggest mistake…"
        let (dangling, _) = score_of(1, 3); // "And they keep doing it…"
        let (backref, back_signals) = score_of(6, 7); // "As I said earlier…"
        assert!(hook > dangling, "{hook} vs {dangling}");
        assert!(hook > backref, "{hook} vs {backref}");
        assert!(hook_signals.contains(&"strong opener"));
        assert!(back_signals.contains(&"refers back to earlier"));
        let (question, q_signals) = score_of(7, 8);
        assert!(q_signals.contains(&"opens with a question"), "{question}");
        let (trailing, t_signals) = score_of(8, 9); // ends without punctuation
        assert!(t_signals.contains(&"trails off"), "{trailing}");
        // "Because nobody showed us…" answers the question before it.
        let (_, a_signals) = score_of(8, 9);
        assert!(a_signals.contains(&"answers a question it leaves out"), "{a_signals:?}");
    }

    #[test]
    fn heuristic_sees_a_missing_setup_and_an_unfinished_ending() {
        let sents = sents_of(&[
            seg(0.0, 5.0, "There's literally two things that could happen."),
            seg(5.0, 20.0, "One, we're all screwed."),
            seg(20.0, 30.0, "Is software engineering dying?"),
            seg(30.0, 35.0, "Or is the fear that I can't pay rent?"),
            seg(35.0, 40.0, "nobody knows the answer."),
        ]);
        let main = Timeline::new(40.0);
        let o = opts(4.0, 40.0);
        let ws = candidate_windows(&sents, None, &main, &o);
        let signals = |first: usize, last: usize| {
            let w = ws.iter().find(|w| w.first == first && w.last == last).unwrap();
            heuristic_score(&sents, &main, w, &[], &vec![0; sents.len()], &o).1
        };
        assert!(signals(1, 2).contains(&"starts after its setup"), "{:?}", signals(1, 2));
        assert!(signals(1, 2).contains(&"stops before the thought ends"), "the next line starts with 'Or'");
        assert!(signals(0, 3).contains(&"stops before the thought ends"), "the next line starts lowercase");
        assert!(signals(4, 4).contains(&"opens mid-sentence"));
        assert!(!signals(0, 1).contains(&"starts after its setup"));
    }

    #[test]
    fn leading_fillers_are_penalised_and_stripped_from_the_hook() {
        assert_eq!(strip_leading_fillers("Um, this is huge."), "This is huge.");
        assert_eq!(strip_leading_fillers("uh um, so here's the thing"), "So here's the thing");
        assert_eq!(strip_leading_fillers("Umbrellas are great."), "Umbrellas are great.");
        assert_eq!(strip_leading_fillers("um"), "");

        let speech = vec![
            seg(0.0, 10.0, "Um, here's the biggest mistake people make."),
            seg(10.0, 20.0, "It costs hours."),
            seg(20.0, 30.0, "Here's the biggest mistake people make."),
            seg(30.0, 40.0, "It costs hours."),
        ];
        let sents = sents_of(&speech);
        let main = Timeline::new(40.0);
        let ws = candidate_windows(&sents, None, &main, &opts(15.0, 25.0));
        let score_of = |first: usize| {
            let w = ws.iter().find(|w| w.first == first && w.last == first + 1).unwrap();
            heuristic_score(&sents, &main, w, &[], &vec![0; sents.len()], &opts(15.0, 25.0))
        };
        let (with_filler, signals) = score_of(0);
        let (clean, _) = score_of(2);
        assert!(signals.contains(&"opens on a filler"));
        assert!(signals.contains(&"strong opener"), "the hook still counts once the filler is cut");
        assert!(clean > with_filler);
        let w = ws.iter().find(|w| w.first == 0 && w.last == 1).unwrap();
        let c = to_candidate(&speech, &sents, w, "heuristic", None);
        assert_eq!(c.hook, "Here's the biggest mistake people make.");
        assert!(c.title.starts_with("Here's the biggest"));
        assert_eq!(c.segment_ids, vec![speech[0].id, speech[1].id]);
    }

    #[test]
    fn theme_relevance_needs_a_hit_and_rewards_staying_on_topic() {
        let speech = speech();
        let sents = sents_of(&speech);
        let main = Timeline::new(60.0);
        let ws = candidate_windows(&sents, None, &main, &opts(10.0, 20.0));
        // The search returned two segments: 3 (best) and 5.
        let hits = vec![(speech[3].id, 0.04), (speech[5].id, 0.02)];
        let rel = sentence_relevance(&sents, &segment_relevance(&speech, &hits));
        assert_eq!(rel[3], 1.0);
        assert_eq!(rel[5], 0.5);
        assert_eq!(rel[0], 0.0);
        let window = |first: usize, last: usize| ws.iter().find(|w| w.first == first && w.last == last).unwrap();
        assert_eq!(window_relevance(&rel, window(0, 1)), 0.0, "no hit → off theme");
        let both = window_relevance(&rel, window(3, 5));
        let one = window_relevance(&rel, window(5, 6));
        assert!(both > one, "{both} vs {one}");
    }

    fn win(first: usize, start: f64, end: f64, score: f64, judged: bool) -> Window {
        Window {
            first,
            last: first,
            start,
            end,
            duration: end - start,
            score,
            signals: vec![],
            title: None,
            reason: None,
            judged,
            scorer: None,
            beat: 0,
        }
    }

    #[test]
    fn diversify_drops_the_same_moment() {
        let picked =
            diversify(vec![win(0, 0.0, 30.0, 60.0, false), win(0, 2.0, 32.0, 70.0, false), win(0, 40.0, 70.0, 50.0, false)], 5);
        assert_eq!(picked.len(), 2);
        assert_eq!(picked[0].score, 70.0);
        assert_eq!(picked[1].start, 40.0);
        assert_eq!(diversify(vec![win(0, 0.0, 10.0, 1.0, false), win(0, 20.0, 30.0, 2.0, false)], 1).len(), 1);
    }

    #[test]
    fn heuristic_slices_of_one_idea_are_one_moment_but_model_picks_are_not() {
        // Back to back in one beat: the setup, then the payoff.
        let slices = vec![win(0, 0.0, 30.0, 60.0, false), win(1, 30.5, 60.0, 50.0, false)];
        assert_eq!(diversify(slices, 5).len(), 1);
        // In different beats, they're different topics.
        let mut other = win(1, 30.5, 60.0, 50.0, false);
        other.beat = 1;
        assert_eq!(diversify(vec![win(0, 0.0, 30.0, 60.0, false), other], 5).len(), 2);
        // The model picked both as separate ideas: trust it.
        assert_eq!(diversify(vec![win(0, 0.0, 30.0, 60.0, true), win(1, 30.5, 60.0, 50.0, true)], 5).len(), 2);
    }

    #[test]
    fn scan_pool_takes_every_beat_before_any_beat_twice() {
        // Beat 0 has three strong windows, beats 1 and 2 one weak window each.
        let beat_of = vec![0, 0, 0, 1, 2];
        let windows = vec![
            win(0, 0.0, 20.0, 90.0, false),
            win(1, 30.0, 50.0, 85.0, false),
            win(2, 60.0, 80.0, 80.0, false),
            win(3, 100.0, 120.0, 20.0, false),
            win(4, 130.0, 150.0, 10.0, false),
        ];
        let pool = round_robin_pool(windows.clone(), &beat_of, 3);
        let firsts: Vec<usize> = pool.iter().map(|w| w.first).collect();
        assert_eq!(firsts, vec![0, 3, 4], "the weak beats get in before beat 0's second best");
        // A bigger cap then takes the second round.
        let pool = round_robin_pool(windows, &beat_of, 10);
        assert_eq!(pool.iter().map(|w| w.first).collect::<Vec<_>>(), vec![0, 3, 4, 1, 2]);
    }

    #[test]
    fn a_round_cut_off_by_the_cap_keeps_its_best_not_its_earliest() {
        // Five beats, one window each, the best ones late in the video.
        let beat_of = vec![0, 1, 2, 3, 4];
        let windows: Vec<Window> =
            (0..5).map(|i| win(i, i as f64 * 100.0, i as f64 * 100.0 + 20.0, 10.0 * i as f64, false)).collect();
        let pool = round_robin_pool(windows, &beat_of, 2);
        assert_eq!(pool.iter().map(|w| w.first).collect::<Vec<_>>(), vec![4, 3]);
    }

    #[test]
    fn the_heuristic_pool_by_mode() {
        let beat_of = vec![0; 40];
        let windows: Vec<Window> =
            (0..40).map(|i| win(i, i as f64 * 30.0, i as f64 * 30.0 + 20.0, i as f64, false)).collect();
        let ids = |p: Vec<Window>| p.iter().map(|w| w.first).collect::<Vec<_>>();
        let plain = FindOptions { count: 8, ..Default::default() };
        assert_eq!(ids(build_pool(windows.clone(), &beat_of, true, &plain)), ids(diversify(windows.clone(), POOL_MAX)));
        // A scan: nothing is cut off.
        let scan = FindOptions { scan: true, ..Default::default() };
        let many: Vec<Window> =
            (0..120).map(|i| win(i, i as f64 * 30.0, i as f64 * 30.0 + 20.0, i as f64, false)).collect();
        let beats: Vec<usize> = (0..120).collect();
        assert_eq!(build_pool(many.clone(), &beats, true, &scan).len(), 120);
        assert_eq!(build_pool(many, &beats, false, &scan).len(), 120);
    }

    #[test]
    fn scan_pool_skips_the_same_moment_within_a_beat() {
        let beat_of = vec![0, 0, 1];
        let windows = vec![
            win(0, 0.0, 30.0, 90.0, false),
            win(1, 2.0, 32.0, 80.0, false), // the same moment as the first
            win(2, 40.0, 60.0, 50.0, false),
        ];
        let pool = round_robin_pool(windows, &beat_of, 10);
        assert_eq!(pool.iter().map(|w| w.first).collect::<Vec<_>>(), vec![0, 2]);
    }

    #[test]
    fn min_score_drops_only_judged_windows() {
        let mut ranked = vec![
            win(0, 0.0, 20.0, 80.0, true),
            win(1, 30.0, 50.0, 30.0, true),
            win(2, 60.0, 80.0, 10.0, false),
        ];
        keep_above(&mut ranked, Some(40.0));
        assert_eq!(ranked.iter().map(|w| w.first).collect::<Vec<_>>(), vec![0, 2]);
        keep_above(&mut ranked, None);
        assert_eq!(ranked.len(), 2);
    }

    /// `n` sentences of `secs` seconds each, back to back.
    fn timed(n: usize, secs: f64) -> Vec<Sentence> {
        sents_of(
            &(0..n)
                .map(|i| seg(i as f64 * secs, (i + 1) as f64 * secs, &format!("Sentence number {i} is here.")))
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn regions_join_short_beats_and_split_long_ones_with_overlap() {
        // 100 sentences of 6s = 600s in two beats: 0-1 (12s) and 2-99.
        let sents = timed(100, 6.0);
        let main = Timeline::new(600.0);
        let beat_of: Vec<usize> = (0..100).map(|i| usize::from(i >= 2)).collect();
        let o = opts(15.0, 60.0);
        let regions = pick_regions(&sents, &beat_of, &main, &o);
        // The 12s beat joined the next; the 600s that makes is split into
        // overlapping parts of at most REGION_MAX_S.
        assert_eq!(regions[0].first, 0);
        assert!(regions.len() >= 3, "{regions:?}");
        for r in &regions {
            assert!(main.included_within(sents[r.first].start, sents[r.last].end) <= REGION_MAX_S + 1e-6, "{r:?}");
        }
        // Every 60s stretch (10 sentences) fits whole in some region.
        for a in 0..=90 {
            assert!(regions.iter().any(|r| r.first <= a && a + 9 <= r.last), "{a}-{} in no region: {regions:?}", a + 9);
        }
        // A beat run of exactly one short's length stays whole.
        let short = timed(10, 6.0);
        assert_eq!(pick_regions(&short, &[0; 10], &Timeline::new(60.0), &o), vec![Region { first: 0, last: 9 }]);
    }

    #[test]
    fn a_plain_run_asks_about_the_best_topics_only() {
        let regions: Vec<Region> = (0..10).map(|i| Region { first: i * 10, last: i * 10 + 9 }).collect();
        let windows: Vec<Window> = (0..10).map(|i| win(i * 10, 0.0, 20.0, i as f64, false)).collect();
        let plain = FindOptions { count: 4, ..Default::default() };
        let chosen = choose_regions(regions.clone(), &windows, &plain);
        assert_eq!(chosen.iter().map(|r| r.first).collect::<Vec<_>>(), vec![60, 70, 80, 90], "best four, in order");
        let scan = FindOptions { scan: true, ..Default::default() };
        assert_eq!(choose_regions(regions.clone(), &windows, &scan).len(), 10);
        // A region with no usable window (all saved, off theme) isn't asked about.
        assert_eq!(choose_regions(regions, &windows[..3], &scan).len(), 3);
    }

    #[test]
    fn the_prompt_numbers_kept_lines_and_marks_cuts() {
        let speech = speech();
        let sents = sents_of(&speech);
        let mut main = Timeline::new(60.0);
        main.set_range_included(24.0, 30.0, false, ClipOrigin::Manual); // "It sounds simple."
        let (lines, text) = pick_prompt(&sents, &main, &Region { first: 3, last: 6 });
        assert_eq!(lines, vec![3, 5, 6]);
        assert!(text.contains("Just before this topic"), "{text}");
        assert!(text.contains("And they keep doing it for years."), "context skips the filler: {text}");
        assert!(text.contains("1| [0:00] The fix is"), "{text}");
        assert!(text.contains("— cut in the edit —\n2| [0:06] But it saves hours"), "{text}");
        assert!(text.contains("[0:18] end of topic"), "playable time, without the cut: {text}");
    }

    #[test]
    fn picks_are_checked_and_scored_by_the_rubric() {
        let sents = sents_of(&speech());
        let main = Timeline::new(60.0);
        let lines: Vec<usize> = (0..sents.len()).collect();
        let o = opts(10.0, 20.0);
        assert_eq!(check_pick(&sents, &main, &lines, 1, 2, &o).map(|p| (p.0, p.1)), Ok((0, 1)));
        // Line 3 is the filler "um": an edge on it moves inward.
        assert_eq!(check_pick(&sents, &main, &lines, 3, 5, &o).map(|p| (p.0, p.1)), Ok((3, 4)));
        assert_eq!(check_pick(&sents, &main, &lines, 0, 2, &o), Err(Reject::Lines), "line numbers start at 1");
        assert_eq!(check_pick(&sents, &main, &lines, 2, 1, &o), Err(Reject::Lines));
        assert_eq!(check_pick(&sents, &main, &lines, 1, 11, &o), Err(Reject::Lines), "no such line");
        assert_eq!(check_pick(&sents, &main, &lines, 1, 5, &o), Err(Reject::TooLong(30.0)), "30s is over the 20s limit");
        assert!(check_pick(&sents, &main, &lines, 1, 1, &opts(6.5, 20.0)).is_ok(), "a second short is fine");
        assert_eq!(check_pick(&sents, &main, &lines, 1, 1, &o), Err(Reject::TooShort(6.0)), "6s is under 10s");
        assert_eq!(rubric_score([1.0, 1.0, 1.0, 1.0]), 0.0);
        assert_eq!(rubric_score([5.0, 5.0, 5.0, 5.0]), 100.0);
        assert_eq!(rubric_score([9.0, 0.0, 3.0, 3.0]), 50.0, "ratings are clamped to 1-5");
    }

    /// A model that picks lines 1-2 (good) and 7-8 (poor) in every topic, plus
    /// one pick it can't have meant.
    struct Picker;
    #[async_trait::async_trait]
    impl InferenceClient for Picker {
        async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse> {
            let replies = serde_json::json!([
                {"first": 1, "last": 2, "hook": 5, "standalone": 5, "payoff": 4, "ending": 5, "title": "The big mistake", "reason": "ok"},
                {"first": 8, "last": 9, "hook": 2, "standalone": 2, "payoff": 2, "ending": 2, "title": "Why", "reason": "meh"},
                {"first": 40, "last": 41, "hook": 5, "standalone": 5, "payoff": 5, "ending": 5, "title": "x", "reason": "x"}
            ]);
            Ok(ChatResponse::new(ChatMessage::assistant(&replies.to_string())))
        }
        async fn healthy(&self) -> bool {
            true
        }
    }

    /// A model that picks nothing it could mean.
    struct Nonsense;
    #[async_trait::async_trait]
    impl InferenceClient for Nonsense {
        async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse> {
            let replies = serde_json::json!([{"first": 99, "last": 100, "hook": 5, "standalone": 5, "payoff": 5, "ending": 5, "title": "x", "reason": "x"}]);
            Ok(ChatResponse::new(ChatMessage::assistant(&replies.to_string())))
        }
        async fn healthy(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn the_model_picks_the_edges_and_the_rubric_ranks_them() {
        let sents = sents_of(&speech());
        let main = Timeline::new(60.0);
        let beat_of = vec![0; sents.len()];
        let o = opts(10.0, 20.0);
        let ctx = PickContext { sents: &sents, main: &main, beats: &[], beat_of: &beat_of, relevance: None, opts: &o };
        let region = Region { first: 0, last: sents.len() - 1 };
        let picks = pick_shorts(&Picker, "m", &ctx, &region, None, &mut Tally::default()).await.unwrap().usable().expect("usable");
        assert_eq!(picks.len(), 2, "the out-of-range pick is dropped");
        let best = diversify(picks, 1).remove(0);
        assert_eq!((best.first, best.last), (0, 1));
        assert_eq!(best.title.as_deref(), Some("The big mistake"));
        assert!(best.judged && best.score > 70.0, "{}", best.score);
        // Nothing usable: the caller keeps the heuristic windows instead.
        assert!(pick_shorts(&Nonsense, "m", &ctx, &region, None, &mut Tally::default()).await.unwrap().usable().is_none());
    }

    /// A model whose first answer runs too long and whose second is fine. It
    /// keeps what it was asked.
    #[derive(Default)]
    struct SecondTry {
        asked: std::sync::Mutex<Vec<String>>,
    }
    #[async_trait::async_trait]
    impl InferenceClient for SecondTry {
        async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
            let user = request.messages.iter().rev().find(|m| m.role == "user").and_then(|m| m.content.clone());
            let mut asked = self.asked.lock().unwrap();
            asked.push(user.unwrap_or_default());
            let (last, title) = if asked.len() == 1 { (5, "Too long") } else { (2, "Just right") };
            let replies = serde_json::json!([
                {"first": 1, "last": last, "hook": 5, "standalone": 5, "payoff": 5, "ending": 5, "title": title, "reason": "x"}
            ]);
            Ok(ChatResponse::new(ChatMessage::assistant(&replies.to_string())))
        }
        async fn healthy(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn an_unusable_answer_is_asked_again_with_what_was_wrong() {
        let editor = Editor::test_instance();
        let sents = sents_of(&speech());
        let main = Timeline::new(60.0);
        let beat_of = vec![0; sents.len()];
        let o = FindOptions { scan: true, ..opts(10.0, 20.0) };
        let ctx = PickContext { sents: &sents, main: &main, beats: &[], beat_of: &beat_of, relevance: None, opts: &o };
        let windows = candidate_windows(&sents, Some(&beat_of), &main, &o);
        let model = SecondTry::default();
        let mut tally = Tally::default();
        let picks = pick_all(&editor, Uuid::new_v4(), &model, &ctx, &windows, &mut tally).await.unwrap().unwrap();
        assert_eq!((tally.topics, tally.retried, tally.failed, tally.too_long), (1, 1, 0, 1));
        assert_eq!(picks.iter().filter(|w| w.judged).filter_map(|w| w.title.as_deref()).collect::<Vec<_>>(), vec!["Just right"]);
        let asked = model.asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 2);
        assert!(!asked[0].contains("previous answer"));
        assert!(asked[1].contains("lines 1-5: runs 30 s, too long") && asked[1].contains("10-20 seconds"), "{}", asked[1]);
        // Unusable twice: the topic keeps its rule-scored windows (and with no
        // judged pick anywhere, the caller falls back to them all).
        let mut tally = Tally::default();
        assert!(pick_all(&editor, Uuid::new_v4(), &Nonsense, &ctx, &windows, &mut tally).await.unwrap().is_none());
        assert_eq!((tally.retried, tally.failed, tally.bad_lines), (1, 1, 2));
    }

    #[test]
    fn a_sentence_never_joins_a_cut_false_start_to_the_kept_take() {
        let speech = vec![seg(0.0, 2.0, "So the pricing model"), seg(2.3, 5.0, "So the pricing model is per seat.")];
        let mut main = Timeline::new(5.0);
        main.set_range_included(0.0, 2.15, false, ClipOrigin::Manual);
        let s = sentences(&speech, &main);
        assert_eq!(s.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), vec!["So the pricing model", "So the pricing model is per seat."]);
        // Uncut, they're one run-on sentence.
        assert_eq!(sents_of(&speech).len(), 1);
    }

    #[test]
    fn product_names_and_trailing_ellipses_dont_split_wrongly() {
        let texts = |t: &str| sents_of(&[seg(0.0, 10.0, t)]).into_iter().map(|s| s.text).collect::<Vec<_>>();
        assert_eq!(texts("We use Next.js. It is fast."), vec!["We use Next.js.", "It is fast."]);
        assert_eq!(texts("Version 3.5. Then 4."), vec!["Version 3.5.", "Then 4."]);
        assert_eq!(texts("And then… it broke. Fine."), vec!["And then… it broke.", "Fine."]);
        assert_eq!(texts("The U.S. Army came. Okay then."), vec!["The U.S. Army came.", "Okay then."]);
    }

    #[test]
    fn line_numbers_and_ratings_are_read_leniently() {
        use serde_json::json;
        assert_eq!(line_number(&json!(3)), Some(3));
        assert_eq!(line_number(&json!(3.0)), Some(3));
        assert_eq!(line_number(&json!("3")), Some(3));
        assert_eq!(line_number(&json!(-1)), None);
        assert_eq!(line_number(&json!(2.5)), None);
        assert_eq!(line_number(&json!(null)), None);
        assert_eq!(rating(Some(&json!("5"))), 5.0);
        assert_eq!(rating(Some(&json!(4))), 4.0);
        assert_eq!(rating(None), 3.0);
    }

    /// A model whose reply mixes a malformed pick, string numbers and more
    /// picks than asked for.
    struct Messy;
    #[async_trait::async_trait]
    impl InferenceClient for Messy {
        async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse> {
            let replies = serde_json::json!([
                {"first": -1, "last": 2, "hook": 5, "standalone": 5, "payoff": 5, "ending": 5, "title": "bad", "reason": "x"},
                {"first": "1", "last": 2.0, "hook": "5", "standalone": 5, "payoff": 5, "ending": 5, "title": "Strings", "reason": "x"},
                {"first": 4, "last": 5, "hook": 1, "standalone": 1, "payoff": 1, "ending": 1, "title": "Weak", "reason": "x"},
                {"first": 6, "last": 7, "hook": 3, "standalone": 3, "payoff": 3, "ending": 3, "title": "Mid", "reason": "x"},
                {"first": 8, "last": 9, "hook": 4, "standalone": 4, "payoff": 4, "ending": 4, "title": "Good", "reason": "x"},
                "not even an object"
            ]);
            Ok(ChatResponse::new(ChatMessage::assistant(&replies.to_string())))
        }
        async fn healthy(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn one_bad_pick_doesnt_cost_the_rest_and_a_topic_gives_at_most_three() {
        let sents = sents_of(&speech());
        let main = Timeline::new(60.0);
        let beat_of = vec![0; sents.len()];
        let o = opts(10.0, 20.0);
        let ctx = PickContext { sents: &sents, main: &main, beats: &[], beat_of: &beat_of, relevance: None, opts: &o };
        let picks = pick_shorts(&Messy, "m", &ctx, &Region { first: 0, last: sents.len() - 1 }, None, &mut Tally::default()).await.unwrap().usable().unwrap();
        let titles: Vec<&str> = picks.iter().filter_map(|w| w.title.as_deref()).collect();
        assert_eq!(titles, vec!["Strings", "Good", "Mid"], "the best three, best first");
    }

    #[tokio::test]
    async fn with_a_theme_a_pick_the_search_never_hit_is_dropped() {
        let sents = sents_of(&speech());
        let main = Timeline::new(60.0);
        let beat_of = vec![0; sents.len()];
        let o = opts(10.0, 20.0);
        // Only sentence 8 is about the theme: Picker's 1-2 pick is off theme.
        let mut rel = vec![0.0; sents.len()];
        rel[8] = 1.0;
        let ctx = PickContext { sents: &sents, main: &main, beats: &[], beat_of: &beat_of, relevance: Some(&rel), opts: &o };
        let picks = pick_shorts(&Picker, "m", &ctx, &Region { first: 0, last: sents.len() - 1 }, None, &mut Tally::default()).await.unwrap().usable().unwrap();
        assert_eq!(picks.iter().map(|w| (w.first, w.last)).collect::<Vec<_>>(), vec![(7, 8)]);
        assert!(picks[0].signals.contains(&"on theme"));
    }

    #[test]
    fn a_plain_run_doesnt_ask_twice_about_the_same_moment() {
        // Two overlapping parts of one long beat share its best window.
        let regions = vec![
            Region { first: 0, last: 49 },
            Region { first: 40, last: 89 },
            Region { first: 90, last: 99 },
            Region { first: 100, last: 109 },
            Region { first: 110, last: 119 },
            Region { first: 120, last: 129 },
        ];
        let mut windows: Vec<Window> = (0..6).map(|i| win(90 + i * 10, 0.0, 20.0, 10.0 + i as f64, false)).collect();
        windows.push(win(45, 0.0, 20.0, 99.0, false));
        let plain = FindOptions { count: 4, ..Default::default() };
        let chosen = choose_regions(regions, &windows, &plain);
        assert_eq!(chosen.len(), 4);
        assert_eq!(chosen.iter().filter(|r| r.contains(&windows[6])).count(), 1, "{chosen:?}");
    }

    #[test]
    fn long_shorts_still_move_each_region_on() {
        let o = FindOptions { min_s: 15.0, max_s: 180.0, ..Default::default() };
        assert_eq!(region_bounds(&o), (360.0, 180.0));
        let o3 = FindOptions { min_s: 15.0, max_s: 300.0, ..Default::default() };
        assert_eq!(region_bounds(&o3), (600.0, 300.0));
        // 1200s in one beat with 3-minute shorts: parts of at most 360s,
        // each starting at least 180s after the one before.
        let sents = timed(200, 6.0);
        let main = Timeline::new(1200.0);
        let regions = pick_regions(&sents, &[0; 200], &main, &o);
        for pair in regions.windows(2) {
            assert!(sents[pair[1].first].start - sents[pair[0].first].start >= 180.0 - 1e-6, "{regions:?}");
        }
        for a in 0..=170 {
            assert!(regions.iter().any(|r| r.first <= a && a + 29 <= r.last), "a 180s short at {a} fits no region");
        }
    }
}
