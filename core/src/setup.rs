//! First-run setup: report what the local toolchain is missing and download
//! whisper models into `<data dir>/models/` with progress events.
//!
//! The one deliberate egress in setup is the model download from Hugging
//! Face — user-triggered, never automatic, consistent with the local-first
//! principle (weights are fetched by the user at runtime, never bundled).

use crate::adapters::transcribe::models_dir;
use crate::capabilities;
use crate::engine::Editor;
use crate::error::{CoreError, Result};
use crate::events::SharedSink;
use serde::Serialize;

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
    pub demo: bool,
    /// Why demo mode is active (None when it isn't).
    pub demo_reason: Option<String>,
}

/// Snapshot of `capabilities::probe()` for the setup screen — resolved at
/// call time, so it's truthful even after mid-session installs/downloads.
pub fn status(editor: &Editor) -> SetupStatus {
    let caps = capabilities::probe();
    let demo = editor.demo_mode();
    SetupStatus {
        ffmpeg: caps.media_ready(),
        ffmpeg_path: caps.ffmpeg.as_ref().map(|p| p.to_string_lossy().into_owned()),
        whisper_model: caps.whisper_model.as_ref().map(|p| p.to_string_lossy().into_owned()),
        whisper_native: caps.whisper_native,
        whisper_cli: caps.whisper_cli,
        transcription_ready: caps.transcription_ready(),
        models_dir: models_dir().to_string_lossy().into_owned(),
        demo,
        demo_reason: if demo {
            caps.demo_reason().or_else(|| Some("fixture adapters forced (test instance)".into()))
        } else {
            None
        },
    }
}

pub struct ModelTier {
    pub id: &'static str,
    pub file: &'static str,
    pub approx_mb: u64,
}

/// Downloadable tiers, best first. "accurate" is the default recommendation
/// (large-v3-turbo quantized: near-large quality, ~1 GB RAM at runtime).
pub const MODEL_TIERS: &[ModelTier] = &[
    ModelTier { id: "accurate", file: "ggml-large-v3-turbo-q5_0.bin", approx_mb: 547 },
    ModelTier { id: "compact", file: "ggml-small-q5_1.bin", approx_mb: 190 },
];

const MODEL_BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// Stream a whisper model into the models dir, emitting `progress` events
/// (task `model_download`). Writes to a `.part` file and renames on success,
/// so an interrupted download never half-installs a model.
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
            crate::events::CoreEvent::progress("model_download", None, fraction, message),
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
        written += chunk.len() as u64;
        let percent = written * 100 / total.max(1);
        if percent > last_percent {
            last_percent = percent;
            emit(
                (written as f64 / total.max(1) as f64).min(1.0),
                &format!("{} MB / {} MB", written / (1024 * 1024), total / (1024 * 1024)),
            );
        }
    }
    drop(file);
    std::fs::rename(&part, &dest)?;
    emit(1.0, "download complete");
    Ok(dest.to_string_lossy().into_owned())
}
