use crate::engine::Editor;
use crate::error::Result;
use crate::model::ActionSource;
use crate::tools;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use rand::distributions::Alphanumeric;
use rand::Rng;
use serde_json::{json, Value};

const PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Clone)]
struct McpState {
    editor: Editor,
    token: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct EndpointInfo {
    pub url: String,
    pub token: String,
}

/// Start the MCP server on a localhost-only port. Persists the per-install
/// token and the current endpoint to `<data dir>/mcp.json` so the stdio shim
/// can find the running app.
pub async fn start(editor: Editor) -> Result<EndpointInfo> {
    let token = match editor.store().get_kv("mcp_token")? {
        Some(t) => t,
        None => {
            let t: String =
                rand::thread_rng().sample_iter(&Alphanumeric).take(40).map(char::from).collect();
            editor.store().set_kv("mcp_token", &t)?;
            t
        }
    };

    let state = McpState { editor, token: token.clone() };
    let app = Router::new().route("/mcp", post(handle)).with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let url = format!("http://127.0.0.1:{port}/mcp");

    let info = EndpointInfo { url: url.clone(), token: token.clone() };
    let endpoint_file = crate::store::data_dir().join("mcp.json");
    if let Some(parent) = endpoint_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&endpoint_file, serde_json::to_string_pretty(&json!(&info))?)?;

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("mcp server stopped: {e}");
        }
    });
    Ok(info)
}

fn authorized(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.strip_prefix("Bearer ").map(|t| t == token).unwrap_or(false))
        .unwrap_or(false)
}

async fn handle(
    State(state): State<McpState>,
    headers: HeaderMap,
    Json(req): Json<Value>,
) -> (StatusCode, Json<Value>) {
    if !authorized(&headers, &state.token) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "jsonrpc": "2.0", "id": req.get("id"),
                "error": { "code": -32001, "message": "invalid or missing auth token" } })),
        );
    }
    let id = req.get("id").cloned();
    let method = req["method"].as_str().unwrap_or_default().to_string();

    // Notifications need no response body.
    if id.is_none() {
        return (StatusCode::ACCEPTED, Json(json!({})));
    }

    let result: std::result::Result<Value, (i64, String)> = match method.as_str() {
        "initialize" => {
            let requested =
                req["params"]["protocolVersion"].as_str().unwrap_or(PROTOCOL_VERSION).to_string();
            Ok(json!({
                "protocolVersion": requested,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "roughcut", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Local-first AI video editor. YOU are the orchestrator: list_projects to find work, read_transcript (paged) or find_segments (semantic search) to understand the footage, then apply_edits to land a batch of cuts/trims/padding in one call. Everything is non-destructive and undoable; export hands off to an NLE."
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => {
            // External clients are orchestrators themselves: granular tools +
            // the apply_edits batch, no meta-tools (see tools::mcp_defs).
            let defs: Vec<Value> = tools::mcp_defs()
                .into_iter()
                .map(|d| {
                    json!({ "name": d.name, "description": d.description, "inputSchema": d.input_schema })
                })
                .collect();
            Ok(json!({ "tools": defs }))
        }
        "tools/call" => {
            let name = req["params"]["name"].as_str().unwrap_or_default().to_string();
            let args = req["params"]["arguments"].clone();
            match tools::dispatch(&state.editor, &name, &args, ActionSource::McpClient).await {
                Ok(v) => Ok(json!({
                    "content": [{ "type": "text", "text": serde_json::to_string(&v).unwrap_or_default() }],
                    "isError": false
                })),
                Err(e) => Ok(json!({
                    "content": [{ "type": "text", "text": e.to_json().to_string() }],
                    "isError": true
                })),
            }
        }
        other => Err((-32601, format!("method not found: {other}"))),
    };

    let body = match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err((code, message)) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        }
    };
    (StatusCode::OK, Json(body))
}
