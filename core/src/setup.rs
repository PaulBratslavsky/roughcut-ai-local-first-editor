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
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize)]
pub struct TierStatus {
    pub id: &'static str,
    pub file: &'static str,
    pub approx_mb: u64,
    pub downloaded: bool,
    /// Best fit for this machine's RAM.
    pub recommended: bool,
}

/// LLM runtime details: which runtime is around and what it has loaded.
/// Lives INSIDE [`SetupStatus`] — the UI fetches one shape, the core probes
/// the inference server once.
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
    let emit = |fraction: f64, message: &str| {
        crate::events::send(
            sink,
            crate::events::CoreEvent::progress(crate::events::ProgressTask::ModelDownload, None, fraction, message),
        );
    };
    emit(0.0, "starting download");

    let resp = reqwest::get(&url)
        .await
        .map_err(|e| CoreError::Unavailable(format!("model download: {e}")))?;
    if !resp.status().is_success() {
        return Err(CoreError::Unavailable(format!("model download: HTTP {}", resp.status())));
    }
    let total = resp.content_length().unwrap_or(tier.approx_mb * 1024 * 1024);

    let part = dir.join(format!("{}.part", tier.file));
    let mut file = std::fs::File::create(&part)?;
    let mut hasher = Sha256::new();
    let mut written: u64 = 0;
    let mut last_percent = 0u64;
    let mut resp = resp;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| CoreError::Unavailable(format!("model download: {e}")))?
    {
        use std::io::Write;
        file.write_all(&chunk)?;
        hasher.update(&chunk);
        written += chunk.len() as u64;
        let percent = written * 100 / total.max(1);
        if percent > last_percent {
            last_percent = percent;
            emit(
                (written as f64 / total.max(1) as f64).min(0.99),
                &format!("{} MB / {} MB", written / (1024 * 1024), total / (1024 * 1024)),
            );
        }
    }
    drop(file);

    let digest = format!("{:x}", hasher.finalize());
    if digest != tier.sha256 {
        let _ = std::fs::remove_file(&part);
        return Err(CoreError::Other(format!(
            "model download corrupted (sha256 {digest} != expected {}); removed — try again",
            tier.sha256
        )));
    }
    std::fs::rename(&part, &dest)?;
    emit(1.0, "download complete (verified)");
    Ok(dest.to_string_lossy().into_owned())
}
