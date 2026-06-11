//! One streaming download: .part file → optional sha256 verify → atomic
//! rename, with throttled per-percent progress. Three features had divergent
//! copies (whisper models had hashing but the others didn't; the llama
//! tarball buffered entirely in RAM with no progress at all).

use crate::error::{CoreError, Result};
use crate::events::{send, CoreEvent, ProgressTask, SharedSink};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;

pub struct DownloadOpts<'a> {
    pub task: ProgressTask,
    /// Pinned sha256 (hex); verified before the rename when present.
    pub sha256: Option<&'a str>,
    /// Fallback size for progress when the server omits content-length.
    pub approx_bytes: Option<u64>,
}

/// Stream `url` to `dest`. Never leaves a half-written file at `dest`:
/// writes to `<dest>.part`, verifies, then renames.
pub async fn download_to(
    sink: &SharedSink,
    url: &str,
    dest: &Path,
    opts: DownloadOpts<'_>,
) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let client = reqwest::Client::builder()
        .user_agent("roughcut")
        .build()
        .map_err(|e| CoreError::Unavailable(format!("download client: {e}")))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| CoreError::Unavailable(format!("download: {e}")))?;
    if !resp.status().is_success() {
        return Err(CoreError::Unavailable(format!("download: HTTP {}", resp.status())));
    }
    let total = resp.content_length().or(opts.approx_bytes).unwrap_or(0);

    let part = dest.with_extension(format!(
        "{}.part",
        dest.extension().and_then(|e| e.to_str()).unwrap_or("bin")
    ));
    let mut file = std::fs::File::create(&part)?;
    let mut hasher = Sha256::new();
    let mut written: u64 = 0;
    let mut last_percent = 0u64;
    let mut resp = resp;
    while let Some(chunk) =
        resp.chunk().await.map_err(|e| CoreError::Unavailable(format!("download: {e}")))?
    {
        file.write_all(&chunk)?;
        if opts.sha256.is_some() {
            hasher.update(&chunk);
        }
        written += chunk.len() as u64;
        if total > 0 {
            let percent = written * 100 / total;
            if percent > last_percent {
                last_percent = percent;
                send(
                    sink,
                    CoreEvent::progress(
                        opts.task,
                        None,
                        (written as f64 / total as f64).min(0.99),
                        format!("{} MB / {} MB", written / 1_048_576, total / 1_048_576),
                    ),
                );
            }
        }
    }
    drop(file);

    if let Some(expected) = opts.sha256 {
        let digest = format!("{:x}", hasher.finalize());
        if digest != expected {
            let _ = std::fs::remove_file(&part);
            return Err(CoreError::Other(format!(
                "download corrupted (sha256 {digest} != expected {expected}); removed — try again"
            )));
        }
    }
    std::fs::rename(&part, dest)?;
    Ok(())
}
