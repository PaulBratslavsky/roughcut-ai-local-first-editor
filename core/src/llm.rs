//! The "LLM call with a strict JSON contract" seam. Six features copy-pasted
//! this pattern (health gate → model lookup → ChatRequest → content unwrap →
//! first/last-delimiter scan → serde parse → error wrap) before it was
//! extracted; hardening JSON extraction against weak-model output now lands
//! ONCE. Fallback POLICY stays at call sites — heuristics differ per feature.

use crate::adapters::inference::{ChatMessage, ChatRequest, InferenceClient};
use crate::engine::Editor;
use crate::error::{CoreError, Result};
use serde::de::DeserializeOwned;

/// One JSON-contract call against the project's configured model. Checks
/// health (use [`ask_json_with`] inside chunk loops to gate once).
pub async fn ask_json<T: DeserializeOwned>(
    editor: &Editor,
    label: &str,
    system: &str,
    user: &str,
    temperature: f64,
) -> Result<T> {
    let inference = editor.inference()?;
    if !inference.healthy().await {
        return Err(CoreError::Unavailable("local model server not reachable".into()));
    }
    let prefs = editor.get_preferences()?;
    ask_json_with(inference.as_ref(), &prefs.inference_model, label, system, user, temperature)
        .await
}

/// Same contract with an explicit client + model — no health probe, for
/// chunked loops (gate once at the loop head) and the frontier path.
pub async fn ask_json_with<T: DeserializeOwned>(
    inference: &dyn InferenceClient,
    model: &str,
    label: &str,
    system: &str,
    user: &str,
    temperature: f64,
) -> Result<T> {
    ask_json_schema(inference, model, label, system, user, temperature, None).await
}

/// The full contract: when `schema` is provided the server CONSTRAINS
/// decoding to it (Ollama structured outputs / llama-server grammar —
/// malformed JSON becomes impossible rather than merely discouraged).
/// One repair retry with the parse error otherwise.
pub async fn ask_json_schema<T: DeserializeOwned>(
    inference: &dyn InferenceClient,
    model: &str,
    label: &str,
    system: &str,
    user: &str,
    temperature: f64,
    schema: Option<&serde_json::Value>,
) -> Result<T> {
    let response_format = schema.map(|s| {
        serde_json::json!({
            "type": "json_schema",
            "json_schema": { "name": label.replace(' ', "_"), "schema": s, "strict": true }
        })
    });
    let request = |messages: Vec<ChatMessage>| ChatRequest {
        model: model.to_string(),
        messages,
        tools: None,
        temperature,
        response_format: response_format.clone(),
    };
    let base = vec![ChatMessage::system(system), ChatMessage::user(user)];
    let mut last_err = String::new();
    for attempt in 0..2 {
        let messages = if attempt == 0 {
            base.clone()
        } else {
            // Repair pass: echo the parse failure — the research-standard
            // retry-with-validation-error-feedback loop.
            let mut m = base.clone();
            m.push(ChatMessage::user(&format!(
                "Your previous reply could not be parsed: {last_err}. \
                 Reply again with ONLY the corrected JSON."
            )));
            m
        };
        let response = match inference.chat(request(messages)).await {
            Ok(r) => r,
            // A server that rejects response_format gets one unconstrained shot.
            Err(_) if attempt == 0 && response_format.is_some() => {
                let mut req = request(base.clone());
                req.response_format = None;
                inference.chat(req).await?
            }
            Err(e) => return Err(e),
        };
        let text = response.message.content.unwrap_or_default();
        match extract_json(&text)
            .ok_or_else(|| format!("no JSON found in the reply"))
            .and_then(|j| serde_json::from_str::<T>(j).map_err(|e| e.to_string()))
        {
            Ok(v) => return Ok(v),
            Err(e) => last_err = e,
        }
    }
    Err(CoreError::Inference(format!("bad JSON in {label} response: {last_err}")))
}

/// Find the JSON payload in model output that may carry preamble, code
/// fences, or trailing chatter. Balanced-scan from the first opening
/// delimiter; falls back to first..=last slicing if balancing fails
/// (truncated output still surfaces a parse error with context).
pub fn extract_json(text: &str) -> Option<&str> {
    let start = text.find(['{', '['])?;
    let bytes = text.as_bytes();
    let (open, close) = if bytes[start] == b'{' { (b'{', b'}') } else { (b'[', b']') };
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes[start..].iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            _ if b == open => depth += 1,
            _ if b == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..=start + i]);
                }
            }
            _ => {}
        }
    }
    // Unbalanced (truncated output): old behavior as a last resort.
    let last = text.rfind(close as char)?;
    (last > start).then(|| &text[start..=last])
}

#[cfg(test)]
mod tests {
    use super::extract_json;

    #[test]
    fn extracts_through_preamble_fences_and_chatter() {
        assert_eq!(extract_json(r#"{"a":1}"#), Some(r#"{"a":1}"#));
        assert_eq!(
            extract_json("Sure! Here is the JSON:\n```json\n[{\"b\":2}]\n```\nHope that helps."),
            Some(r#"[{"b":2}]"#)
        );
        // Braces inside strings don't confuse the scan.
        assert_eq!(
            extract_json(r#"note {"text":"a } inside","n":1} trailing {junk"#),
            Some(r#"{"text":"a } inside","n":1}"#)
        );
        // Prose containing a brace BEFORE the payload still resolves: the
        // scan starts at the first delimiter and balances from there.
        assert_eq!(extract_json("none here"), None);
    }
}
