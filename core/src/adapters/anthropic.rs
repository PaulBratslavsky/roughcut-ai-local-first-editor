//! AnthropicClient: Claude through the Messages API, for Hosted mode.
//!
//! It implements the same `InferenceClient` trait as the local client, so
//! every AI step works unchanged: it translates the app's OpenAI-shaped
//! `ChatRequest` into a Messages request and the reply back. Rust has no
//! official Anthropic SDK, so this speaks the HTTP API directly (never the
//! OpenAI-compatible shim).
//!
//! What it sends, and why:
//! - `system` messages become the top-level `system` text.
//! - A reply this client produced earlier in the same run goes back with its
//!   original content blocks unchanged (thinking included) — current models
//!   require that, and the history to be append-only.
//! - JSON schemas become `output_config.format`, after removing keywords
//!   structured outputs don't accept and closing every object
//!   (`additionalProperties: false`). A root-array schema is wrapped in an
//!   object and unwrapped from the reply.
//! - Never: `temperature` (rejected by current models), `tool_choice`,
//!   `thinking` (adaptive by default), `effort` (the model's default).
//! - `fallbacks: "default"`: a safety refusal is re-run on Anthropic's
//!   recommended fallback model inside the same call; `served_by` reports
//!   which model actually answered.

use super::inference::{
    ChatMessage, ChatRequest, ChatResponse, FunctionCall, InferenceClient, ServedBy, ToolCall, Usage,
};
use crate::error::{CoreError, HostedErrorKind, Result};
use crate::model::RouteReason;
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::time::{Duration, Instant};

/// The Messages API version this client is written against.
pub const API_VERSION: &str = "2026-10-21";
/// Opt-in to server-side refusal fallbacks (`fallbacks: "default"`).
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// The app's replies are short JSON or a tool call; thinking counts too.
const MAX_TOKENS: u32 = 16_000;
const DEFAULT_BASE: &str = "https://api.anthropic.com";
/// Longest `retry-after` honoured before giving up on a 429.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(10);

/// Why a hosted request produced no reply.
#[derive(Debug)]
pub enum HostedFailure {
    /// Anthropic couldn't be reached, or kept failing after retries: the
    /// router may finish the run on the local model.
    Unreachable { reason: RouteReason, detail: String },
    /// Must be shown, never papered over: a `CoreError::Hosted`.
    Fatal(CoreError),
}

impl From<HostedFailure> for CoreError {
    fn from(f: HostedFailure) -> Self {
        match f {
            HostedFailure::Unreachable { detail, .. } => {
                CoreError::Unavailable(format!("Anthropic isn't reachable: {detail}"))
            }
            HostedFailure::Fatal(e) => e,
        }
    }
}

pub struct AnthropicClient {
    key: String,
    base: String,
    http: reqwest::Client,
    /// Waits before each retry of a 429 / 5xx; its length is the retry count.
    retry_delays: Vec<Duration>,
}

impl AnthropicClient {
    pub fn new(key: impl Into<String>) -> Self {
        Self::with_base_url(key, DEFAULT_BASE, vec![Duration::from_secs(1), Duration::from_secs(3)])
    }

    /// A client against another base URL (tests point it at a local stub,
    /// with zero retry delays).
    pub(crate) fn with_base_url(key: impl Into<String>, base: &str, retry_delays: Vec<Duration>) -> Self {
        Self {
            key: key.into(),
            base: base.trim_end_matches('/').to_string(),
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(120))
                .build()
                .unwrap_or_default(),
            retry_delays,
        }
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.base))
            .header("x-api-key", &self.key)
            .header("anthropic-version", API_VERSION)
    }

    /// One Messages request, with retries for 429 / 5xx.
    pub async fn send(&self, request: &ChatRequest) -> std::result::Result<ChatResponse, HostedFailure> {
        let (body, wrapped) = build_body(request);
        let mut attempt = 0;
        loop {
            let sent = self
                .request(reqwest::Method::POST, "/v1/messages")
                .header("anthropic-beta", FALLBACK_BETA)
                .json(&body)
                .send()
                .await;
            let resp = match sent {
                Ok(r) => r,
                Err(e) => return Err(transport_failure(e)),
            };
            let status = resp.status().as_u16();
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<f64>().ok())
                .filter(|s| s.is_finite())
                .map(|s| Duration::from_secs_f64(s.clamp(0.0, MAX_RETRY_AFTER.as_secs_f64())));
            let text = resp.text().await.map_err(|e| HostedFailure::Unreachable {
                reason: RouteReason::Offline,
                detail: e.to_string(),
            })?;
            match status {
                200..=299 => return parse_response(&text, wrapped),
                429 | 500..=599 => {
                    if let Some(&delay) = self.retry_delays.get(attempt) {
                        let wait = match retry_after {
                            Some(after) if status == 429 => after,
                            _ => delay,
                        };
                        tokio::time::sleep(wait).await;
                        attempt += 1;
                        continue;
                    }
                    let reason = if status == 429 { RouteReason::RateLimited } else { RouteReason::ServerError };
                    return Err(HostedFailure::Unreachable { reason, detail: api_message(&text, status) });
                }
                _ => return Err(HostedFailure::Fatal(status_error(status, &text))),
            }
        }
    }

    /// Check the key and access to `model` without spending tokens
    /// (`GET /v1/models/{model}`). Returns how long it took.
    pub async fn check_model(&self, model: &str) -> std::result::Result<Duration, HostedFailure> {
        let started = Instant::now();
        let resp = self
            .request(reqwest::Method::GET, &format!("/v1/models/{model}"))
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(transport_failure)?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        match status {
            200..=299 => Ok(started.elapsed()),
            404 => Err(HostedFailure::Fatal(CoreError::hosted(
                HostedErrorKind::BadRequest,
                format!("this key can't use {model} (404)"),
            ))),
            429 => Err(HostedFailure::Unreachable { reason: RouteReason::RateLimited, detail: api_message(&text, status) }),
            500..=599 => Err(HostedFailure::Unreachable { reason: RouteReason::ServerError, detail: api_message(&text, status) }),
            _ => Err(HostedFailure::Fatal(status_error(status, &text))),
        }
    }
}

/// What the Hosted router sends requests through: the real client, or a test
/// double. Unlike `InferenceClient::chat`, it says whether a failure means
/// "unreachable" (the run may finish locally) or must be shown.
#[async_trait]
pub trait HostedBackend: Send + Sync {
    async fn send(&self, request: &ChatRequest) -> std::result::Result<ChatResponse, HostedFailure>;
}

#[async_trait]
impl HostedBackend for AnthropicClient {
    async fn send(&self, request: &ChatRequest) -> std::result::Result<ChatResponse, HostedFailure> {
        AnthropicClient::send(self, request).await
    }
}

#[async_trait]
impl InferenceClient for AnthropicClient {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        self.send(&request).await.map_err(Into::into)
    }

    /// No network call: a key is all it takes to try.
    async fn healthy(&self) -> bool {
        !self.key.is_empty()
    }
}

/// A request that never got an HTTP answer. Only a failed connection or a
/// timeout means "unreachable"; anything else (an invalid header value from a
/// malformed key, a bad URL) is a problem to show, not to fall back from.
fn transport_failure(e: reqwest::Error) -> HostedFailure {
    if e.is_connect() || e.is_timeout() || e.is_request() && e.source_is_io() {
        HostedFailure::Unreachable { reason: RouteReason::Offline, detail: e.to_string() }
    } else {
        HostedFailure::Fatal(CoreError::hosted(HostedErrorKind::BadRequest, format!("couldn't send the request to Anthropic: {e}")))
    }
}

/// Whether a reqwest error was caused by I/O (a dropped connection mid-way).
trait SourceIsIo {
    fn source_is_io(&self) -> bool;
}

impl SourceIsIo for reqwest::Error {
    fn source_is_io(&self) -> bool {
        let mut source = std::error::Error::source(self);
        while let Some(s) = source {
            if s.is::<std::io::Error>() {
                return true;
            }
            source = s.source();
        }
        false
    }
}

/// The error message Anthropic sent, or the start of the body.
fn api_message(text: &str, status: u16) -> String {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| {
            let t: String = text.chars().take(300).collect();
            if t.trim().is_empty() { format!("HTTP {status}") } else { t }
        })
}

fn status_error(status: u16, text: &str) -> CoreError {
    match status {
        401 | 403 => CoreError::hosted(
            HostedErrorKind::KeyRejected,
            format!("Anthropic rejected the API key ({status}). Update it in Setup."),
        ),
        _ => CoreError::hosted(
            HostedErrorKind::BadRequest,
            format!("Anthropic rejected the request ({status}): {}", api_message(text, status)),
        ),
    }
}

// ------------------------------------------------------------ the request

/// The Messages request body for `req`, and whether the output schema's
/// root array was wrapped (so the reply must be unwrapped).
pub(crate) fn build_body(req: &ChatRequest) -> (Value, bool) {
    let system: Vec<&str> = req
        .messages
        .iter()
        .filter(|m| m.role == "system")
        .filter_map(|m| m.content.as_deref())
        .filter(|t| !t.trim().is_empty())
        .collect();

    // Translate, merging consecutive messages of the same role.
    let mut turns: Vec<(&'static str, Vec<Value>)> = vec![];
    for m in &req.messages {
        let (role, blocks) = match m.role.as_str() {
            "system" => continue,
            "assistant" => ("assistant", assistant_blocks(m)),
            "tool" => (
                "user",
                vec![json!({
                    "type": "tool_result",
                    "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                    "content": m.content.clone().unwrap_or_default(),
                })],
            ),
            _ => ("user", text_block(m.content.as_deref())),
        };
        if blocks.is_empty() {
            continue;
        }
        match turns.last_mut() {
            Some((r, b)) if *r == role => b.extend(blocks),
            _ => turns.push((role, blocks)),
        }
    }
    // All tool results of a turn come first in their user message.
    for (role, blocks) in turns.iter_mut() {
        if *role == "user" {
            blocks.sort_by_key(|b| b["type"] != "tool_result");
        }
    }
    // A conversation can't open on an assistant turn (chat history can).
    if turns.first().is_some_and(|(r, _)| *r == "assistant") {
        turns.insert(0, ("user", vec![json!({"type": "text", "text": "Earlier in this conversation:"})]));
    }

    // Every request the app makes ends on the user's turn (a trailing
    // assistant turn would be a prefill, which current models reject).
    debug_assert!(turns.last().map_or(true, |(r, _)| *r == "user"), "a request must end on a user turn");
    let messages: Vec<Value> = turns.into_iter().map(|(r, b)| json!({"role": r, "content": b})).collect();
    let mut body = json!({
        "model": req.model,
        "max_tokens": MAX_TOKENS,
        "messages": messages,
        // Automatic prompt caching: the chat agent resends a long system
        // prompt and tool list on every step; short prompts just don't cache.
        "cache_control": {"type": "ephemeral"},
        "fallbacks": "default",
    });
    if !system.is_empty() {
        body["system"] = json!(system.join("\n\n"));
    }
    if let Some(tools) = &req.tools {
        body["tools"] = Value::Array(tools.iter().map(convert_tool).collect());
    }
    let mut wrapped = false;
    if let Some(schema) = req.response_format.as_ref().map(|rf| &rf["json_schema"]["schema"]).filter(|s| s.is_object()) {
        let (schema, w) = output_schema(schema);
        wrapped = w;
        body["output_config"] = json!({"format": {"type": "json_schema", "schema": schema}});
    }
    (body, wrapped)
}

fn text_block(text: Option<&str>) -> Vec<Value> {
    match text {
        // The API rejects empty and whitespace-only text blocks.
        Some(t) if !t.trim().is_empty() => vec![json!({"type": "text", "text": t})],
        _ => vec![],
    }
}

fn assistant_blocks(m: &ChatMessage) -> Vec<Value> {
    if let Some(Value::Array(blocks)) = &m.provider_blocks {
        // The blocks must still be the reply they came from: the same tool
        // calls (the app never edits a reply it keeps in history).
        debug_assert_eq!(
            blocks.iter().filter(|b| b["type"] == "tool_use").filter_map(|b| b["id"].as_str()).collect::<Vec<_>>(),
            m.tool_calls.iter().flatten().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            "provider_blocks no longer match the message's tool calls"
        );
        return blocks.clone();
    }
    let mut blocks = text_block(m.content.as_deref());
    for call in m.tool_calls.iter().flatten() {
        let input = serde_json::from_str::<Value>(&call.function.arguments)
            .ok()
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        blocks.push(json!({"type": "tool_use", "id": call.id, "name": call.function.name, "input": input}));
    }
    blocks
}

/// OpenAI `{type: "function", function: {name, description, parameters}}`
/// → Messages `{name, description, input_schema}`.
fn convert_tool(t: &Value) -> Value {
    let f = if t["type"] == "function" { &t["function"] } else { t };
    let schema = if f["parameters"].is_object() {
        f["parameters"].clone()
    } else if f["input_schema"].is_object() {
        f["input_schema"].clone()
    } else {
        json!({"type": "object", "properties": {}})
    };
    json!({
        "name": f["name"],
        "description": f["description"].as_str().unwrap_or(""),
        "input_schema": schema,
    })
}

/// Keywords structured outputs don't accept. Ranges are still enforced: the
/// callers clamp and check what they parse.
const DROPPED_KEYWORDS: &[&str] = &[
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "minLength",
    "maxLength",
    "pattern",
    "minItems",
    "maxItems",
    "uniqueItems",
];

/// The schema structured outputs accept, and whether a root array was
/// wrapped as `{items: [...]}`.
pub(crate) fn output_schema(schema: &Value) -> (Value, bool) {
    let normalized = normalize_schema(schema);
    if normalized["type"] == "array" {
        let wrapped = json!({
            "type": "object",
            "properties": {"items": normalized},
            "required": ["items"],
            "additionalProperties": false,
        });
        (wrapped, true)
    } else {
        (normalized, false)
    }
}

fn normalize_schema(v: &Value) -> Value {
    let Value::Object(map) = v else { return v.clone() };
    let mut out = Map::new();
    for (k, val) in map {
        if DROPPED_KEYWORDS.contains(&k.as_str()) {
            continue;
        }
        let val = match k.as_str() {
            "properties" | "$defs" | "definitions" => match val {
                Value::Object(props) => {
                    Value::Object(props.iter().map(|(name, s)| (name.clone(), normalize_schema(s))).collect())
                }
                other => other.clone(),
            },
            "items" => normalize_schema(val),
            "anyOf" | "allOf" | "oneOf" => match val {
                Value::Array(list) => Value::Array(list.iter().map(normalize_schema).collect()),
                other => other.clone(),
            },
            _ => val.clone(),
        };
        out.insert(k.clone(), val);
    }
    if out.get("type").and_then(Value::as_str) == Some("object") || out.contains_key("properties") {
        out.insert("additionalProperties".into(), Value::Bool(false));
    }
    Value::Object(out)
}

// ------------------------------------------------------------ the reply

pub(crate) fn parse_response(text: &str, wrapped: bool) -> std::result::Result<ChatResponse, HostedFailure> {
    let body: Value = serde_json::from_str(text).map_err(|e| {
        HostedFailure::Fatal(CoreError::hosted(HostedErrorKind::BadRequest, format!("unexpected reply from Anthropic: {e}")))
    })?;
    match body["stop_reason"].as_str() {
        Some("refusal") => {
            return Err(HostedFailure::Fatal(CoreError::hosted(
                HostedErrorKind::Refused,
                "Claude declined this request",
            )))
        }
        Some("max_tokens") => {
            return Err(HostedFailure::Fatal(CoreError::hosted(
                HostedErrorKind::Truncated,
                "Claude's reply was cut off at the token limit",
            )))
        }
        _ => {}
    }
    let blocks = body["content"].as_array().cloned().unwrap_or_default();
    let mut texts: Vec<&str> = vec![];
    let mut calls: Vec<ToolCall> = vec![];
    for b in &blocks {
        match b["type"].as_str() {
            Some("text") => texts.push(b["text"].as_str().unwrap_or("")),
            Some("tool_use") => calls.push(ToolCall {
                id: b["id"].as_str().unwrap_or_default().to_string(),
                kind: "function".into(),
                function: FunctionCall {
                    name: b["name"].as_str().unwrap_or_default().to_string(),
                    arguments: b["input"].to_string(),
                },
            }),
            _ => {} // thinking, fallback notices: kept in provider_blocks
        }
    }
    let mut content = texts.join("\n");
    if wrapped {
        if let Some(items) = serde_json::from_str::<Value>(content.trim()).ok().and_then(|v| v.get("items").cloned()) {
            content = items.to_string();
        }
    }
    let message = ChatMessage {
        role: "assistant".into(),
        content: Some(content),
        tool_calls: (!calls.is_empty()).then_some(calls),
        tool_call_id: None,
        provider_blocks: Some(Value::Array(blocks)),
    };
    let u = &body["usage"];
    let usage = Usage {
        input_tokens: u["input_tokens"].as_u64().unwrap_or(0)
            + u["cache_read_input_tokens"].as_u64().unwrap_or(0)
            + u["cache_creation_input_tokens"].as_u64().unwrap_or(0),
        output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
    };
    Ok(ChatResponse {
        message,
        served_by: Some(ServedBy { model: body["model"].as_str().unwrap_or_default().to_string(), hosted: true }),
        usage: Some(usage),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::inference::ChatMessage as M;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    fn req(messages: Vec<M>) -> ChatRequest {
        ChatRequest { model: "claude-opus-5-5".into(), messages, tools: None, temperature: 0.2, response_format: None }
    }

    fn assistant_with_call(text: &str, id: &str, name: &str, args: &str) -> M {
        let mut m = M::assistant(text);
        m.tool_calls = Some(vec![ToolCall {
            id: id.into(),
            kind: "function".into(),
            function: FunctionCall { name: name.into(), arguments: args.into() },
        }]);
        m
    }

    #[test]
    fn system_messages_join_and_sampling_settings_are_never_sent() {
        let (body, wrapped) = build_body(&req(vec![M::system("one"), M::system("two"), M::user("hi")]));
        assert_eq!(body["system"], "one\n\ntwo");
        assert_eq!(body["messages"], json!([{"role": "user", "content": [{"type": "text", "text": "hi"}]}]));
        assert_eq!(body["model"], "claude-opus-5-5");
        assert_eq!(body["max_tokens"], 16_000);
        assert_eq!(body["cache_control"], json!({"type": "ephemeral"}));
        assert_eq!(body["fallbacks"], "default");
        for absent in ["temperature", "tool_choice", "thinking", "output_config", "tools"] {
            assert!(body.get(absent).is_none(), "{absent} must not be sent");
        }
        assert!(!wrapped);
    }

    #[test]
    fn tool_results_and_a_trailing_nudge_share_one_user_turn_results_first() {
        let (body, _) = build_body(&req(vec![
            M::system("s"),
            M::user("Instruction: cut the tangent"),
            assistant_with_call("Looking.", "call_1", "find_segments", r#"{"query":"weekend"}"#),
            M::tool_result("call_1", "[{\"id\":\"a\"}]".into()),
            M::user("You have gathered enough. Act now."),
        ]));
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(
            msgs[1]["content"],
            json!([
                {"type": "text", "text": "Looking."},
                {"type": "tool_use", "id": "call_1", "name": "find_segments", "input": {"query": "weekend"}}
            ])
        );
        let last = msgs[2]["content"].as_array().unwrap();
        assert_eq!(last[0]["type"], "tool_result");
        assert_eq!(last[0]["tool_use_id"], "call_1");
        assert_eq!(last[1], json!({"type": "text", "text": "You have gathered enough. Act now."}));
    }

    #[test]
    fn a_text_before_tool_results_still_puts_the_results_first() {
        let (body, _) = build_body(&req(vec![
            M::user("go"),
            assistant_with_call("", "c1", "t", "{}"),
            M::user("note"),
            M::tool_result("c1", "done".into()),
        ]));
        let last = body["messages"][2]["content"].as_array().unwrap();
        assert_eq!(last[0]["type"], "tool_result");
        assert_eq!(last[1]["text"], "note");
    }

    #[test]
    fn history_that_opens_on_an_assistant_turn_gets_a_user_turn_first() {
        let (body, _) = build_body(&req(vec![
            M::system("s"),
            M::assistant("Earlier answer"),
            M::user("first"),
            M::user("  \n"),
            M::user("second"),
        ]));
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"][0]["text"], "Earlier in this conversation:");
        assert_eq!(msgs[1]["role"], "assistant");
        // The two user turns merged; the whitespace-only one dropped.
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[2]["content"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_claude_reply_goes_back_with_its_blocks_unchanged() {
        let blocks = json!([
            {"type": "thinking", "thinking": "", "signature": "sig"},
            {"type": "tool_use", "id": "toolu_1", "name": "cut_range", "input": {"start": 1.0}}
        ]);
        let mut reply = assistant_with_call("", "toolu_1", "cut_range", r#"{"start":1.0}"#);
        reply.provider_blocks = Some(blocks.clone());
        let (body, _) = build_body(&req(vec![M::user("go"), reply, M::tool_result("toolu_1", "ok".into())]));
        assert_eq!(body["messages"][1]["content"], blocks);
    }

    #[test]
    fn openai_tools_become_messages_tools() {
        let mut r = req(vec![M::user("go")]);
        r.tools = Some(vec![json!({
            "type": "function",
            "function": {"name": "cut_range", "description": "Cut", "parameters": {"type": "object", "properties": {"start": {"type": "number"}}}}
        })]);
        let (body, _) = build_body(&r);
        assert_eq!(
            body["tools"],
            json!([{"name": "cut_range", "description": "Cut", "input_schema": {"type": "object", "properties": {"start": {"type": "number"}}}}])
        );
        assert!(body.get("tool_choice").is_none());
    }

    /// Every object closed, no dropped keyword anywhere.
    fn assert_accepted(v: &Value, path: &str) {
        let Value::Object(map) = v else { return };
        for k in DROPPED_KEYWORDS {
            assert!(!map.contains_key(*k), "{path} still has {k}");
        }
        if map.get("type").and_then(Value::as_str) == Some("object") {
            assert_eq!(map.get("additionalProperties"), Some(&Value::Bool(false)), "{path} not closed");
        }
        if let Some(Value::Object(props)) = map.get("properties") {
            for (name, s) in props {
                assert_accepted(s, &format!("{path}.{name}"));
            }
        }
        if let Some(items) = map.get("items") {
            assert_accepted(items, &format!("{path}[]"));
        }
    }

    #[test]
    fn the_apps_four_schemas_are_accepted() {
        let cases = [
            ("short_scores", crate::clip_finder::short_scores_schema(), true),
            ("beats", crate::story::beats_schema(), true),
            ("verdicts", crate::story::verdicts_schema(), true),
            ("story_plan", crate::story::story_plan_schema(), false),
        ];
        for (name, schema, root_array) in cases {
            let (out, wrapped) = output_schema(&schema);
            assert_eq!(wrapped, root_array, "{name}");
            assert_eq!(out["type"], "object", "{name}: the root is an object");
            assert_accepted(&out, name);
            if wrapped {
                assert_eq!(out["required"], json!(["items"]));
                assert_eq!(out["properties"]["items"]["type"], "array");
            }
        }
        // Content survives: enums, required lists, item types.
        let (beats, _) = output_schema(&crate::story::beats_schema());
        let item = &beats["properties"]["items"]["items"];
        assert_eq!(item["properties"]["role"]["enum"][0], "hook");
        assert_eq!(item["properties"]["cut_priority"], json!({"type": "integer"}));
        assert_eq!(item["required"], json!(["title", "first", "last"]));
        // A property NAMED like a dropped keyword is kept.
        let (odd, _) = output_schema(&json!({"type": "object", "properties": {"minimum": {"type": "number"}}}));
        assert!(odd["properties"]["minimum"].is_object());
    }

    #[test]
    fn a_schema_request_uses_output_config_and_unwraps_the_reply() {
        let mut r = req(vec![M::user("score")]);
        r.response_format = Some(json!({
            "type": "json_schema",
            "json_schema": {"name": "short_scores", "schema": crate::clip_finder::short_scores_schema(), "strict": true}
        }));
        let (body, wrapped) = build_body(&r);
        assert!(wrapped);
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        assert!(body.get("response_format").is_none());
        let reply = json!({
            "model": "claude-opus-5-5", "stop_reason": "end_turn",
            "content": [{"type": "thinking", "thinking": ""}, {"type": "text", "text": "{\"items\":[{\"index\":0,\"score\":91,\"title\":\"T\",\"reason\":\"R\"}]}"}],
            "usage": {"input_tokens": 100, "output_tokens": 20, "cache_read_input_tokens": 50}
        });
        let out = parse_response(&reply.to_string(), true).unwrap();
        let parsed: Value = serde_json::from_str(out.message.content.as_deref().unwrap()).unwrap();
        assert_eq!(parsed[0]["score"], 91);
        assert_eq!(out.served_by, Some(ServedBy { model: "claude-opus-5-5".into(), hosted: true }));
        assert_eq!(out.usage, Some(Usage { input_tokens: 150, output_tokens: 20 }));
    }

    #[test]
    fn replies_map_text_tool_calls_refusals_and_truncation() {
        let reply = json!({
            "model": "claude-opus-4-8", "stop_reason": "tool_use",
            "content": [
                {"type": "text", "text": "Cutting."},
                {"type": "tool_use", "id": "toolu_9", "name": "cut_range", "input": {"start": 3.5, "end": 9}}
            ]
        });
        let out = parse_response(&reply.to_string(), false).unwrap();
        assert_eq!(out.message.content.as_deref(), Some("Cutting."));
        let call = &out.message.tool_calls.as_ref().unwrap()[0];
        assert_eq!(call.id, "toolu_9");
        let args: Value = serde_json::from_str(&call.function.arguments).unwrap();
        assert_eq!(args["start"], 3.5);
        assert_eq!(out.message.provider_blocks.as_ref().unwrap().as_array().unwrap().len(), 2);
        assert_eq!(out.served_by.unwrap().model, "claude-opus-4-8", "the fallback model is reported");

        let refused = json!({"model": "m", "stop_reason": "refusal", "content": []});
        assert!(matches!(
            parse_response(&refused.to_string(), false),
            Err(HostedFailure::Fatal(CoreError::Hosted { kind: HostedErrorKind::Refused, .. }))
        ));
        let cut = json!({"model": "m", "stop_reason": "max_tokens", "content": []});
        assert!(matches!(
            parse_response(&cut.to_string(), false),
            Err(HostedFailure::Fatal(CoreError::Hosted { kind: HostedErrorKind::Truncated, .. }))
        ));
    }

    // ------------------------------------------------ against a local stub

    type Seen = Arc<Mutex<Vec<(axum::http::HeaderMap, String)>>>;

    /// A fake Anthropic: answers each request with the next scripted
    /// (status, extra header, body), and records what it was sent.
    async fn stub(script: Vec<(u16, Option<(&'static str, &'static str)>, Value)>) -> (String, Seen) {
        use axum::{body::Bytes, extract::State, http::HeaderMap, response::IntoResponse, routing::any, Router};
        type Script = Arc<Mutex<VecDeque<(u16, Option<(&'static str, &'static str)>, Value)>>>;
        let seen: Seen = Arc::new(Mutex::new(vec![]));
        let script: Script = Arc::new(Mutex::new(script.into()));
        async fn handle(
            State((seen, script)): State<(Seen, Script)>,
            headers: HeaderMap,
            body: Bytes,
        ) -> axum::response::Response {
            seen.lock().unwrap().push((headers, String::from_utf8_lossy(&body).to_string()));
            let (status, header, body) = script.lock().unwrap().pop_front().expect("unexpected extra request");
            let mut resp = (axum::http::StatusCode::from_u16(status).unwrap(), body.to_string()).into_response();
            if let Some((k, v)) = header {
                resp.headers_mut().insert(k, v.parse().unwrap());
            }
            resp
        }
        let app = Router::new().route("/{*path}", any(handle)).with_state((seen.clone(), script));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), seen)
    }

    fn ok_reply(text: &str) -> Value {
        json!({"model": "claude-opus-5-5", "stop_reason": "end_turn", "content": [{"type": "text", "text": text}], "usage": {"input_tokens": 5, "output_tokens": 2}})
    }

    fn client(base: &str) -> AnthropicClient {
        AnthropicClient::with_base_url("sk-test", base, vec![Duration::ZERO, Duration::ZERO])
    }

    #[tokio::test]
    async fn a_request_carries_the_key_version_and_fallback_beta() {
        let (base, seen) = stub(vec![(200, None, ok_reply("hello"))]).await;
        let out = client(&base).send(&req(vec![M::user("hi")])).await.unwrap();
        assert_eq!(out.message.content.as_deref(), Some("hello"));
        let seen = seen.lock().unwrap();
        let (headers, body) = &seen[0];
        assert_eq!(headers["x-api-key"], "sk-test");
        assert_eq!(headers["anthropic-version"], API_VERSION);
        assert_eq!(headers["anthropic-beta"], FALLBACK_BETA);
        let body: Value = serde_json::from_str(body).unwrap();
        assert!(body.get("temperature").is_none());
    }

    #[tokio::test]
    async fn a_rejected_key_fails_at_once_without_retrying() {
        let (base, seen) = stub(vec![(401, None, json!({"error": {"message": "invalid x-api-key"}}))]).await;
        let err = client(&base).send(&req(vec![M::user("hi")])).await.unwrap_err();
        let HostedFailure::Fatal(e) = err else { panic!("401 is fatal") };
        assert!(e.is_fatal_model_error());
        assert_eq!(e.code(), "hosted_key_rejected");
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_bad_request_is_shown_with_the_apis_message() {
        let (base, _) = stub(vec![(400, None, json!({"error": {"message": "messages: roles must alternate"}}))]).await;
        let HostedFailure::Fatal(e) = client(&base).send(&req(vec![M::user("hi")])).await.unwrap_err() else {
            panic!("400 is fatal")
        };
        assert_eq!(e.code(), "hosted_bad_request");
        assert!(e.to_string().contains("roles must alternate"), "{e}");
    }

    #[tokio::test]
    async fn rate_limits_and_overloads_retry_then_report_unreachable() {
        let busy = json!({"error": {"message": "slow down"}});
        let (base, seen) = stub(vec![
            (429, Some(("retry-after", "0")), busy.clone()),
            (429, Some(("retry-after", "0.01")), busy.clone()),
            (429, None, busy),
        ])
        .await;
        let err = client(&base).send(&req(vec![M::user("hi")])).await.unwrap_err();
        assert!(matches!(err, HostedFailure::Unreachable { reason: RouteReason::RateLimited, .. }));
        assert_eq!(seen.lock().unwrap().len(), 3, "the first try and two retries");

        let (base, seen) = stub(vec![(529, None, json!({})), (200, None, ok_reply("fine"))]).await;
        let out = client(&base).send(&req(vec![M::user("hi")])).await.unwrap();
        assert_eq!(out.message.content.as_deref(), Some("fine"));
        assert_eq!(seen.lock().unwrap().len(), 2);

        // Any 5xx is "Anthropic unavailable" once the retries run out.
        let (base, _) = stub(vec![(522, None, json!({})), (500, None, json!({})), (503, None, json!({}))]).await;
        let err = client(&base).send(&req(vec![M::user("hi")])).await.unwrap_err();
        assert!(matches!(err, HostedFailure::Unreachable { reason: RouteReason::ServerError, .. }));
    }

    #[tokio::test]
    async fn other_client_errors_are_shown_never_retried() {
        for (status, code) in [(403, "hosted_key_rejected"), (404, "hosted_bad_request"), (413, "hosted_bad_request"), (422, "hosted_bad_request")] {
            let (base, seen) = stub(vec![(status, None, json!({"error": {"message": "nope"}}))]).await;
            let HostedFailure::Fatal(e) = client(&base).send(&req(vec![M::user("hi")])).await.unwrap_err() else {
                panic!("{status} is fatal")
            };
            assert_eq!(e.code(), code, "{status}");
            assert_eq!(seen.lock().unwrap().len(), 1, "{status} is not retried");
        }
    }

    #[tokio::test]
    async fn a_malformed_key_is_an_error_not_offline() {
        let c = AnthropicClient::with_base_url("sk-bad\nkey", "http://127.0.0.1:9", vec![]);
        let err = c.send(&req(vec![M::user("hi")])).await.unwrap_err();
        assert!(matches!(err, HostedFailure::Fatal(_)), "{err:?}");
    }

    #[tokio::test]
    async fn no_server_is_unreachable_offline() {
        // A port nothing listens on.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let err = client(&base).send(&req(vec![M::user("hi")])).await.unwrap_err();
        assert!(matches!(err, HostedFailure::Unreachable { reason: RouteReason::Offline, .. }));
    }

    #[tokio::test]
    async fn checking_a_key_spends_no_tokens() {
        let (base, seen) = stub(vec![
            (200, None, json!({"id": "claude-opus-5-5"})),
            (401, None, json!({})),
            (404, None, json!({})),
        ])
        .await;
        let c = client(&base);
        assert!(c.check_model("claude-opus-5-5").await.is_ok());
        let HostedFailure::Fatal(e) = c.check_model("claude-opus-5-5").await.unwrap_err() else { panic!() };
        assert_eq!(e.code(), "hosted_key_rejected");
        let HostedFailure::Fatal(e) = c.check_model("claude-x").await.unwrap_err() else { panic!() };
        assert!(e.to_string().contains("can't use claude-x"), "{e}");
        assert!(seen.lock().unwrap().iter().all(|(_, body)| body.is_empty()), "GETs, no message bodies");
    }
}
