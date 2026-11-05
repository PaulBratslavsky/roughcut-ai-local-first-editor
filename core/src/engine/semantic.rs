//! Per-segment embedding index + cosine search. Best-effort by design: no
//! embedding server means no index, and keyword (BM25) search keeps working.
//! Also home to the duration planner, which ranks segments by semantic
//! centrality when an index exists.

use super::Editor;
use crate::error::{CoreError, Result};
use std::collections::HashMap;
use uuid::Uuid;

/// What `plan_duration_cut` proposes: cut these segments and the included
/// duration drops from `before_s` to about `projected_after_s`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DurationCutPlan {
    pub segment_ids: Vec<Uuid>,
    pub before_s: f64,
    pub projected_after_s: f64,
    pub target_s: f64,
    /// "centrality" (embedding-ranked) or "heuristic" (no index).
    pub method: &'static str,
    pub notes: String,
}

impl Editor {
    /// Backfill: projects transcribed before embedding indexing existed (or
    /// while the embedding server was down) have a transcript but no index.
    /// Called on project open; builds the index in the background when one
    /// is missing, so old projects quietly gain hybrid search. Best-effort
    /// and deduplicated; a no-op outside a tokio runtime (sync tests).
    pub fn backfill_index_if_missing(&self, project_id: Uuid) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else { return };
        let has_transcript =
            matches!(self.get_transcript(project_id), Ok(Some(t)) if !t.segments.is_empty());
        let has_index =
            matches!(self.inner.store.load_embeddings(project_id), Ok(Some(_)));
        if !has_transcript || has_index {
            return;
        }
        fn indexing_set() -> &'static std::sync::Mutex<std::collections::HashSet<Uuid>> {
            static S: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<Uuid>>> =
                std::sync::OnceLock::new();
            S.get_or_init(Default::default)
        }
        let Some(claim) = crate::inflight::try_claim(indexing_set(), project_id) else {
            return; // already building
        };
        let editor = self.clone();
        handle.spawn(async move {
            let _claim = claim; // released on completion OR panic
            let _ = editor.index_transcript(project_id).await;
        });
    }

    /// Embed every speech segment and persist the vectors. Best-effort: no
    /// embedding server → Ok(0), keyword search keeps working.
    pub async fn index_transcript(&self, project_id: Uuid) -> Result<u32> {
        let transcript = match self.get_transcript(project_id)? {
            Some(t) => t,
            None => return Ok(0),
        };
        let embedder = self.embedder()?;
        let items: Vec<(Uuid, String)> = transcript
            .segments
            .iter()
            .filter(|seg| !seg.is_silence && !seg.is_filler && !seg.text.is_empty())
            .map(|seg| (seg.id, seg.text.clone()))
            .collect();
        if items.is_empty() {
            return Ok(0);
        }
        let mut vectors: Vec<(Uuid, Vec<f32>)> = Vec::with_capacity(items.len());
        for chunk in items.chunks(64) {
            let texts: Vec<String> = chunk.iter().map(|(_, t)| t.clone()).collect();
            let embedded = match embedder.embed(&texts).await {
                Ok(v) => v,
                // No embedding server / model missing: silently skip.
                Err(_) => return Ok(0),
            };
            for ((id, _), v) in chunk.iter().zip(embedded) {
                vectors.push((*id, v));
            }
        }
        let n = vectors.len() as u32;
        self.inner.store.save_embeddings(project_id, &embedder.model_id(), &vectors)?;
        Ok(n)
    }

    /// Plan cuts that bring the included duration down to `target_s`: rank
    /// every still-included speech segment by how central it is to the video
    /// (cosine to the mean embedding — tangents rank low), protect the intro
    /// and outro, and greedily select the least-central material until the
    /// target is met. Returns a PLAN; the caller applies it with one batched
    /// cut. Without an index the ranking falls back to alt-takes and
    /// low-information segments (few words per second).
    pub fn plan_duration_cut(&self, project_id: Uuid, target_s: f64) -> Result<DurationCutPlan> {
        if target_s <= 0.0 {
            return Err(CoreError::InvalidArg("target_duration_s must be positive".into()));
        }
        let vectors: Option<HashMap<Uuid, Vec<f32>>> = self
            .inner
            .store
            .load_embeddings(project_id)?
            .map(|(_, v)| v.into_iter().collect());

        self.with_project(project_id, |p, t| {
            let t = t.ok_or_else(|| CoreError::InvalidArg("no transcript; transcribe first".into()))?;
            let before_s = p.timeline.included_duration();
            if before_s <= target_s {
                return Ok(DurationCutPlan {
                    segment_ids: vec![],
                    before_s,
                    projected_after_s: before_s,
                    target_s,
                    method: "centrality",
                    notes: "already at or under the target".into(),
                });
            }
            // How much of a segment is still on the timeline.
            let included_overlap = |start: f64, end: f64| -> f64 {
                p.timeline
                    .included_clips()
                    .map(|c| (end.min(c.source_out) - start.max(c.source_in)).max(0.0))
                    .sum()
            };

            // Candidates: speech segments with included footage left.
            let speech: Vec<_> = t
                .segments
                .iter()
                .filter(|s| !s.is_silence && !s.text.is_empty())
                .collect();
            let mean: Option<Vec<f32>> = vectors.as_ref().map(|m| {
                let dim = m.values().next().map(|v| v.len()).unwrap_or(0);
                let mut mean = vec![0f32; dim];
                for v in m.values() {
                    for (a, b) in mean.iter_mut().zip(v) {
                        *a += b;
                    }
                }
                for a in &mut mean {
                    *a /= m.len().max(1) as f32;
                }
                mean
            });

            // Score every speech segment (low = cut first), then cut whole
            // SENTENCE GROUPS, never lone segments: whisper segments start and
            // end mid-sentence, so ranking them one by one cut every other
            // segment and left a cut point mid-sentence every few seconds.
            let mut pieces: Vec<Piece> = Vec::with_capacity(speech.len());
            for (i, s) in speech.iter().enumerate() {
                let score = match (&vectors, &mean) {
                    (Some(m), Some(mean)) => match m.get(&s.id) {
                        Some(v) => crate::adapters::embed::cosine(v, mean) as f64,
                        None => 0.5, // unindexed segment: middle of the pack
                    },
                    // No index: alt-takes go first, then rambly low-info
                    // segments (few distinct words per second rank low).
                    _ => {
                        if s.take_group_id.is_some() && !s.is_best_take {
                            -1.0
                        } else {
                            let words: std::collections::HashSet<&str> =
                                s.text.split_whitespace().collect();
                            words.len() as f64 / (s.end - s.start).max(0.5)
                        }
                    }
                };
                let next_start = speech.get(i + 1).map(|n| first_word_start(n));
                pieces.push(Piece {
                    id: s.id,
                    start: s.start,
                    end: s.end,
                    score,
                    included: included_overlap(s.start, s.end),
                    // Protect the opening and closing of the video.
                    protected: i < 2 || i + 2 >= speech.len(),
                    ends_sentence: ends_sentence(&s.text)
                        || next_start.map(|n| n - last_word_end(s) >= PAUSE_BREAK_S).unwrap_or(true),
                    gap_after: next_start.map(|n| n - last_word_end(s)).unwrap_or(f64::MAX),
                });
            }
            let (plan, projected) = plan_by_sentences(&pieces, before_s, target_s);
            let method = if vectors.is_some() { "centrality" } else { "heuristic" };
            let notes = if projected > target_s {
                format!(
                    "even cutting every candidate leaves {projected:.0}s — the intro/outro \
                     protection or already-cut footage caps how much can go"
                )
            } else {
                format!("least-central whole sentences first ({method} ranking)")
            };
            Ok(DurationCutPlan {
                segment_ids: plan,
                before_s,
                projected_after_s: projected.max(0.0),
                target_s,
                method,
                notes,
            })
        })
    }

    /// Hybrid retrieval over the transcript: BM25 (lexical) fused with
    /// embedding cosine (semantic) by reciprocal-rank fusion; BM25 alone when
    /// no index exists. Best first `(segment id, fused score)`, plus which
    /// method ran ("hybrid" | "bm25").
    pub async fn hybrid_find(
        &self,
        project_id: Uuid,
        query: &str,
        k: usize,
    ) -> Result<(Vec<(Uuid, f64)>, &'static str)> {
        let semantic = self.semantic_find(project_id, query, k * 2).await?;
        self.with_project(project_id, |_, t| {
            let t = t.ok_or_else(|| CoreError::InvalidArg("no transcript".into()))?;
            let bm25 = crate::detect::bm25_rank(t, query, k * 2);
            let mut rrf: HashMap<Uuid, f64> = HashMap::new();
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
            fused.truncate(k);
            Ok((fused, method))
        })
    }

    /// Cosine top-k over the project's index. None = no usable index (caller
    /// falls back to keyword search).
    pub async fn semantic_find(
        &self,
        project_id: Uuid,
        query: &str,
        k: usize,
    ) -> Result<Option<Vec<(Uuid, f32)>>> {
        let Some((indexed_model, vectors)) = self.inner.store.load_embeddings(project_id)? else {
            return Ok(None);
        };
        let embedder = self.embedder()?;
        if embedder.model_id() != indexed_model {
            return Ok(None); // stale index from a different model
        }
        let q = match embedder.embed(&[query.to_string()]).await {
            Ok(mut v) if !v.is_empty() => v.remove(0),
            _ => return Ok(None),
        };
        let mut scored: Vec<(Uuid, f32)> = vectors
            .iter()
            .map(|(id, v)| (*id, crate::adapters::embed::cosine(&q, v)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(k);
        Ok(Some(scored))
    }
}

/// A pause this long between two segments counts as a sentence break even
/// without punctuation (long unpunctuated stretches are common in live
/// streams).
const PAUSE_BREAK_S: f64 = 0.6;
/// A sentence group is closed at this length even with no break, so one
/// run-on stretch can't become an all-or-nothing block.
const MAX_GROUP_S: f64 = 40.0;
/// A kept stretch shorter than this, with cuts on both sides, is cut too
/// when there's room under the target: a two-second island reads as a glitch.
const ISLAND_S: f64 = 4.0;
/// How much touching an existing cut lowers a group's score, as a share of
/// the score spread. Small: enough to gather cuts into runs, not enough to
/// override what the content scores say (tuned on a 62-minute stream).
const RUN_BONUS: f64 = 0.05;

/// One transcript segment as the duration planner sees it.
#[derive(Debug, Clone)]
pub(crate) struct Piece {
    pub id: Uuid,
    pub start: f64,
    pub end: f64,
    /// Lower = less central = cut first.
    pub score: f64,
    /// Seconds of it still on the timeline.
    pub included: f64,
    pub protected: bool,
    /// The segment ends a sentence (punctuation or a pause after it).
    pub ends_sentence: bool,
    /// Silence between this segment's last word and the next one's first.
    pub gap_after: f64,
}

fn ends_sentence(text: &str) -> bool {
    matches!(text.trim_end().chars().last(), Some('.' | '!' | '?'))
}

fn first_word_start(s: &crate::model::TranscriptSegment) -> f64 {
    s.words.first().map(|w| w.start).unwrap_or(s.start)
}

fn last_word_end(s: &crate::model::TranscriptSegment) -> f64 {
    s.words.last().map(|w| w.end).unwrap_or(s.end)
}

/// Group pieces into sentences, then cut the least-central groups until the
/// projected duration reaches `target_s`. Groups next to an existing cut get
/// a small bonus so cuts gather into runs instead of scattering. Returns the
/// segment ids to cut and the projected duration.
pub(crate) fn plan_by_sentences(pieces: &[Piece], before_s: f64, target_s: f64) -> (Vec<Uuid>, f64) {
    // 1. Sentence groups (index ranges into `pieces`).
    let mut groups: Vec<std::ops::Range<usize>> = vec![];
    let mut open = 0;
    for (i, p) in pieces.iter().enumerate() {
        if p.ends_sentence || i + 1 == pieces.len() {
            groups.push(open..i + 1);
            open = i + 1;
        } else if p.end - pieces[open].start >= MAX_GROUP_S {
            // A run-on stretch: close it at its longest pause, not here.
            let at = (open..=i)
                .max_by(|&a, &b| pieces[a].gap_after.partial_cmp(&pieces[b].gap_after).unwrap_or(std::cmp::Ordering::Equal))
                .unwrap_or(i);
            groups.push(open..at + 1);
            open = at + 1;
        }
    }
    struct G {
        range: std::ops::Range<usize>,
        score: f64,
        saved: f64,
        cuttable: bool,
    }
    let mut gs: Vec<G> = groups
        .into_iter()
        .map(|r| {
            let ps = &pieces[r.clone()];
            let dur: f64 = ps.iter().map(|p| (p.end - p.start).max(0.01)).sum();
            let score = ps.iter().map(|p| p.score * (p.end - p.start).max(0.01)).sum::<f64>() / dur;
            let saved: f64 = ps.iter().map(|p| p.included).sum();
            let cuttable = !ps.iter().any(|p| p.protected) && saved >= 0.2;
            G { range: r, score, saved, cuttable }
        })
        .collect();
    // Smooth with the neighbours: a sentence inside a weak passage goes
    // before an equally weak sentence inside a strong one.
    let raw: Vec<f64> = gs.iter().map(|g| g.score).collect();
    for (i, g) in gs.iter_mut().enumerate() {
        let prev = if i > 0 { raw[i - 1] } else { raw[i] };
        let next = raw.get(i + 1).copied().unwrap_or(raw[i]);
        g.score = 0.5 * raw[i] + 0.25 * (prev + next);
    }
    // A group counts as "already gone" when nothing of it is on the timeline.
    let mut cut: Vec<bool> = gs.iter().map(|g| g.saved < 0.2).collect();
    let mut chosen: Vec<bool> = vec![false; gs.len()];
    let spread = {
        let (lo, hi) = raw.iter().fold((f64::MAX, f64::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
        if hi > lo { hi - lo } else { 1.0 }
    };
    let mut projected = before_s;

    // 2. Greedy: cheapest group first, with a bonus for touching a cut.
    while projected > target_s {
        let best = (0..gs.len())
            .filter(|&i| gs[i].cuttable && !chosen[i])
            .map(|i| {
                let touching = (i > 0 && cut[i - 1]) as u8 + (i + 1 < gs.len() && cut[i + 1]) as u8;
                (i, gs[i].score - RUN_BONUS * spread * touching as f64)
            })
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let Some((i, _)) = best else { break };
        chosen[i] = true;
        cut[i] = true;
        projected -= gs[i].saved;
    }

    // 3. Islands: a short kept stretch between two cuts goes too, while the
    // result stays within 3% of the target.
    for i in 1..gs.len().saturating_sub(1) {
        if !chosen[i] && gs[i].cuttable && cut[i - 1] && cut[i + 1] && gs[i].saved < ISLAND_S
            && projected - gs[i].saved >= target_s * 0.97
        {
            chosen[i] = true;
            cut[i] = true;
            projected -= gs[i].saved;
        }
    }

    // 4. Overshoot: the last pick can be a long group. Put back the most
    // central cut groups that still fit above the target.
    let mut back: Vec<usize> = (0..gs.len()).filter(|&i| chosen[i]).collect();
    back.sort_by(|&a, &b| gs[b].score.partial_cmp(&gs[a].score).unwrap_or(std::cmp::Ordering::Equal));
    for i in back {
        if projected + gs[i].saved <= target_s {
            chosen[i] = false;
            projected += gs[i].saved;
        }
    }

    let ids = gs
        .iter()
        .zip(&chosen)
        .filter(|(_, &c)| c)
        .flat_map(|(g, _)| pieces[g.range.clone()].iter().filter(|p| p.included > 0.0).map(|p| p.id))
        .collect();
    (ids, projected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piece(i: usize, text: &str, score: f64) -> (Piece, String) {
        let p = Piece {
            id: Uuid::new_v4(),
            start: i as f64 * 5.0,
            end: i as f64 * 5.0 + 5.0,
            score,
            included: 5.0,
            protected: false,
            ends_sentence: ends_sentence(text),
            gap_after: 0.1,
        };
        (p, text.to_string())
    }

    /// Sentences split across segments the way whisper splits them, with
    /// alternating scores: the old planner cut every other segment.
    #[test]
    fn cuts_whole_sentences_not_alternate_segments() {
        let texts = [
            "So the first thing", "is the setup.", "And then we", "talk about cats.",
            "Cats are", "very nice animals.", "Dogs are also", "pretty good.",
            "Finally the", "wrap up.",
        ];
        let scores = [0.9, 0.1, 0.8, 0.2, 0.7, 0.3, 0.85, 0.15, 0.9, 0.2];
        let built: Vec<(Piece, String)> =
            texts.iter().zip(scores).enumerate().map(|(i, (t, s))| piece(i, t, s)).collect();
        let pieces: Vec<Piece> = built.iter().map(|(p, _)| p.clone()).collect();
        let (ids, projected) = plan_by_sentences(&pieces, 50.0, 30.0);
        assert!(projected <= 30.0);
        // Every cut sentence is cut whole: both halves or neither.
        for pair in pieces.chunks(2) {
            let a = ids.contains(&pair[0].id);
            let b = ids.contains(&pair[1].id);
            assert_eq!(a, b, "a sentence was split by the cut");
        }
    }

    #[test]
    fn protected_and_already_cut_pieces_stay_out() {
        let mut pieces: Vec<Piece> =
            (0..6).map(|i| piece(i, "One sentence.", 0.1 * i as f64).0).collect();
        pieces[0].protected = true;
        pieces[1].included = 0.0;
        let (ids, _) = plan_by_sentences(&pieces, 25.0, 10.0);
        assert!(!ids.contains(&pieces[0].id));
        assert!(!ids.contains(&pieces[1].id));
    }

    #[test]
    fn run_on_stretches_break_at_the_longest_pause() {
        // 12 unpunctuated 5 s segments; the longest pause is after #4.
        let mut pieces: Vec<Piece> = (0..12).map(|i| piece(i, "and so on", 0.5).0).collect();
        pieces[4].gap_after = 0.5;
        pieces[4].score = 0.0; // make the first group the cheapest
        let (ids, _) = plan_by_sentences(&pieces, 60.0, 35.0);
        assert!(ids.contains(&pieces[4].id));
        assert!(!ids.contains(&pieces[5].id), "the group should end at the pause");
    }

    #[test]
    fn cuts_gather_into_runs() {
        // Flat scores: the adjacency bonus should keep the cuts together.
        let pieces: Vec<Piece> = (0..12).map(|i| piece(i, "A sentence.", 0.5).0).collect();
        let (ids, _) = plan_by_sentences(&pieces, 60.0, 40.0);
        let idx: Vec<usize> =
            pieces.iter().enumerate().filter(|(_, p)| ids.contains(&p.id)).map(|(i, _)| i).collect();
        assert_eq!(idx.len(), 4);
        assert_eq!(idx.last().unwrap() - idx[0], 3, "cuts should be one run: {idx:?}");
    }
}
