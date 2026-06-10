//! Managed local LLM runtime (spec M4/M7): detect Ollama first; otherwise
//! install and run a llama.cpp `llama-server` sidecar. Everything is
//! user-triggered, with progress events — the same lean-app/download-on-
//! demand story as the whisper models.
//!
//! macOS-focused per project direction; the release-asset naming used here
//! (`llama-<tag>-bin-macos-{arm64,x64}.tar.gz`) matches ggml-org/llama.cpp.

use crate::engine::Editor;
use crate::error::{CoreError, Result};
use crate::events::{send, CoreEvent, SharedSink};
use serde::Serialize;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use tokio::io::AsyncBufReadExt;
use tokio::process::{Child, Command};

const MANAGED_PORT: u16 = 47861;

fn managed_child() -> &'static Mutex<Option<Child>> {
    static CHILD: OnceLock<Mutex<Option<Child>>> = OnceLock::new();
    CHILD.get_or_init(|| Mutex::new(None))
}

fn bin_dir() -> PathBuf {
    crate::store::data_dir().join("bin")
}

fn llama_server_path() -> PathBuf {
    bin_dir().join("llama-server")
}

#[derive(Debug, Clone, Serialize)]
pub struct LlmRuntimeStatus {
    /// `ollama` CLI on PATH (preferred runtime).
    pub ollama_cli: bool,
    /// The configured OpenAI-compatible server answers.
    pub server_reachable: bool,
    /// Model ids the server reports.
    pub models: Vec<String>,
    pub chat_model_present: bool,
    pub embedding_model_present: bool,
    /// llama-server sidecar binary installed in <data dir>/bin.
    pub llama_server_installed: bool,
    /// The sidecar this app launched is currently running.
    pub managed_running: bool,
    /// GGUF files available in the models dir (for the sidecar path).
    pub ggufs: Vec<String>,
}

fn ollama_cli_available() -> bool {
    std::process::Command::new("which")
        .arg("ollama")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub async fn status(editor: &Editor) -> Result<LlmRuntimeStatus> {
    let prefs = editor.get_preferences()?;
    let http = reqwest::Client::new();
    let models: Vec<String> = match http
        .get(format!("{}/models", prefs.inference_endpoint.trim_end_matches('/')))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| {
                v["data"].as_array().map(|a| {
                    a.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect()
                })
            })
            .unwrap_or_default(),
        _ => vec![],
    };
    let server_reachable = !models.is_empty()
        || crate::adapters::OllamaEmbedder::new(&prefs.inference_endpoint, "x").reachable().await;
    let has = |tag: &str| {
        let base = tag.split(':').next().unwrap_or(tag);
        models.iter().any(|m| m == tag || m.starts_with(base))
    };
    let ggufs = std::fs::read_dir(crate::adapters::transcribe::models_dir())
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map(|x| x == "gguf").unwrap_or(false))
                .map(|e| e.path().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    Ok(LlmRuntimeStatus {
        ollama_cli: ollama_cli_available(),
        server_reachable,
        chat_model_present: has(&prefs.inference_model),
        embedding_model_present: has(&prefs.embedding_model),
        models,
        llama_server_installed: llama_server_path().is_file(),
        managed_running: managed_child().lock().unwrap().is_some(),
        ggufs,
    })
}

/// `ollama pull <model>` with progress events (task `model_pull`).
pub async fn ollama_pull(sink: &SharedSink, model: &str) -> Result<()> {
    if !ollama_cli_available() {
        return Err(CoreError::Unavailable("ollama CLI not found".into()));
    }
    let mut child = Command::new("ollama")
        .args(["pull", model])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| CoreError::Unavailable(format!("ollama pull: {e}")))?;
    let stderr = child.stderr.take();
    let sink2 = sink.clone();
    let model2 = model.to_string();
    let progress_task = tokio::spawn(async move {
        if let Some(stderr) = stderr {
            let mut reader = tokio::io::BufReader::new(stderr).split(b'\r');
            let mut last = 0u8;
            while let Ok(Some(line)) = reader.next_segment().await {
                let text = String::from_utf8_lossy(&line);
                if let Some(p) = text.rfind('%').and_then(|i| {
                    text[..i].split_whitespace().last().and_then(|n| n.parse::<u8>().ok())
                }) {
                    if p != last {
                        last = p;
                        send(
                            &sink2,
                            CoreEvent::progress(
                                "model_pull",
                                None,
                                f64::from(p) / 100.0,
                                format!("pulling {model2}: {p}%"),
                            ),
                        );
                    }
                }
            }
        }
    });
    let status = child
        .wait()
        .await
        .map_err(|e| CoreError::Other(format!("ollama pull: {e}")))?;
    let _ = progress_task.await;
    if !status.success() {
        return Err(CoreError::Other(format!("ollama pull {model} failed")));
    }
    send(&sink, CoreEvent::progress("model_pull", None, 1.0, format!("{model} ready")));
    Ok(())
}

/// Download + install the llama.cpp `llama-server` sidecar binary from the
/// latest GitHub release (macOS asset). User-triggered.
pub async fn install_llama_server(sink: &SharedSink) -> Result<String> {
    if llama_server_path().is_file() {
        return Ok(llama_server_path().to_string_lossy().into_owned());
    }
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };
    let http = reqwest::Client::builder().user_agent("roughcut").build().unwrap();
    send(&sink, CoreEvent::progress("runtime_install", None, 0.0, "resolving latest llama.cpp release"));
    let release: serde_json::Value = http
        .get("https://api.github.com/repos/ggml-org/llama.cpp/releases/latest")
        .send()
        .await
        .map_err(|e| CoreError::Unavailable(format!("release lookup: {e}")))?
        .json()
        .await
        .map_err(|e| CoreError::Unavailable(format!("release lookup: {e}")))?;
    let needle = format!("macos-{arch}.tar.gz");
    let asset = release["assets"]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["name"].as_str().unwrap_or("").ends_with(&needle)))
        .ok_or_else(|| CoreError::Unavailable(format!("no {needle} asset in latest release")))?;
    let url = asset["browser_download_url"]
        .as_str()
        .ok_or_else(|| CoreError::Other("asset url missing".into()))?;

    std::fs::create_dir_all(bin_dir())?;
    let archive = bin_dir().join("llama-server.tar.gz");
    send(&sink, CoreEvent::progress("runtime_install", None, 0.1, "downloading llama-server"));
    let bytes = http
        .get(url)
        .send()
        .await
        .map_err(|e| CoreError::Unavailable(format!("download: {e}")))?
        .bytes()
        .await
        .map_err(|e| CoreError::Unavailable(format!("download: {e}")))?;
    std::fs::write(&archive, &bytes)?;

    send(&sink, CoreEvent::progress("runtime_install", None, 0.7, "extracting"));
    let extract_dir = bin_dir().join("llama-extract");
    let _ = std::fs::remove_dir_all(&extract_dir);
    std::fs::create_dir_all(&extract_dir)?;
    let out = Command::new("tar")
        .args(["-xzf", &archive.to_string_lossy(), "-C", &extract_dir.to_string_lossy()])
        .output()
        .await
        .map_err(|e| CoreError::Other(format!("tar: {e}")))?;
    if !out.status.success() {
        return Err(CoreError::Other(format!(
            "tar failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    // Locate the llama-server binary inside the archive and move the whole
    // bin dir contents next to it (dylibs live alongside).
    let mut found: Option<PathBuf> = None;
    for entry in walk(&extract_dir) {
        if entry.file_name().map(|n| n == "llama-server").unwrap_or(false) {
            found = Some(entry);
            break;
        }
    }
    let server = found.ok_or_else(|| CoreError::Other("llama-server not in archive".into()))?;
    let source_dir = server.parent().unwrap().to_path_buf();
    for entry in std::fs::read_dir(&source_dir)? {
        let entry = entry?;
        let dest = bin_dir().join(entry.file_name());
        let _ = std::fs::rename(entry.path(), dest);
    }
    let _ = std::fs::remove_dir_all(&extract_dir);
    let _ = std::fs::remove_file(&archive);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(llama_server_path(), std::fs::Permissions::from_mode(0o755))?;
    }
    send(&sink, CoreEvent::progress("runtime_install", None, 1.0, "llama-server installed"));
    Ok(llama_server_path().to_string_lossy().into_owned())
}

fn walk(dir: &PathBuf) -> Vec<PathBuf> {
    let mut out = vec![];
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

/// Download a GGUF chat model from a user-supplied URL into the models dir.
pub async fn download_gguf(sink: &SharedSink, url: &str) -> Result<String> {
    let name = url.split('/').next_back().unwrap_or("model.gguf").split('?').next().unwrap();
    if !name.ends_with(".gguf") {
        return Err(CoreError::InvalidArg("URL must point at a .gguf file".into()));
    }
    let dir = crate::adapters::transcribe::models_dir();
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(name);
    if dest.is_file() {
        return Ok(dest.to_string_lossy().into_owned());
    }
    let resp = reqwest::get(url)
        .await
        .map_err(|e| CoreError::Unavailable(format!("gguf download: {e}")))?;
    if !resp.status().is_success() {
        return Err(CoreError::Unavailable(format!("gguf download: HTTP {}", resp.status())));
    }
    let total = resp.content_length().unwrap_or(0);
    let part = dir.join(format!("{name}.part"));
    let mut file = std::fs::File::create(&part)?;
    let mut written = 0u64;
    let mut resp = resp;
    while let Some(chunk) =
        resp.chunk().await.map_err(|e| CoreError::Unavailable(format!("gguf download: {e}")))?
    {
        use std::io::Write;
        file.write_all(&chunk)?;
        written += chunk.len() as u64;
        if total > 0 {
            send(
                &sink,
                CoreEvent::progress(
                    "model_download",
                    None,
                    (written as f64 / total as f64).min(0.99),
                    format!("{} MB / {} MB", written / 1048576, total / 1048576),
                ),
            );
        }
    }
    drop(file);
    std::fs::rename(&part, &dest)?;
    send(&sink, CoreEvent::progress("model_download", None, 1.0, "download complete"));
    Ok(dest.to_string_lossy().into_owned())
}

/// Start the managed sidecar on a fixed local port and point preferences at
/// it. `--jinja` per the Gemma tool-calling guidance in docs/04.
pub async fn start_managed(editor: &Editor, gguf_path: &str) -> Result<String> {
    if !llama_server_path().is_file() {
        return Err(CoreError::Unavailable("llama-server not installed".into()));
    }
    if !std::path::Path::new(gguf_path).is_file() {
        return Err(CoreError::NotFound(format!("model file {gguf_path}")));
    }
    stop_managed();
    let child = Command::new(llama_server_path())
        .args([
            "-m", gguf_path,
            "--port", &MANAGED_PORT.to_string(),
            "--host", "127.0.0.1",
            "--jinja",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| CoreError::Other(format!("llama-server spawn: {e}")))?;
    *managed_child().lock().unwrap() = Some(child);

    // Wait for health (model load can take a while on big GGUFs).
    let endpoint = format!("http://127.0.0.1:{MANAGED_PORT}/v1");
    let http = reqwest::Client::new();
    let sink = editor.sink();
    for i in 0..120 {
        if http
            .get(format!("{endpoint}/models"))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            let mut prefs = editor.get_preferences()?;
            prefs.inference_endpoint = endpoint.clone();
            editor.set_preferences(prefs)?;
            send(&sink, CoreEvent::progress("runtime_install", None, 1.0, "local model server running"));
            return Ok(endpoint);
        }
        if i % 10 == 0 {
            send(
                &sink,
                CoreEvent::progress("runtime_install", None, 0.9, "loading model into the local server…"),
            );
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    stop_managed();
    Err(CoreError::Other("llama-server did not become healthy within 4 minutes".into()))
}

pub fn stop_managed() {
    if let Some(mut child) = managed_child().lock().unwrap().take() {
        let _ = child.start_kill();
    }
}
