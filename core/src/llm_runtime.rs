//! Managed local LLM runtime (spec M4/M7): detect Ollama first; otherwise
//! install and run a llama.cpp `llama-server` sidecar. Everything is
//! user-triggered, with progress events — the same lean-app/download-on-
//! demand story as the whisper models.
//!
//! macOS-focused per project direction; the release-asset naming used here
//! (`llama-<tag>-bin-macos-{arm64,x64}.tar.gz`) matches ggml-org/llama.cpp.

use crate::engine::Editor;
use crate::error::{CoreError, Result};
use crate::events::{send, CoreEvent, ProgressTask, SharedSink};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
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

// Status questions (reported through `setup::status()` — the ONE
// provisioning status the UI fetches).

pub fn ollama_cli_available() -> bool {
    std::process::Command::new("which")
        .arg("ollama")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn llama_server_installed() -> bool {
    llama_server_path().is_file()
}

pub fn managed_running() -> bool {
    // try_wait notices a child that crashed or was killed externally —
    // is_some() alone reported a corpse as "running" forever.
    let mut guard = managed_child().lock().unwrap();
    match guard.as_mut() {
        None => false,
        Some(child) => match child.try_wait() {
            Ok(Some(_exit)) => {
                *guard = None;
                false
            }
            Ok(None) => true,
            Err(_) => false,
        },
    }
}

/// GGUF files available in the models dir (for the sidecar path).
pub fn list_ggufs() -> Vec<String> {
    std::fs::read_dir(crate::adapters::transcribe::models_dir())
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map(|x| x == "gguf").unwrap_or(false))
                .map(|e| e.path().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
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
    let model2 = model.to_string();
    let progress_task = child.stderr.take().map(|stderr| {
        crate::progress::stream_progress(
            stderr,
            sink.clone(),
            ProgressTask::ModelPull,
            None,
            crate::progress::Delimiter::CarriageReturn,
            move |text| {
                let i = text.rfind('%')?;
                let p: u8 = text[..i].split_whitespace().last()?.parse().ok()?;
                Some((f64::from(p) / 100.0, format!("pulling {model2}: {p}%")))
            },
        )
    });
    let status = child
        .wait()
        .await
        .map_err(|e| CoreError::Other(format!("ollama pull: {e}")))?;
    if let Some(t) = progress_task {
        let _ = t.await;
    }
    if !status.success() {
        return Err(CoreError::Other(format!("ollama pull {model} failed")));
    }
    send(&sink, CoreEvent::progress(ProgressTask::ModelPull, None, 1.0, format!("{model} ready")));
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
    send(&sink, CoreEvent::progress(ProgressTask::RuntimeInstall, None, 0.0, "resolving latest llama.cpp release"));
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
    send(&sink, CoreEvent::progress(ProgressTask::RuntimeInstall, None, 0.1, "downloading llama-server"));
    crate::download::download_to(
        sink,
        url,
        &archive,
        crate::download::DownloadOpts {
            task: ProgressTask::RuntimeInstall,
            sha256: None,
            approx_bytes: None,
        },
    )
    .await?;

    send(&sink, CoreEvent::progress(ProgressTask::RuntimeInstall, None, 0.7, "extracting"));
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
    send(&sink, CoreEvent::progress(ProgressTask::RuntimeInstall, None, 1.0, "llama-server installed"));
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
    crate::download::download_to(
        sink,
        url,
        &dest,
        crate::download::DownloadOpts {
            task: ProgressTask::ModelDownload,
            sha256: None,
            approx_bytes: None,
        },
    )
    .await?;
    send(&sink, CoreEvent::progress(ProgressTask::ModelDownload, None, 1.0, "download complete"));
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
            send(&sink, CoreEvent::progress(ProgressTask::RuntimeInstall, None, 1.0, "local model server running"));
            return Ok(endpoint);
        }
        if i % 10 == 0 {
            send(
                &sink,
                CoreEvent::progress(ProgressTask::RuntimeInstall, None, 0.9, "loading model into the local server…"),
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
