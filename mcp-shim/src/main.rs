//! Claude Desktop launches MCP servers as stdio subprocesses — it cannot
//! attach to a running GUI app. This shim bridges the two: it reads
//! newline-delimited JSON-RPC from stdin, forwards each message to the
//! running app's localhost MCP endpoint (with the per-install auth token),
//! and writes responses back to stdout.
//!
//! Endpoint discovery, in order:
//!   1. --endpoint <url> --token <token> arguments
//!   2. ROUGHCUT_MCP_ENDPOINT / ROUGHCUT_MCP_TOKEN environment variables
//!   3. `<data dir>/roughcut/mcp.json` written by the running app
//!
//! Claude Desktop config example:
//! {
//!   "mcpServers": { "roughcut": { "command": "/path/to/roughcut-mcp-shim" } }
//! }

use std::io::{BufRead, Write};

struct Endpoint {
    url: String,
    token: String,
}

fn discover() -> Result<Endpoint, String> {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| {
        args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
    };
    if let (Some(url), Some(token)) = (flag("--endpoint"), flag("--token")) {
        return Ok(Endpoint { url, token });
    }
    if let (Ok(url), Ok(token)) =
        (std::env::var("ROUGHCUT_MCP_ENDPOINT"), std::env::var("ROUGHCUT_MCP_TOKEN"))
    {
        return Ok(Endpoint { url, token });
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
    let endpoint = match discover() {
        Ok(e) => e,
        Err(msg) => {
            // Without an endpoint we can still speak enough JSON-RPC to fail clearly.
            let stdin = std::io::stdin();
            let mut stdout = std::io::stdout();
            for line in stdin.lock().lines().map_while(Result::ok) {
                if line.trim().is_empty() {
                    continue;
                }
                let v: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
                if let Some(id) = v.get("id") {
                    let _ = writeln!(stdout, "{}", error_response(id, &msg));
                    let _ = stdout.flush();
                }
            }
            return;
        }
    };

    let client = reqwest::blocking::Client::new();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        let parsed: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let is_notification = parsed.get("id").is_none();
        let response = client
            .post(&endpoint.url)
            .bearer_auth(&endpoint.token)
            .header("content-type", "application/json")
            .body(line)
            .send();
        if is_notification {
            continue; // notifications get no response on stdout
        }
        let id = parsed.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let out = match response.and_then(|r| r.text()) {
            Ok(body) if !body.trim().is_empty() => body,
            Ok(_) => error_response(&id, "empty response from app"),
            Err(e) => error_response(&id, &format!("app unreachable: {e}")),
        };
        let _ = writeln!(stdout, "{}", out.trim());
        let _ = stdout.flush();
    }
}
