//! Tauri shell: registers the IPC commands (one generic `call_tool` plus a
//! couple of conveniences), forwards core events to the webview, and starts
//! the localhost MCP server.

use roughcut_core::events::EventSink;
use roughcut_core::mcp::server::EndpointInfo;
use roughcut_core::model::ActionSource;
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
    tools::dispatch(&state.editor, &name, &args, ActionSource::Ui)
        .await
        .map_err(|e| e.to_json())
}

#[tauri::command]
fn list_tools() -> Vec<tools::ToolDef> {
    tools::all_defs()
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

#[tauri::command]
async fn record_devices() -> Result<roughcut_core::adapters::record::CaptureDevices, Value> {
    roughcut_core::adapters::record::list_devices().await.map_err(|e| e.to_json())
}

#[tauri::command]
async fn record_start(
    state: State<'_, AppState>,
    camera: u32,
    microphone: u32,
) -> Result<String, Value> {
    roughcut_core::adapters::record::start_camera(&state.editor.sink(), camera, microphone)
        .await
        .map_err(|e| e.to_json())
}

#[tauri::command]
fn record_status() -> roughcut_core::adapters::record::RecordingStatus {
    roughcut_core::adapters::record::status()
}

#[tauri::command]
async fn record_stop() -> Result<String, Value> {
    roughcut_core::adapters::record::stop().await.map_err(|e| e.to_json())
}

#[tauri::command]
async fn record_pause() -> Result<roughcut_core::adapters::record::RecordingStatus, Value> {
    roughcut_core::adapters::record::pause().await.map_err(|e| e.to_json())
}

#[tauri::command]
async fn record_resume(
    state: State<'_, AppState>,
) -> Result<roughcut_core::adapters::record::RecordingStatus, Value> {
    roughcut_core::adapters::record::resume(&state.editor.sink()).await.map_err(|e| e.to_json())
}

#[tauri::command]
async fn list_recordings() -> Result<Vec<roughcut_core::adapters::record::RecordingFile>, Value> {
    roughcut_core::adapters::record::list_recordings().await.map_err(|e| e.to_json())
}

#[tauri::command]
async fn combine_recordings(paths: Vec<String>) -> Result<String, Value> {
    roughcut_core::adapters::record::combine_recordings(&paths).await.map_err(|e| e.to_json())
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
            setup_status,
            ollama_pull_model,
            install_llama_server,
            download_gguf,
            start_managed_llm,
            download_whisper_model,
            install_resolve_plugin,
            resolve_status,
            send_to_resolve,
            record_devices,
            record_start,
            record_status,
            record_stop,
            record_pause,
            record_resume,
            list_recordings,
            combine_recordings
        ])
        .build(tauri::generate_context!())
        .expect("error while building RoughCut")
        .run(|_app, event| {
            if let tauri::RunEvent::Exit = event {
                roughcut_core::llm_runtime::stop_managed();
                roughcut_core::adapters::record::abort_on_exit();
            }
        });
}
