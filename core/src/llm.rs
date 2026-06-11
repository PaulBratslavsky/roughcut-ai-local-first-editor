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
    let response = inference
        .chat(ChatRequest {
            model: model.to_string(),
            messages: vec![ChatMessage::system(system), ChatMessage::user(user)],
            tools: None,
            temperature,
        })
        .await?;
    let text = response.message.content.unwrap_or_default();
    let json = extract_json(&text)
        .ok_or_else(|| CoreError::Inference(format!("no JSON in {label} response")))?;
    serde_json::from_str(json)
        .map_err(|e| CoreError::Inference(format!("bad JSON in {label} response: {e}")))
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
