//! One answer to "what can this machine do RIGHT NOW".
//!
//! Bootstrap, the setup screen, the demo banner, and the Auto adapters all
//! read this instead of probing separately — so installing ffmpeg or
//! downloading a whisper model mid-session takes effect on the next
//! operation, without restarting the app.

use crate::adapters::transcribe::{resolve_whisper_model, whisper_available, whisper_native_enabled};
use crate::adapters::video::{ffmpeg_path, ffprobe_path};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Capabilities {
    pub ffmpeg: Option<PathBuf>,
    pub ffprobe: Option<PathBuf>,
    /// Whisper model file that would be used, if any is downloaded.
    pub whisper_model: Option<PathBuf>,
    /// In-process whisper engine compiled in (no binary needed).
    pub whisper_native: bool,
    /// `whisper-cli` binary on PATH.
    pub whisper_cli: bool,
    /// `FABLE_DEMO=1` set — fixtures regardless of the toolchain.
    pub forced_demo: bool,
}

/// Resolve availability at call time (cheap: env + file-exists checks).
pub fn probe() -> Capabilities {
    Capabilities {
        ffmpeg: ffmpeg_path(),
        ffprobe: ffprobe_path(),
        whisper_model: resolve_whisper_model(),
        whisper_native: whisper_native_enabled(),
        whisper_cli: whisper_available(),
        forced_demo: std::env::var("FABLE_DEMO").map(|v| v == "1").unwrap_or(false),
    }
}

impl Capabilities {
    /// Probe/extract/render real media.
    pub fn media_ready(&self) -> bool {
        self.ffmpeg.is_some() && self.ffprobe.is_some()
    }

    /// A speech-to-text engine AND a model are both present.
    pub fn transcription_ready(&self) -> bool {
        self.whisper_model.is_some() && (self.whisper_native || self.whisper_cli)
    }

    /// Fixture adapters instead of real media processing.
    pub fn demo(&self) -> bool {
        self.forced_demo || !self.media_ready()
    }

    /// Human-readable reason demo mode is active (None = not in demo).
    pub fn demo_reason(&self) -> Option<String> {
        if self.forced_demo {
            return Some("FABLE_DEMO=1 is set".into());
        }
        if !self.media_ready() {
            return Some("ffmpeg/ffprobe not found (PATH, Homebrew, or FFMPEG_PATH)".into());
        }
        None
    }
}
