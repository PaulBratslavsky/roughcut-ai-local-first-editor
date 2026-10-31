//! The clip finder: find the moments of a long-form video that work as
//! standalone short-form clips (Shorts / Reels / TikTok) and rank them.
//!
//! Same design rule as `story.rs`: deterministic scaffolding with the model
//! embedded in ONE step. Candidates are generated without a model
//! (sentence-aligned windows of the current cut, kept inside one story
//! beat), pre-ranked by a cheap heuristic, and only the best few are scored
//! by the model, in small batches, by index (small models mangle uuids). No
//! model reachable → the heuristic ranking stands.

use crate::adapters::InferenceClient;
use crate::engine::Editor;
use crate::error::{CoreError, Result};
use crate::model::{pieces_overlap, ClipCandidate, SourceRange, Timeline, TranscriptSegment};
use crate::story::Beat;
use serde::Deserialize;
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
    /// Scan the whole video: a bigger model pool, filled across every story
    /// beat so no part of the video goes unjudged.
    pub scan: bool,
    /// Drop windows the MODEL scored below this. Windows it didn't judge
    /// (no model, a failed batch) are kept and rank after judged ones.
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

/// A sentence-aligned stretch of speech: segments `first..=last`.
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
    /// The model scored it. In a model run, anything it skipped ranks after
    /// everything it judged — heuristic and model scores aren't one scale.
    judged: bool,
}

/// The candidate pool the model sees — enough to find the good ones without
/// a long chain of slow local-model calls.
const POOL_MAX: usize = 24;
/// The pool in scan mode: 15 model calls. A starting value — lower it if a
/// scan of a long video is too slow on the user's model.
const SCAN_POOL: usize = 90;
/// The most candidates one run returns.
const MAX_COUNT: usize = 60;
/// Candidates per model call.
const BATCH: usize = 6;
/// Two candidates sharing more than this fraction of the SHORTER one are the
/// same moment; keep the better one. (Over the union is too lenient: two 30s
/// clips sharing 13s read as the same short but score only 0.24.)
const MAX_OVERLAP: f64 = 0.3;
/// With a theme, model-judged candidates under this are off-topic rejects.
const MIN_THEMED_SCORE: f64 = 30.0;

/// One run's result: the candidates written, and how they were scored.
pub struct Found {
    pub candidates: Vec<ClipCandidate>,
    /// "llm" (the on-device model judged them) | "heuristic".
    pub method: &'static str,
}

/// Find, rank, and persist clip candidates for the project's current cut.
pub async fn find_clips(editor: &Editor, project_id: Uuid, opts: FindOptions) -> Result<Found> {
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
    let beat_of = beat_index(&speech, &outline.beats);

    let mut windows = candidate_windows(&speech, Some(&beat_of), &main, &opts);
    let mut by_beat = true;
    if windows.is_empty() {
        // Beats shorter than min_s (choppy pause-based sections): let a
        // window span them rather than return nothing.
        windows = candidate_windows(&speech, None, &main, &opts);
        by_beat = false;
    }
    let inference = editor.inference()?;
    let healthy = inference.healthy().await;
    // A scan doesn't offer moments already saved as clips. (A plain run
    // behaves as it always has: the chat may ask for a saved moment again.)
    if opts.scan && !windows.is_empty() {
        windows.retain(|w| saved.iter().all(|r| pieces_overlap(&w.pieces(), r) <= MAX_OVERLAP));
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
        let (score, signals) = heuristic_score(&speech, w, &outline.beats, &beat_of, &opts);
        w.score = score;
        w.signals = signals;
    }
    if let Some(theme) = &opts.theme {
        progress(editor, project_id, 0.2, format!("searching for “{theme}”…"));
        // Generous recall: the model, told the theme, judges what is really
        // about it. Without a model this ranking is the answer.
        let k = (speech.len() / 6).clamp(8, 40);
        let (hits, _) = editor.hybrid_find(project_id, theme, k).await?;
        let relevance = segment_relevance(&speech, &hits);
        windows.retain_mut(|w| {
            let rel = window_relevance(&relevance, w);
            if rel <= 0.0 {
                return false;
            }
            // On theme first, short-form craft second.
            w.score = 0.4 * w.score + 60.0 * rel;
            w.signals.push("on theme");
            true
        });
        if windows.is_empty() {
            return Err(CoreError::InvalidArg(format!(
                "nothing in the current cut matches the theme “{theme}” — try different words"
            )));
        }
    }
    let pool = build_pool(windows, &beat_of, by_beat, &opts, healthy);

    let (ranked, method) = if healthy {
        let model = editor.get_preferences()?.inference_model;
        let on_batch = |done: usize, total: usize| {
            progress(
                editor,
                project_id,
                0.3 + 0.65 * done as f64 / total.max(1) as f64,
                format!("scoring candidates on-device — {done}/{total}"),
            )
        };
        let theme = opts.theme.as_deref();
        match llm_rescore(inference.as_ref(), &model, &speech, pool.clone(), theme, on_batch).await {
            Ok(scored) => (scored, "llm"),
            Err(_) => (pool, "heuristic"),
        }
    } else {
        (pool, "heuristic")
    };
    let mut ranked = ranked;
    if opts.theme.is_some() && method == "llm" {
        // The model was told to score off-theme clips below 20: trust it and
        // return fewer, on-topic results rather than pad the list.
        let keep_best = ranked.iter().any(|w| w.judged && w.score >= MIN_THEMED_SCORE);
        if keep_best {
            ranked.retain(|w| !w.judged || w.score >= MIN_THEMED_SCORE);
        }
    }
    keep_above(&mut ranked, opts.min_score);
    let picked = diversify(ranked, opts.count);
    let candidates: Vec<ClipCandidate> =
        picked.iter().map(|w| to_candidate(&speech, w, method, opts.theme.as_deref())).collect();
    // Written in one step that (for a scan) also drops moments saved while
    // it ran.
    let drop_saved = opts.scan.then_some(MAX_OVERLAP);
    let candidates = editor.replace_clip_candidates(project_id, candidates, drop_saved)?;
    progress(editor, project_id, 1.0, format!("found {} clip candidates", candidates.len()));
    Ok(Found { candidates, method })
}

/// The windows worth scoring. A plain run: the best `POOL_MAX` (or `count`)
/// distinct moments, as always. A scan: taken beat by beat
/// (`round_robin_pool`), or best-first when the windows span beats. The scan
/// cap only bounds model calls, so without a model nothing is cut off and the
/// result is simply the top moments by rule score.
fn build_pool(windows: Vec<Window>, beat_of: &[usize], by_beat: bool, opts: &FindOptions, model: bool) -> Vec<Window> {
    let cap = if model { SCAN_POOL } else { usize::MAX };
    if !opts.scan {
        diversify(windows, POOL_MAX.max(opts.count))
    } else if by_beat {
        round_robin_pool(windows, beat_of, cap)
    } else {
        diversify(windows, cap)
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
                if pool.iter().chain(round.iter()).all(|p| overlap(p, &w) <= MAX_OVERLAP) {
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

/// Is a segment kept in the current cut? (Its midpoint decides, the same
/// rule the SRT export uses.)
fn kept(main: &Timeline, seg: &TranscriptSegment) -> bool {
    let mid = (seg.start + seg.end) / 2.0;
    main.clips.iter().any(|c| c.included && c.source_in <= mid && mid < c.source_out)
}

/// Every sentence-aligned window whose playable length fits the bounds.
/// A window starts and ends on a kept, non-filler segment, and (when `beat_of`
/// is given) never crosses a beat boundary.
fn candidate_windows(
    speech: &[TranscriptSegment],
    beat_of: Option<&[usize]>,
    main: &Timeline,
    opts: &FindOptions,
) -> Vec<Window> {
    let usable: Vec<bool> =
        speech.iter().map(|s| !s.is_filler && !is_throwaway(&s.text) && kept(main, s)).collect();
    let mut out = vec![];
    for first in 0..speech.len() {
        if !usable[first] {
            continue;
        }
        for last in first..speech.len() {
            if let Some(beats) = beat_of {
                if beats[last] != beats[first] {
                    break;
                }
            }
            let (start, end) = (speech[first].start, speech[last].end);
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

/// Hesitations that can lead a whisper segment ("Um, this is…").
const OPENING_FILLERS: &[&str] = &["um", "uh", "er", "erm", "ah", "hmm", "mm"];

/// The line with leading hesitations removed and re-capitalised — what a
/// viewer actually hears once the filler is cut, and what a title should say.
fn strip_leading_fillers(text: &str) -> String {
    let mut rest = text.trim();
    loop {
        let word = first_word(rest);
        if word.is_empty() || !OPENING_FILLERS.contains(&word.as_str()) {
            break;
        }
        let after = rest.split_once(char::is_whitespace).map(|(_, r)| r).unwrap_or("");
        rest = after.trim_start_matches([',', '.', ' ', '…', '-']).trim_start();
    }
    let mut chars = rest.chars();
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

/// 0..100 short-form potential without a model, plus the signals behind it
/// (they become the human-readable reason).
fn heuristic_score(
    speech: &[TranscriptSegment],
    w: &Window,
    beats: &[Beat],
    beat_of: &[usize],
    opts: &FindOptions,
) -> (f64, Vec<&'static str>) {
    let mut score: f64 = 50.0;
    let mut signals = vec![];
    let raw_open = speech[w.first].text.trim();
    if OPENING_FILLERS.contains(&first_word(raw_open).as_str()) {
        score -= 10.0;
        signals.push("opens on a filler");
    }
    let open_owned = strip_leading_fillers(raw_open);
    let open = open_owned.as_str();
    let open_lc = open.to_lowercase();
    let close = speech[w.last].text.trim();

    let opener = first_word(open);
    if DANGLING.contains(&opener.as_str()) {
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
    if open.contains('?') && open.split_whitespace().count() >= 4 {
        score += 12.0;
        signals.push("opens with a question");
    } else if HOOK_OPENERS.iter().any(|h| open_lc.starts_with(h)) {
        score += 10.0;
        signals.push("strong opener");
    }
    if open.chars().any(|c| c.is_ascii_digit()) {
        score += 4.0;
    }
    if close.ends_with(['.', '!', '?']) {
        score += 8.0;
        signals.push("ends on a complete thought");
    } else {
        score -= 8.0;
        signals.push("trails off");
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

    let segs = &speech[w.first..=w.last];
    let fillers = segs.iter().filter(|s| s.is_filler).count();
    score -= 15.0 * fillers as f64 / segs.len() as f64;

    (score.clamp(0.0, 100.0), signals)
}

fn overlap(a: &Window, b: &Window) -> f64 {
    pieces_overlap(&a.pieces(), &b.pieces())
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
        if picked.iter().all(|p| overlap(p, &w) <= MAX_OVERLAP) {
            picked.push(w);
        }
    }
    picked
}

fn window_text(speech: &[TranscriptSegment], w: &Window) -> String {
    speech[w.first..=w.last].iter().map(|s| s.text.trim()).collect::<Vec<_>>().join(" ")
}

/// Score the pool with the model, in batches. The model's 0-100 dominates;
/// the heuristic breaks near-ties and covers anything the model skipped.
async fn llm_rescore(
    inference: &dyn InferenceClient,
    model: &str,
    speech: &[TranscriptSegment],
    mut pool: Vec<Window>,
    theme: Option<&str>,
    on_batch: impl Fn(usize, usize),
) -> Result<Vec<Window>> {
    #[derive(Deserialize)]
    struct Scored {
        index: usize,
        score: f64,
        #[serde(default)]
        title: String,
        #[serde(default)]
        reason: String,
    }
    let schema = serde_json::json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "index": {"type": "integer"},
                "score": {"type": "integer", "minimum": 0, "maximum": 100},
                "title": {"type": "string"},
                "reason": {"type": "string"}
            },
            "required": ["index", "score", "title", "reason"]
        }
    });
    let mut system = String::from(
        "You are a short-form video editor (YouTube Shorts, Reels, TikTok). Each \
        numbered candidate is a contiguous excerpt from a longer talking-head video. Score how \
        well it works as a STANDALONE short: (1) the first sentence is a hook that stops the \
        scroll; (2) it makes sense with zero context — no 'as I said', no unexplained \
        'this'/'it'; (3) it delivers a payoff (an insight, a surprise, a strong opinion, a \
        laugh) before it ends; (4) it ends on a complete thought. Be harsh: most excerpts are \
        mediocre, so reserve 80+ for clips you would actually post. Reply with ONLY a JSON \
        array, one object per candidate: [{\"index\": candidate number, \"score\": 0-100, \
        \"title\": a catchy title under 60 characters, \"reason\": one short sentence on why \
        it works or doesn't}].",
    );
    if let Some(theme) = theme {
        system.push_str(&format!(
            " The user wants shorts ABOUT: {theme}. A candidate that is not substantially \
             about that scores below 20, however well made it is otherwise."
        ));
    }
    let mut scored_any = false;
    let batches = pool.len().div_ceil(BATCH);
    on_batch(0, batches);
    for (batch_no, batch_start) in (0..pool.len()).step_by(BATCH).enumerate() {
        let batch_end = (batch_start + BATCH).min(pool.len());
        let user: String = (batch_start..batch_end)
            .map(|i| {
                let w = &pool[i];
                let mut text = window_text(speech, w);
                if text.len() > 900 {
                    let cut = (0..=900).rev().find(|&b| text.is_char_boundary(b)).unwrap_or(0);
                    text.truncate(cut);
                    text.push('…');
                }
                format!("{i}| ({:.0}s) {text}", w.duration)
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let replies: Vec<Scored> = match crate::llm::ask_json_schema(
            inference,
            model,
            "short_scores",
            &system,
            &user,
            0.2,
            Some(&schema),
        )
        .await
        {
            Ok(r) => r,
            // One bad batch keeps its heuristic scores; the rest still count.
            Err(_) => {
                on_batch(batch_no + 1, batches);
                continue;
            }
        };
        for r in replies {
            if !(batch_start..batch_end).contains(&r.index) {
                continue;
            }
            let w = &mut pool[r.index];
            w.score = 0.8 * r.score.clamp(0.0, 100.0) + 0.2 * w.score;
            if !r.title.trim().is_empty() {
                w.title = Some(r.title.trim().to_string());
            }
            if !r.reason.trim().is_empty() {
                w.reason = Some(r.reason.trim().to_string());
            }
            w.judged = true;
            scored_any = true;
        }
        on_batch(batch_no + 1, batches);
    }
    if !scored_any {
        return Err(CoreError::Inference("model scored no clip candidates".into()));
    }
    Ok(pool)
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
    w: &Window,
    method: &str,
    theme: Option<&str>,
) -> ClipCandidate {
    let open = strip_leading_fillers(&speech[w.first].text);
    let reason = w.reason.clone().unwrap_or_else(|| {
        let mut parts: Vec<String> = w.signals.iter().map(|s| s.to_string()).collect();
        parts.push(format!("{:.0}s", w.duration));
        let mut r = parts.join("; ");
        if let Some(c) = r.get_mut(0..1) {
            c.make_ascii_uppercase();
        }
        r
    });
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
        segment_ids: speech[w.first..=w.last].iter().map(|s| s.id).collect(),
        method: method.to_string(),
        theme: theme.map(str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::inference::{ChatMessage, ChatRequest, ChatResponse};
    use crate::model::ClipOrigin;

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

    fn opts(min_s: f64, max_s: f64) -> FindOptions {
        FindOptions { count: 5, min_s, max_s, ..Default::default() }
    }

    #[test]
    fn windows_are_sentence_aligned_and_skip_fillers_and_cut_material() {
        let speech = speech();
        let mut main = Timeline::new(60.0);
        // Cut "It sounds simple." (24-30s) in the main edit.
        main.set_range_included(24.0, 30.0, false, ClipOrigin::Manual);
        let ws = candidate_windows(&speech, None, &main, &opts(10.0, 20.0));
        assert!(!ws.is_empty());
        for w in &ws {
            assert!(!speech[w.first].is_filler && !speech[w.last].is_filler);
            assert_ne!(w.first, 4, "never starts on a segment the main cut removed");
            assert_ne!(w.last, 4, "never ends on a segment the main cut removed");
            assert!((10.0..=20.0).contains(&w.duration), "duration {}", w.duration);
        }
        // The cut doesn't count toward length: 18-36s spans 18s of source
        // but only 12s plays.
        let w = ws.iter().find(|w| w.first == 3 && w.last == 5).expect("window over the cut");
        assert!((w.duration - 12.0).abs() < 1e-6);
    }

    #[test]
    fn windows_stay_inside_a_beat() {
        let speech = speech();
        let main = Timeline::new(60.0);
        let beat_of: Vec<usize> = (0..speech.len()).map(|i| if i < 5 { 0 } else { 1 }).collect();
        let ws = candidate_windows(&speech, Some(&beat_of), &main, &opts(10.0, 40.0));
        assert!(ws.iter().all(|w| beat_of[w.first] == beat_of[w.last]));
    }

    #[test]
    fn heuristic_rewards_hooks_and_punishes_cold_opens() {
        let speech = speech();
        let main = Timeline::new(60.0);
        let ws = candidate_windows(&speech, None, &main, &opts(10.0, 20.0));
        let score_of = |first: usize, last: usize| {
            let w = ws.iter().find(|w| w.first == first && w.last == last).unwrap();
            heuristic_score(&speech, w, &[], &vec![0; speech.len()], &opts(10.0, 20.0))
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
        let main = Timeline::new(40.0);
        let ws = candidate_windows(&speech, None, &main, &opts(15.0, 25.0));
        let score_of = |first: usize| {
            let w = ws.iter().find(|w| w.first == first && w.last == first + 1).unwrap();
            heuristic_score(&speech, w, &[], &vec![0; speech.len()], &opts(15.0, 25.0))
        };
        let (with_filler, signals) = score_of(0);
        let (clean, _) = score_of(2);
        assert!(signals.contains(&"opens on a filler"));
        assert!(signals.contains(&"strong opener"), "the hook still counts once the filler is cut");
        assert!(clean > with_filler);
        let w = ws.iter().find(|w| w.first == 0 && w.last == 1).unwrap();
        let c = to_candidate(&speech, w, "heuristic", None);
        assert_eq!(c.hook, "Here's the biggest mistake people make.");
        assert!(c.title.starts_with("Here's the biggest"));
    }

    #[test]
    fn theme_relevance_needs_a_hit_and_rewards_staying_on_topic() {
        let speech = speech();
        let main = Timeline::new(60.0);
        let ws = candidate_windows(&speech, None, &main, &opts(10.0, 20.0));
        // The search returned two segments: 3 (best) and 5.
        let hits = vec![(speech[3].id, 0.04), (speech[5].id, 0.02)];
        let rel = segment_relevance(&speech, &hits);
        assert_eq!(rel[3], 1.0);
        assert_eq!(rel[5], 0.5);
        assert_eq!(rel[0], 0.0);
        let window = |first: usize, last: usize| ws.iter().find(|w| w.first == first && w.last == last).unwrap();
        assert_eq!(window_relevance(&rel, window(0, 1)), 0.0, "no hit → off theme");
        let both = window_relevance(&rel, window(3, 5));
        let one = window_relevance(&rel, window(5, 6));
        assert!(both > one, "{both} vs {one}");
    }

    #[test]
    fn diversify_drops_the_same_moment() {
        let w = |start: f64, end: f64, score: f64| Window {
            first: 0,
            last: 0,
            start,
            end,
            duration: end - start,
            score,
            signals: vec![],
            title: None,
            reason: None,
            judged: false,
        };
        let picked = diversify(vec![w(0.0, 30.0, 60.0), w(2.0, 32.0, 70.0), w(40.0, 70.0, 50.0)], 5);
        assert_eq!(picked.len(), 2);
        assert_eq!(picked[0].score, 70.0);
        assert_eq!(picked[1].start, 40.0);
        assert_eq!(diversify(vec![w(0.0, 10.0, 1.0), w(20.0, 30.0, 2.0)], 1).len(), 1);
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
        }
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
    fn the_pool_by_mode() {
        let beat_of = vec![0; 40];
        let windows: Vec<Window> =
            (0..40).map(|i| win(i, i as f64 * 30.0, i as f64 * 30.0 + 20.0, i as f64, false)).collect();
        let ids = |p: Vec<Window>| p.iter().map(|w| w.first).collect::<Vec<_>>();
        let plain = FindOptions { count: 8, ..Default::default() };
        // Not a scan: exactly today's pool.
        assert_eq!(
            ids(build_pool(windows.clone(), &beat_of, true, &plain, true)),
            ids(diversify(windows.clone(), POOL_MAX))
        );
        let scan = FindOptions { scan: true, ..Default::default() };
        // A scan whose windows span beats: best-first up to the cap.
        assert_eq!(build_pool(windows.clone(), &beat_of, false, &scan, true).len(), 40.min(SCAN_POOL));
        // No model: nothing is cut off.
        let many: Vec<Window> =
            (0..120).map(|i| win(i, i as f64 * 30.0, i as f64 * 30.0 + 20.0, i as f64, false)).collect();
        let beats: Vec<usize> = (0..120).collect();
        assert_eq!(build_pool(many.clone(), &beats, true, &scan, true).len(), SCAN_POOL);
        assert_eq!(build_pool(many, &beats, true, &scan, false).len(), 120);
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

    /// A model that loves exactly one candidate (by index) and pans the rest.
    struct Favors(usize);
    #[async_trait::async_trait]
    impl InferenceClient for Favors {
        async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
            let user = request.messages.iter().rev().find(|m| m.role == "user").unwrap();
            let replies: Vec<serde_json::Value> = user
                .content
                .as_deref()
                .unwrap_or("")
                .split("\n\n")
                .filter_map(|block| block.split('|').next()?.trim().parse::<usize>().ok())
                .map(|i| serde_json::json!({
                    "index": i, "score": if i == self.0 { 95 } else { 5 },
                    "title": format!("Title {i}"), "reason": "ok"
                }))
                .collect();
            Ok(ChatResponse { message: ChatMessage::assistant(&serde_json::to_string(&replies).unwrap()) })
        }
        async fn healthy(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn llm_scores_reorder_the_pool_and_name_the_clips() {
        let speech = speech();
        let main = Timeline::new(60.0);
        let pool = diversify(candidate_windows(&speech, None, &main, &opts(10.0, 20.0)), 8);
        let n = pool.len();
        assert!(n >= 3);
        let scored = llm_rescore(&Favors(n - 1), "m", &speech, pool, None, |_, _| {}).await.unwrap();
        let best = diversify(scored, 1).remove(0);
        assert_eq!(best.title.as_deref(), Some(format!("Title {}", n - 1).as_str()));
        assert_eq!(best.reason.as_deref(), Some("ok"));
    }
}
