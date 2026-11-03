//! Tauri shell: registers the IPC commands (one generic `call_tool` plus a
//! couple of conveniences), forwards core events to the webview, and starts
//! the localhost MCP server.

use roughcut_core::events::EventSink;
use roughcut_core::mcp::server::EndpointInfo;
use roughcut_core::{tools, Editor};
use serde_json::Value;
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};

struct TauriSink(tauri::AppHandle);

impl EventSink for TauriSink {
    fn emit(&self, event: &str, payload: Value) {
        let _ = self.0.emit(event, payload);
    }
}

struct AppState {
    editor: Editor,
    mcp: Mutex<Option<EndpointInfo>>,
}

#[tauri::command]
async fn call_tool(
    state: State<'_, AppState>,
    name: String,
    args: Value,
) -> Result<Value, Value> {
    tools::dispatch(&state.editor, &name, &args, tools::Caller::Ui)
        .await
        .map_err(|e| e.to_json())
}

#[tauri::command]
fn list_tools() -> Vec<tools::ToolDef> {
    tools::defs(tools::Caller::Ui)
}

#[tauri::command]
fn mcp_endpoint_info(state: State<'_, AppState>) -> Option<EndpointInfo> {
    state.mcp.lock().unwrap().clone()
}

#[tauri::command]
fn demo_mode(state: State<'_, AppState>) -> bool {
    state.editor.demo_mode()
}

#[tauri::command]
fn reveal_path(path: String) -> Result<(), Value> {
    #[cfg(target_os = "macos")]
    std::process::Command::new("open")
        .args(["-R", &path])
        .spawn()
        .map_err(|e| serde_json::json!({ "error": { "code": "io", "message": e.to_string() } }))?;
    #[cfg(not(target_os = "macos"))]
    let _ = path;
    Ok(())
}

/// Web pages the app links to, opened in the system browser (a link in the
/// webview can't open one by itself). Only these sites: nothing a page could
/// be talked into opening.
const EXTERNAL_SITES: &[&str] = &[
    "https://console.anthropic.com",
    "https://ollama.com",
    "https://www.blackmagicdesign.com/products/davinciresolve",
];

#[tauri::command]
fn open_external(url: String) -> Result<(), Value> {
    let allowed = EXTERNAL_SITES.iter().any(|site| url == *site || url.starts_with(&format!("{site}/")));
    if !allowed {
        return Err(serde_json::json!({ "error": { "code": "invalid_argument", "message": format!("not an allowed link: {url}") } }));
    }
    #[cfg(target_os = "macos")]
    let spawned = std::process::Command::new("open").arg(&url).spawn();
    #[cfg(target_os = "windows")]
    let spawned = std::process::Command::new("explorer").arg(&url).spawn();
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let spawned = std::process::Command::new("xdg-open").arg(&url).spawn();
    spawned.map_err(|e| serde_json::json!({ "error": { "code": "io", "message": e.to_string() } }))?;
    Ok(())
}

#[tauri::command]
fn confirm_action(state: State<'_, AppState>, id: String, approved: bool) -> Result<(), Value> {
    let id = uuid::Uuid::parse_str(&id)
        .map_err(|e| serde_json::json!({ "error": { "code": "invalid_argument", "message": e.to_string() } }))?;
    state.editor.resolve_confirmation(id, approved);
    Ok(())
}

#[tauri::command]
async fn setup_status(
    state: State<'_, AppState>,
) -> Result<roughcut_core::setup::SetupStatus, Value> {
    Ok(roughcut_core::setup::status(&state.editor).await)
}

// ---- The LOCAL / HOSTED switch -----------------------------------------
// Turning Hosted on and handling its API key are the user's actions in the
// app, kept out of the tool registry (ADR-0004): no MCP client or agent can
// reach them. Each is a thin wrapper over the Editor, which owns the state.

#[tauri::command]
async fn model_status(state: State<'_, AppState>) -> Result<roughcut_core::model::ModelStatus, Value> {
    state.editor.model_status().map_err(|e| e.to_json())
}

#[tauri::command]
async fn set_model_mode(
    state: State<'_, AppState>,
    mode: roughcut_core::model::ModelMode,
    consent: bool,
) -> Result<roughcut_core::model::ModelStatus, Value> {
    state.editor.set_model_mode(mode, consent).map_err(|e| e.to_json())
}

#[tauri::command]
async fn set_hosted_model(
    state: State<'_, AppState>,
    model: String,
) -> Result<roughcut_core::model::ModelStatus, Value> {
    state.editor.set_hosted_model(&model).map_err(|e| e.to_json())
}

#[tauri::command]
async fn set_hosted_key(
    state: State<'_, AppState>,
    key: Option<String>,
) -> Result<roughcut_core::model::HostedKeyResult, Value> {
    state.editor.set_hosted_key(key.as_deref()).await.map_err(|e| e.to_json())
}

#[tauri::command]
async fn check_hosted_key(state: State<'_, AppState>) -> Result<roughcut_core::model::KeyCheck, Value> {
    state.editor.check_hosted_key().await.map_err(|e| e.to_json())
}

#[tauri::command]
async fn ollama_pull_model(state: State<'_, AppState>, model: String) -> Result<(), Value> {
    roughcut_core::llm_runtime::ollama_pull(&state.editor.sink(), &model)
        .await
        .map_err(|e| e.to_json())
}

#[tauri::command]
async fn install_llama_server(state: State<'_, AppState>) -> Result<String, Value> {
    roughcut_core::llm_runtime::install_llama_server(&state.editor.sink())
        .await
        .map_err(|e| e.to_json())
}

#[tauri::command]
async fn download_gguf(state: State<'_, AppState>, url: String) -> Result<String, Value> {
    roughcut_core::llm_runtime::download_gguf(&state.editor.sink(), &url)
        .await
        .map_err(|e| e.to_json())
}

#[tauri::command]
async fn start_managed_llm(state: State<'_, AppState>, gguf_path: String) -> Result<String, Value> {
    roughcut_core::llm_runtime::start_managed(&state.editor, &gguf_path)
        .await
        .map_err(|e| e.to_json())
}

#[tauri::command]
fn install_resolve_plugin() -> Result<String, Value> {
    roughcut_core::resolve::install_plugin().map_err(|e| e.to_json())
}

#[tauri::command]
fn resolve_status() -> roughcut_core::resolve::ResolveStatus {
    roughcut_core::resolve::status()
}

/// Export the CURRENT cut as Resolve XML and push it into the running
/// Resolve instance. Never rejects: `{ok, timeline?, xml_path, error?}` so
/// the UI can fall back to revealing the XML for a manual drag.
#[tauri::command]
async fn send_to_resolve(state: State<'_, AppState>, project_id: String) -> Result<Value, Value> {
    let pid = uuid::Uuid::parse_str(&project_id)
        .map_err(|e| serde_json::json!({ "error": { "code": "invalid_argument", "message": e.to_string() } }))?;
    let xml_path = std::env::temp_dir()
        .join(format!("roughcut-{}.xml", &project_id[..8.min(project_id.len())]))
        .to_string_lossy()
        .into_owned();
    let xml_path = state
        .editor
        .export(pid, "resolve_xml", &xml_path)
        .await
        .map_err(|e| e.to_json())?;
    let outcome = match roughcut_core::resolve::send_timeline(&xml_path).await {
        Ok(timeline) => roughcut_core::resolve::SendToResolveOutcome {
            ok: true,
            timeline: Some(timeline),
            xml_path,
            error: None,
        },
        Err(e) => roughcut_core::resolve::SendToResolveOutcome {
            ok: false,
            timeline: None,
            xml_path,
            error: Some(e.to_string()),
        },
    };
    Ok(serde_json::to_value(outcome).unwrap_or_default())
}

/// Open an in-app terminal session. The MCP endpoint (when up) is injected
/// into the shell env so a one-click connect can wire Claude Code to it.
#[tauri::command]
fn pty_spawn(state: State<'_, AppState>, id: String, cols: u16, rows: u16) -> Result<(), Value> {
    let mcp = state.mcp.lock().unwrap().clone().map(|i| (i.url, i.token));
    roughcut_core::pty::spawn(state.editor.sink(), id, cols, rows, mcp).map_err(|e| e.to_json())
}

#[tauri::command]
fn pty_write(id: String, data: String) -> Result<(), Value> {
    roughcut_core::pty::write(&id, data.as_bytes()).map_err(|e| e.to_json())
}

#[tauri::command]
fn pty_resize(id: String, cols: u16, rows: u16) -> Result<(), Value> {
    roughcut_core::pty::resize(&id, cols, rows).map_err(|e| e.to_json())
}

#[tauri::command]
fn pty_kill(id: String) -> Result<(), Value> {
    roughcut_core::pty::kill(&id).map_err(|e| e.to_json())
}

#[tauri::command]
async fn download_whisper_model(
    state: State<'_, AppState>,
    tier: String,
) -> Result<String, Value> {
    roughcut_core::setup::download_whisper_model(&state.editor.sink(), &tier)
        .await
        .map_err(|e| e.to_json())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Must be first: a second launch focuses the existing window instead
        // of racing it for the MCP endpoint file.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let sink = std::sync::Arc::new(TauriSink(app.handle().clone()));
            let editor = Editor::bootstrap(sink)?;
            app.manage(AppState { editor: editor.clone(), mcp: Mutex::new(None) });
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match roughcut_core::mcp::server::start(editor).await {
                    Ok(info) => {
                        let state: State<'_, AppState> = handle.state();
                        *state.mcp.lock().unwrap() = Some(info.clone());
                        let _ = handle.emit("mcp-ready", serde_json::json!(info));
                    }
                    Err(e) => eprintln!("failed to start MCP server: {e}"),
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            call_tool,
            list_tools,
            mcp_endpoint_info,
            demo_mode,
            confirm_action,
            reveal_path,
            open_external,
            setup_status,
            model_status,
            set_model_mode,
            set_hosted_model,
            set_hosted_key,
            check_hosted_key,
            ollama_pull_model,
            install_llama_server,
            download_gguf,
            start_managed_llm,
            download_whisper_model,
            pty_spawn,
            pty_write,
            pty_resize,
            pty_kill,
            install_resolve_plugin,
            resolve_status,
            send_to_resolve
        ])
        .build(tauri::generate_context!())
        .expect("error while building RoughCut")
        .run(|_app, event| {
            if let tauri::RunEvent::Exit = event {
                roughcut_core::llm_runtime::stop_managed();
                roughcut_core::pty::kill_all();
            }
        });
}
