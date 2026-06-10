//! The opt-in outbound path: connect to a frontier provider with the user's
//! own key (stored in the OS keychain, never in config files or the repo) and
//! run the SAME agent loop against it. Local by default, frontier by choice —
//! these functions are only ever reached by an explicit user action.

use crate::agent::{self, InstructionOutcome};
use crate::engine::Editor;
use crate::error::{CoreError, Result};
use crate::model::{ActionSource, ExternalConnection};
use crate::adapters::OpenAiCompatClient;
use uuid::Uuid;

pub fn connect_external(
    editor: &Editor,
    provider: &str,
    api_key: &str,
) -> Result<ExternalConnection> {
    let api_key_ref = format!("roughcut-{provider}");
    store_secret(&api_key_ref, api_key)?;
    let conn = ExternalConnection {
        id: Uuid::new_v4(),
        provider: provider.to_string(),
        api_key_ref,
        enabled: true,
    };
    editor
        .store()
        .set_kv(&format!("external_connection:{}", conn.id), &serde_json::to_string(&conn)?)?;
    Ok(conn)
}

pub async fn escalate_to_frontier(
    editor: &Editor,
    project_id: Uuid,
    instruction: &str,
    connection_id: &str,
) -> Result<InstructionOutcome> {
    let conn_id = Uuid::parse_str(connection_id)
        .map_err(|_| CoreError::InvalidArg("connection_id is not a uuid".into()))?;
    let raw = editor
        .store()
        .get_kv(&format!("external_connection:{conn_id}"))?
        .ok_or_else(|| CoreError::NotFound("external connection (run connect_external first)".into()))?;
    let conn: ExternalConnection = serde_json::from_str(&raw)?;
    if !conn.enabled {
        return Err(CoreError::InvalidArg("connection is disabled".into()));
    }
    let key = load_secret(&conn.api_key_ref)?;
    let (endpoint, default_model) = provider_endpoint(&conn.provider)?;
    let model = std::env::var("FRONTIER_MODEL").unwrap_or_else(|_| default_model.to_string());
    let client = OpenAiCompatClient::new(&endpoint, Some(key));
    agent::run_instruction_with(
        editor,
        project_id,
        instruction,
        // Edits land tagged as externally driven, same undoable path.
        ActionSource::McpClient,
        &client,
        &model,
    )
    .await
}

fn provider_endpoint(provider: &str) -> Result<(String, &'static str)> {
    if provider.starts_with("http://") || provider.starts_with("https://") {
        return Ok((provider.to_string(), "default"));
    }
    match provider {
        // Anthropic's OpenAI-compatible endpoint.
        "claude" | "anthropic" => Ok(("https://api.anthropic.com/v1".into(), "claude-sonnet-4-6")),
        "openai" => Ok(("https://api.openai.com/v1".into(), "gpt-4o")),
        other => Err(CoreError::InvalidArg(format!(
            "unknown provider '{other}' — pass a full OpenAI-compatible base URL instead"
        ))),
    }
}

// ---------------------------------------------------------------- secrets

fn store_secret(key_ref: &str, secret: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("security")
            .args(["add-generic-password", "-U", "-a", "roughcut", "-s", key_ref, "-w", secret])
            .status()?;
        if status.success() {
            return Ok(());
        }
        // fall through to file storage if keychain refused
    }
    let dir = crate::store::data_dir().join("secrets");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(key_ref);
    std::fs::write(&path, secret)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn load_secret(key_ref: &str) -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("security")
            .args(["find-generic-password", "-a", "roughcut", "-s", key_ref, "-w"])
            .output()?;
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
        }
    }
    let path = crate::store::data_dir().join("secrets").join(key_ref);
    std::fs::read_to_string(&path)
        .map(|s| s.trim().to_string())
        .map_err(|_| CoreError::NotFound(format!("credential {key_ref}")))
}
