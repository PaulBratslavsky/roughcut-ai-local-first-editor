//! Per-segment embedding index + cosine search. Best-effort by design: no
//! embedding server means no index, and keyword (BM25) search keeps working.

use super::Editor;
use crate::error::Result;
use uuid::Uuid;

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
        if !self.inner.indexing.lock().unwrap().insert(project_id) {
            return; // already building
        }
        let editor = self.clone();
        handle.spawn(async move {
            let _ = editor.index_transcript(project_id).await;
            editor.inner.indexing.lock().unwrap().remove(&project_id);
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
