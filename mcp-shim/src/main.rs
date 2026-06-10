//! Claude Desktop launches MCP servers as stdio subprocesses — it cannot
//! attach to a running GUI app. This shim bridges the two: it reads
//! newline-delimited JSON-RPC from stdin, forwards each message to the
//! running app's localhost MCP endpoint (with the per-install auth token),
//! and writes responses back to stdout.
//!
//! The app binds a NEW random port each launch and rewrites its endpoint
//! file, so discovery is re-run lazily: on the first request, and again
//! whenever a send fails. A long-lived shim therefore survives app restarts.
//!
//! Endpoint discovery, in order:
//!   1. --endpoint <url> --token <token> arguments (pinned; no re-discovery)
//!   2. ROUGHCUT_MCP_ENDPOINT / ROUGHCUT_MCP_TOKEN environment variables
//!   3. `<data dir>/roughcut/mcp.json` written by the running app
//!
//! Claude Desktop config example:
//! {
//!   "mcpServers": { "roughcut": { "command": "/path/to/roughcut-mcp-shim" } }
//! }

use std::io::{BufRead, Write};

#[derive(Clone)]
struct Endpoint {
    url: String,
    token: String,
    /// From args/env — never re-discovered.
    pinned: bool,
}

fn discover() -> Result<Endpoint, String> {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| {
        args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
    };
    if let (Some(url), Some(token)) = (flag("--endpoint"), flag("--token")) {
        return Ok(Endpoint { url, token, pinned: true });
    }
    if let (Ok(url), Ok(token)) =
        (std::env::var("ROUGHCUT_MCP_ENDPOINT"), std::env::var("ROUGHCUT_MCP_TOKEN"))
    {
        return Ok(Endpoint { url, token, pinned: true });
    }
    let path = dirs::data_dir()
        .ok_or("no data dir on this platform")?
        .join("roughcut")
        .join("mcp.json");
    let raw = std::fs::read_to_string(&path).map_err(|_| {
        format!(
            "RoughCut does not appear to be running (no endpoint file at {}). \
             Start the RoughCut app, then retry.",
            path.display()
        )
    })?;
    let v: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("bad endpoint file: {e}"))?;
    Ok(Endpoint {
        url: v["url"].as_str().unwrap_or_default().to_string(),
        token: v["token"].as_str().unwrap_or_default().to_string(),
        pinned: false,
    })
}

fn error_response(id: &serde_json::Value, message: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0", "id": id,
        "error": { "code": -32000, "message": message }
    })
    .to_string()
}

fn main() {
    let client = reqwest::blocking::Client::new();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut endpoint: Option<Endpoint> = None;

    for line in stdin.lock().lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        let parsed: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let is_notification = parsed.get("id").is_none();
        let id = parsed.get("id").cloned().unwrap_or(serde_json::Value::Null);

        // Send with one re-discovery retry: the app may have (re)started since
        // the endpoint was last read, on a fresh port.
        let mut response: Result<String, String> = Err(String::new());
        for attempt in 0..2 {
            if endpoint.is_none() {
                match discover() {
                    Ok(ep) => endpoint = Some(ep),
                    Err(msg) => {
                        response = Err(msg);
                        break;
                    }
                }
            }
            let ep = endpoint.as_ref().unwrap();
            match client
                .post(&ep.url)
                .bearer_auth(&ep.token)
                .header("content-type", "application/json")
                .body(line.clone())
                .send()
                .and_then(|r| r.text())
            {
                Ok(body) => {
                    response = Ok(body);
                    break;
                }
                Err(e) => {
                    response = Err(format!(
                        "RoughCut app unreachable ({e}). Start the app, then retry."
                    ));
                    // Stale port? Re-discover and retry once (unless pinned).
                    if ep.pinned || attempt == 1 {
                        break;
                    }
                    endpoint = None;
                }
            }
        }

        if is_notification {
            continue; // notifications get no response on stdout
        }
        let out = match response {
            Ok(body) if !body.trim().is_empty() => body,
            Ok(_) => error_response(&id, "empty response from app"),
            Err(msg) => error_response(&id, &msg),
        };
        let _ = writeln!(stdout, "{}", out.trim());
        let _ = stdout.flush();
    }
}
