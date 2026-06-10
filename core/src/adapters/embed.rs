//! Embedder: local text embeddings behind a trait, for semantic transcript
//! search. Production hits the local Ollama/llama-server OpenAI-compatible
//! `/v1/embeddings` endpoint (e.g. nomic-embed-text) — fully on-device, same
//! zero-egress story as everything else. No server → callers fall back to
//! keyword search.

use crate::error::{CoreError, Result};
use async_trait::async_trait;
use serde_json::json;

#[async_trait]
pub trait Embedder: Send + Sync {
    /// One vector per input text. Empty input → empty output.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    /// Identifies the model so stale indexes can be detected.
    fn model_id(&self) -> String;
}

pub struct OllamaEmbedder {
    endpoint: String,
    model: String,
    http: reqwest::Client,
}

impl OllamaEmbedder {
    pub fn new(endpoint: &str, model: &str) -> Self {
        Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            model: model.to_string(),
            http: reqwest::Client::new(),
        }
    }

    pub async fn reachable(&self) -> bool {
        self.http
            .get(format!("{}/models", self.endpoint))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}

#[async_trait]
impl Embedder for OllamaEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let url = format!("{}/embeddings", self.endpoint);
        let resp = self
            .http
            .post(&url)
            .json(&json!({ "model": self.model, "input": texts }))
            .send()
            .await
            .map_err(|e| CoreError::Inference(format!("embeddings unreachable at {url}: {e}")))?;
        let status = resp.status();
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| CoreError::Inference(format!("bad embeddings response: {e}")))?;
        if !status.is_success() {
            return Err(CoreError::Inference(format!("embeddings HTTP {status}: {body}")));
        }
        let data = body["data"]
            .as_array()
            .ok_or_else(|| CoreError::Inference("embeddings: no data array".into()))?;
        data.iter()
            .map(|d| {
                d["embedding"]
                    .as_array()
                    .map(|a| a.iter().map(|v| v.as_f64().unwrap_or(0.0) as f32).collect())
                    .ok_or_else(|| CoreError::Inference("embeddings: bad vector".into()))
            })
            .collect()
    }

    fn model_id(&self) -> String {
        self.model.clone()
    }
}

/// Deterministic bag-of-words hashing embedder for tests/demo: similar texts
/// share dimensions, so cosine ranking behaves sensibly without a model.
pub struct MockEmbedder;

#[async_trait]
impl Embedder for MockEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0.0f32; 128];
                for word in t.to_lowercase().split_whitespace() {
                    let w: String = word.chars().filter(|c| c.is_alphanumeric()).collect();
                    if w.len() < 3 {
                        continue;
                    }
                    let mut h: u32 = 2166136261;
                    for b in w.bytes() {
                        h ^= b as u32;
                        h = h.wrapping_mul(16777619);
                    }
                    v[(h % 128) as usize] += 1.0;
                }
                v
            })
            .collect())
    }

    fn model_id(&self) -> String {
        "mock-embedder".into()
    }
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_embedder_ranks_similar_text_higher() {
        let e = MockEmbedder;
        let vs = e
            .embed(&[
                "hiking on the weekend was great".into(),
                "the editor cuts silence automatically".into(),
                "I went hiking last weekend".into(),
            ])
            .await
            .unwrap();
        let sim_related = cosine(&vs[0], &vs[2]);
        let sim_unrelated = cosine(&vs[0], &vs[1]);
        assert!(sim_related > sim_unrelated);
    }
}
