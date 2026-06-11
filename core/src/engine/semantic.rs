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

            let mut candidates: Vec<(Uuid, f64, f64)> = vec![]; // (id, score asc = cut first, seconds saved)
            for (i, s) in speech.iter().enumerate() {
                // Protect the opening and closing of the video.
                if i < 2 || i + 2 >= speech.len() {
                    continue;
                }
                let saved = included_overlap(s.start, s.end);
                if saved < 0.2 {
                    continue; // already cut
                }
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
                candidates.push((s.id, score, saved));
            }
            candidates.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

            let mut plan = vec![];
            let mut projected = before_s;
            for (id, _, saved) in &candidates {
                if projected <= target_s {
                    break;
                }
                plan.push(*id);
                projected -= saved;
            }
            let method = if vectors.is_some() { "centrality" } else { "heuristic" };
            let notes = if projected > target_s {
                format!(
                    "even cutting every candidate leaves {projected:.0}s — the intro/outro \
                     protection or already-cut footage caps how much can go"
                )
            } else {
                format!("least-central material first ({method} ranking)")
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
