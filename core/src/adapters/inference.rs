//! InferenceClient: the local LLM behind an OpenAI-compatible HTTP boundary
//! (llama.cpp `llama-server` or Ollama — and, because it is just an HTTP
//! endpoint, an optional remote frontier model is interchangeable).

use crate::error::{CoreError, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn system(text: &str) -> Self {
        Self { role: "system".into(), content: Some(text.into()), tool_calls: None, tool_call_id: None }
    }
    pub fn user(text: &str) -> Self {
        Self { role: "user".into(), content: Some(text.into()), tool_calls: None, tool_call_id: None }
    }
    pub fn tool_result(id: &str, content: String) -> Self {
        Self { role: "tool".into(), content: Some(content), tool_calls: None, tool_call_id: Some(id.into()) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "default_tool_type")]
    pub kind: String,
    pub function: FunctionCall,
}

fn default_tool_type() -> String {
    "function".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// JSON-encoded arguments string (OpenAI convention).
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    pub temperature: f64,
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub message: ChatMessage,
}

#[async_trait]
pub trait InferenceClient: Send + Sync {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse>;
    /// Cheap health check so the agent loop can fail fast with guidance.
    async fn healthy(&self) -> bool;
}

pub struct OpenAiCompatClient {
    pub endpoint: String,
    pub api_key: Option<String>,
    http: reqwest::Client,
}

impl OpenAiCompatClient {
    pub fn new(endpoint: &str, api_key: Option<String>) -> Self {
        Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            api_key,
            http: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl InferenceClient for OpenAiCompatClient {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let url = format!("{}/chat/completions", self.endpoint);
        let mut req = self.http.post(&url).json(&request);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| CoreError::Inference(format!("local model unreachable at {url}: {e}")))?;
        let status = resp.status();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| CoreError::Inference(format!("bad inference response: {e}")))?;
        if !status.is_success() {
            return Err(CoreError::Inference(format!("inference HTTP {status}: {body}")));
        }
        let msg = &body["choices"][0]["message"];
        let message: ChatMessage = serde_json::from_value(msg.clone())
            .map_err(|e| CoreError::Inference(format!("unexpected message shape: {e} in {msg}")))?;
        Ok(ChatResponse { message })
    }

    async fn healthy(&self) -> bool {
        let url = format!("{}/models", self.endpoint);
        self.http
            .get(&url)
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}
