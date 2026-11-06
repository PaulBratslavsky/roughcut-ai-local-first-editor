//! First-run setup: report what the local toolchain is missing and download
//! whisper models into `<data dir>/models/` with progress events.
//!
//! The one deliberate egress in setup is the model download from Hugging
//! Face — user-triggered, never automatic, consistent with the local-first
//! principle (weights are fetched by the user at runtime, never bundled).
//! Downloads stream to a `.part` file, are sha256-verified against pinned
//! upstream digests, and only then renamed into place.

use crate::adapters::transcribe::models_dir;
use crate::adapters::OllamaEmbedder;
use crate::capabilities;
use crate::engine::Editor;
use crate::error::{CoreError, Result};
use crate::events::SharedSink;
use serde::Serialize;

#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct TierStatus {
    pub id: &'static str,
    pub file: &'static str,
    #[cfg_attr(feature = "ts-bindings", ts(type = "number"))]
    pub approx_mb: u64,
    pub downloaded: bool,
    /// Best fit for this machine's RAM.
    pub recommended: bool,
}

/// LLM runtime details: which runtime is around and what it has loaded.
/// Lives INSIDE [`SetupStatus`] — the UI fetches one shape, the core probes
/// the inference server once.
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeStatus {
    /// `ollama` CLI on PATH (preferred runtime).
    pub ollama_cli: bool,
    /// Model ids the inference server reports.
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

#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct SetupStatus {
    /// ffmpeg + ffprobe both resolved.
    pub ffmpeg: bool,
    pub ffmpeg_path: Option<String>,
    /// Path of the whisper model that will be used, if any is present.
    pub whisper_model: Option<String>,
    /// True when the in-process whisper engine is compiled in (no
    /// whisper-cli binary needed — only a model file).
    pub whisper_native: bool,
    /// `whisper-cli` binary available (the non-native fallback engine).
    pub whisper_cli: bool,
    /// An STT engine AND a model are both usable right now.
    pub transcription_ready: bool,
    pub models_dir: String,
    pub tiers: Vec<TierStatus>,
    /// Total system memory, for sizing guidance in the UI.
    pub ram_gb: Option<f64>,
    /// Chat model (agent loop) — OpenAI-compatible server reachable?
    pub inference_reachable: bool,
    pub inference_endpoint: String,
    pub inference_model: String,
    /// Embedding model (semantic search) endpoint reachable (same server).
    pub embedding_model: String,
    pub runtime: RuntimeStatus,
    /// DaVinci Resolve hand-off (app detected / plugin installed).
    pub resolve: crate::resolve::ResolveStatus,
    /// Import from a link: yt-dlp and Deno, installed by the user here.
    pub link_import: LinkImportStatus,
    pub demo: bool,
    /// Why demo mode is active (None when it isn't).
    pub demo_reason: Option<String>,
}

/// Snapshot of `capabilities::probe()` + ONE live server check for the setup
/// screen — resolved at call time, so it's truthful even after mid-session
/// installs/downloads. This is the single provisioning status: reachability
/// and the model list come from the same `/models` probe, so they can never
/// contradict each other.
pub async fn status(editor: &Editor) -> SetupStatus {
    let caps = capabilities::probe();
    let demo = editor.demo_mode();
    let ram_gb = capabilities::system_ram_gb();
    let prefs = editor.get_preferences().unwrap_or_default();
    let (inference_reachable, models) = probe_inference(&prefs.inference_endpoint).await;
    let has = |tag: &str| {
        let base = tag.split(':').next().unwrap_or(tag);
        models.iter().any(|m| m == tag || m.starts_with(base))
    };
    let runtime = RuntimeStatus {
        ollama_cli: crate::llm_runtime::ollama_cli_available(),
        chat_model_present: has(&prefs.inference_model),
        embedding_model_present: has(&prefs.embedding_model),
        models,
        llama_server_installed: crate::llm_runtime::llama_server_installed(),
        managed_running: crate::llm_runtime::managed_running(),
        ggufs: crate::llm_runtime::list_ggufs(),
    };
    let recommended_id = recommended_tier(ram_gb);
    let tiers = MODEL_TIERS
        .iter()
        .map(|t| TierStatus {
            id: t.id,
            file: t.file,
            approx_mb: t.approx_mb,
            downloaded: models_dir().join(t.file).is_file(),
            recommended: t.id == recommended_id,
        })
        .collect();
    let link_import = link_import_status(&prefs);
    SetupStatus {
        ffmpeg: caps.media_ready(),
        ffmpeg_path: caps.ffmpeg.as_ref().map(|p| p.to_string_lossy().into_owned()),
        whisper_model: caps.whisper_model.as_ref().map(|p| p.to_string_lossy().into_owned()),
        whisper_native: caps.whisper_native,
        whisper_cli: caps.whisper_cli,
        transcription_ready: caps.transcription_ready(),
        models_dir: models_dir().to_string_lossy().into_owned(),
        tiers,
        ram_gb,
        inference_reachable,
        inference_endpoint: prefs.inference_endpoint,
        inference_model: prefs.inference_model,
        embedding_model: prefs.embedding_model,
        runtime,
        resolve: crate::resolve::status(),
        link_import,
        demo,
        demo_reason: if demo {
            caps.demo_reason().or_else(|| Some("fixture adapters forced (test instance)".into()))
        } else {
            None
        },
    }
}

/// The one inference-server probe: GET `{endpoint}/models` with a short
/// timeout. Reachability and the model list come from the same response —
/// the old split (setup pinged health, llm_runtime listed models) could
/// report contradictory state when one timed out.
async fn probe_inference(endpoint: &str) -> (bool, Vec<String>) {
    let resp = reqwest::Client::new()
        .get(format!("{}/models", endpoint.trim_end_matches('/')))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await;
    match resp {
        Ok(resp) if resp.status().is_success() => {
            let models = resp
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| {
                    v["data"].as_array().map(|a| {
                        a.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect()
                    })
                })
                .unwrap_or_default();
            (true, models)
        }
        _ => (false, vec![]),
    }
}

/// The "accurate" tier needs ~1 GB at runtime — fine on 8 GB+ machines.
fn recommended_tier(ram_gb: Option<f64>) -> &'static str {
    match ram_gb {
        Some(gb) if gb < 8.0 => "compact",
        _ => "accurate",
    }
}

/// Quick reachability check for the embeddings model (same local server).
pub async fn embeddings_reachable(editor: &Editor) -> bool {
    match editor.get_preferences() {
        Ok(p) => OllamaEmbedder::new(&p.inference_endpoint, &p.embedding_model).reachable().await,
        Err(_) => false,
    }
}

pub struct ModelTier {
    pub id: &'static str,
    pub file: &'static str,
    pub approx_mb: u64,
    /// Pinned upstream sha256 (Hugging Face LFS digest).
    pub sha256: &'static str,
}

/// Downloadable tiers, best first. "accurate" is the default recommendation
/// (large-v3-turbo quantized: near-large quality, ~1 GB RAM at runtime).
pub const MODEL_TIERS: &[ModelTier] = &[
    ModelTier {
        id: "accurate",
        file: "ggml-large-v3-turbo-q5_0.bin",
        approx_mb: 547,
        sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
    },
    ModelTier {
        id: "compact",
        file: "ggml-small-q5_1.bin",
        approx_mb: 190,
        sha256: "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb",
    },
];

const MODEL_BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// Stream a whisper model into the models dir, emitting `progress` events
/// (task `model_download`). Streams to a `.part` file, verifies the sha256
/// against the pinned digest, and only then renames into place — an
/// interrupted or corrupted download never half-installs a model.
pub async fn download_whisper_model(sink: &SharedSink, tier: &str) -> Result<String> {
    let tier = MODEL_TIERS
        .iter()
        .find(|t| t.id == tier)
        .ok_or_else(|| CoreError::InvalidArg(format!("unknown model tier '{tier}'")))?;
    let dir = models_dir();
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(tier.file);
    if dest.is_file() {
        return Ok(dest.to_string_lossy().into_owned());
    }
    let url = format!("{MODEL_BASE_URL}/{}", tier.file);
    crate::events::send(
        sink,
        crate::events::CoreEvent::progress(
            crate::events::ProgressTask::ModelDownload,
            None,
            0.0,
            "starting download",
        ),
    );
    crate::download::download_to(
        sink,
        &url,
        &dest,
        crate::download::DownloadOpts {
            task: crate::events::ProgressTask::ModelDownload,
            sha256: Some(tier.sha256),
            approx_bytes: Some(tier.approx_mb * 1024 * 1024),
        },
    )
    .await?;
    crate::events::send(
        sink,
        crate::events::CoreEvent::progress(
            crate::events::ProgressTask::ModelDownload,
            None,
            1.0,
            "download complete (verified)",
        ),
    );
    Ok(dest.to_string_lossy().into_owned())
}

// ------------------------------------------------------------ link import

/// "Import from a link" in Setup: the app's own yt-dlp and Deno
/// (docs/designs/url-import-and-cli.md, ADR-0009). Installing them is the
/// user's action that turns link import on; Remove turns it off.
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct LinkImportStatus {
    /// The app's yt-dlp is in `<data dir>/bin/`: link import is on.
    pub installed: bool,
    pub ytdlp: Option<String>,
    pub deno: Option<String>,
    /// yt-dlp's version (a date) is more than 60 days old: Update.
    pub outdated: bool,
    /// Where link imports are saved.
    pub download_dir: String,
    /// There are builds of yt-dlp and Deno for this machine.
    pub supported: bool,
}

/// Read from files only: status never runs yt-dlp or Deno.
pub fn link_import_status(prefs: &crate::model::Preferences) -> LinkImportStatus {
    use crate::adapters::fetch;
    let (ytdlp, deno) = fetch::recorded_versions();
    let outdated = ytdlp.as_deref().map(ytdlp_is_old).unwrap_or(false);
    LinkImportStatus {
        installed: fetch::ytdlp_path().is_file(),
        ytdlp,
        deno,
        outdated,
        download_dir: crate::engine::download_folder_path(prefs).to_string_lossy().into_owned(),
        supported: link_import_assets().is_some(),
    }
}

/// yt-dlp versions are release dates (`2026.10.26`, sometimes with a `.N`
/// suffix). Older than 60 days means YouTube has likely changed since.
fn ytdlp_is_old(version: &str) -> bool {
    let mut parts = version.split('.').map(|p| p.parse::<i32>().ok());
    let (Some(Some(y)), Some(Some(m)), Some(Some(d))) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let Some(date) = chrono::NaiveDate::from_ymd_opt(y, m as u32, d as u32) else { return false };
    (chrono::Utc::now().date_naive() - date).num_days() > 60
}

/// The release assets for this machine: yt-dlp's standalone build and
/// Deno's zip. `None` where neither project publishes one.
fn link_import_assets() -> Option<(&'static str, &'static str)> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    match (os, arch) {
        ("macos", "aarch64") => Some(("yt-dlp_macos", "deno-aarch64-apple-darwin.zip")),
        ("macos", "x86_64") => Some(("yt-dlp_macos", "deno-x86_64-apple-darwin.zip")),
        ("linux", "x86_64") => Some(("yt-dlp_linux", "deno-x86_64-unknown-linux-gnu.zip")),
        ("linux", "aarch64") => Some(("yt-dlp_linux_aarch64", "deno-aarch64-unknown-linux-gnu.zip")),
        ("windows", "x86_64") => Some(("yt-dlp.exe", "deno-x86_64-pc-windows-msvc.zip")),
        _ => None,
    }
}

const YTDLP_RELEASE: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download";
const DENO_RELEASE: &str = "https://github.com/denoland/deno/releases/latest/download";

/// The hash for `name` in a checksum file (`<hex>  <name>` lines, or a file
/// holding only the hash).
fn hash_for(sums: &str, name: &str) -> Option<String> {
    let is_hash = |t: &str| t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit());
    for line in sums.lines() {
        let mut parts = line.split_whitespace();
        let (Some(hash), rest) = (parts.next(), parts.next()) else { continue };
        let file = rest.map(|r| r.trim_start_matches('*'));
        if is_hash(hash) && (file.is_none() || file == Some(name)) {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

async fn fetch_text(url: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .user_agent("roughcut")
        .build()
        .map_err(|e| CoreError::Unavailable(format!("download client: {e}")))?;
    let resp = client.get(url).send().await.map_err(|e| CoreError::Unavailable(format!("download: {e}")))?;
    if !resp.status().is_success() {
        return Err(CoreError::Unavailable(format!("download {url}: HTTP {}", resp.status())));
    }
    resp.text().await.map_err(|e| CoreError::Unavailable(format!("download: {e}")))
}

/// Install, or update, yt-dlp and Deno into `<data dir>/bin/` from their
/// latest GitHub releases, each checked against its published SHA-256.
/// All or nothing: both are staged, checked and run once before either
/// replaces the copy in use, so a failure leaves the previous state.
/// User-triggered from Setup only (ADR-0004): never a registry tool.
pub async fn install_link_import(editor: &Editor) -> Result<LinkImportStatus> {
    use crate::adapters::fetch;
    let sink = &editor.sink();
    let (ytdlp_asset, deno_asset) = link_import_assets()
        .ok_or_else(|| CoreError::Unavailable("yt-dlp and Deno have no builds for this machine".into()))?;
    let bin = fetch::bin_dir();
    let staging = bin.join("link-import-staging");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let result = stage_link_import(sink, ytdlp_asset, deno_asset, &staging).await;
    let outcome = match result {
        Ok((ytdlp_version, deno_version)) => {
            let names = |n: &str| if cfg!(windows) { format!("{n}.exe") } else { n.to_string() };
            std::fs::rename(staging.join(names("yt-dlp")), fetch::bin_dir().join(names("yt-dlp")))?;
            std::fs::rename(staging.join(names("deno")), fetch::bin_dir().join(names("deno")))?;
            let record = serde_json::json!({
                "ytdlp": ytdlp_version,
                "deno": deno_version,
                "installed_at": chrono::Utc::now().to_rfc3339(),
            });
            std::fs::write(fetch::versions_file(), serde_json::to_string_pretty(&record)?)?;
            crate::events::send(
                sink,
                crate::events::CoreEvent::progress(
                    crate::events::ProgressTask::RuntimeInstall,
                    None,
                    1.0,
                    "link import is on".to_string(),
                ),
            );
            Ok(link_import_status(&editor.get_preferences().unwrap_or_default()))
        }
        Err(e) => Err(e),
    };
    let _ = std::fs::remove_dir_all(&staging);
    outcome
}

/// Download, check, unpack and run both into `staging`; returns their
/// versions.
async fn stage_link_import(
    sink: &SharedSink,
    ytdlp_asset: &str,
    deno_asset: &str,
    staging: &std::path::Path,
) -> Result<(Option<String>, Option<String>)> {
    use crate::download::{download_to, DownloadOpts};
    use crate::events::{send, CoreEvent, ProgressTask};
    let step = |fraction: f64, message: &str| {
        send(sink, CoreEvent::progress(ProgressTask::RuntimeInstall, None, fraction, message.to_string()));
    };
    let names = |n: &str| if cfg!(windows) { format!("{n}.exe") } else { n.to_string() };
    let ytdlp = staging.join(names("yt-dlp"));
    let deno = staging.join(names("deno"));

    step(0.0, "checking yt-dlp's latest release");
    let sums = fetch_text(&format!("{YTDLP_RELEASE}/SHA2-256SUMS")).await?;
    let hash = hash_for(&sums, ytdlp_asset)
        .ok_or_else(|| CoreError::Unavailable(format!("no checksum for {ytdlp_asset} in yt-dlp's release")))?;
    download_to(
        sink,
        &format!("{YTDLP_RELEASE}/{ytdlp_asset}"),
        &ytdlp,
        DownloadOpts { task: ProgressTask::RuntimeInstall, sha256: Some(&hash), approx_bytes: Some(36_000_000) },
    )
    .await
    .map_err(|e| checksum_message(e, "yt-dlp"))?;

    step(0.5, "checking Deno's latest release");
    let sums = fetch_text(&format!("{DENO_RELEASE}/{deno_asset}.sha256sum")).await?;
    let hash = hash_for(&sums, deno_asset)
        .ok_or_else(|| CoreError::Unavailable(format!("no checksum for {deno_asset} in Deno's release")))?;
    let zip = staging.join("deno.zip");
    download_to(
        sink,
        &format!("{DENO_RELEASE}/{deno_asset}"),
        &zip,
        DownloadOpts { task: ProgressTask::RuntimeInstall, sha256: Some(&hash), approx_bytes: Some(45_000_000) },
    )
    .await
    .map_err(|e| checksum_message(e, "Deno"))?;
    step(0.9, "unpacking Deno");
    // bsdtar (macOS, Windows) reads zip files; Linux uses unzip.
    let unpacked = if cfg!(target_os = "linux") {
        tokio::process::Command::new("unzip").args(["-o", "-q"]).arg(&zip).arg("-d").arg(staging).status().await
    } else {
        tokio::process::Command::new("tar").arg("-xf").arg(&zip).arg("-C").arg(staging).status().await
    };
    let _ = std::fs::remove_file(&zip);
    match unpacked {
        Ok(s) if s.success() && deno.is_file() => {}
        Ok(s) => return Err(CoreError::Other(format!("unpacking Deno failed ({s})"))),
        Err(e) => return Err(CoreError::Other(format!("unpacking Deno: {e} (on Linux, install unzip)"))),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&ytdlp, &deno] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
        }
    }

    // Run each once, now, so status can show versions without running them,
    // and so a binary that can't start never replaces a working one.
    let version = |path: std::path::PathBuf, what: &'static str| async move {
        let out = tokio::process::Command::new(&path)
            .arg("--version")
            .output()
            .await
            .map_err(|e| CoreError::Unavailable(format!("{what} won't start: {e}")))?;
        if !out.status.success() {
            return Err(CoreError::Unavailable(format!("{what} won't start ({})", out.status)));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let first = text.lines().next().unwrap_or("").trim().to_string();
        Ok(first
            .strip_prefix("deno ")
            .map(|v| v.split_whitespace().next().unwrap_or(v).to_string())
            .or(Some(first).filter(|v| !v.is_empty())))
    };
    let ytdlp_version = version(ytdlp, "yt-dlp").await?;
    let deno_version = version(deno, "Deno").await?;
    Ok((ytdlp_version, deno_version))
}

fn checksum_message(e: CoreError, what: &str) -> CoreError {
    let text = e.to_string();
    if text.to_ascii_lowercase().contains("sha256") || text.to_ascii_lowercase().contains("checksum") {
        CoreError::Unavailable(format!("checksum mismatch for {what}: not installed"))
    } else {
        e
    }
}

/// Remove yt-dlp and Deno: link import is off again.
pub fn remove_link_import() -> Result<()> {
    use crate::adapters::fetch;
    for path in [fetch::ytdlp_path(), fetch::deno_path(), fetch::versions_file()] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod link_import_tests {
    use super::*;

    #[test]
    fn checksums_are_read_from_either_file_shape() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let sums = format!("{a}  yt-dlp\n{b}  yt-dlp_macos\n");
        assert_eq!(hash_for(&sums, "yt-dlp_macos").unwrap(), "b".repeat(64));
        assert!(hash_for(&sums, "yt-dlp_linux").is_none());
        assert_eq!(hash_for(&format!("{a}\n"), "deno-x.zip").unwrap(), a, "a bare hash");
        assert_eq!(hash_for(&format!("{a} *deno-x.zip\n"), "deno-x.zip").unwrap(), a, "binary-mode star");
        assert!(hash_for("not a hash  yt-dlp", "yt-dlp").is_none());
    }

    #[test]
    fn an_old_ytdlp_version_date_is_flagged() {
        assert!(ytdlp_is_old("2020.01.01"));
        let today = chrono::Utc::now().date_naive().format("%Y.%m.%d").to_string();
        assert!(!ytdlp_is_old(&today));
        assert!(!ytdlp_is_old(&format!("{today}.1")));
        assert!(!ytdlp_is_old("nightly"), "an unknown format isn't flagged");
    }
}
