//! VideoEngine: probe media, render the current cut, extract audio for STT.
//!
//! Production implementation shells out to ffmpeg/ffprobe. The build spec
//! calls for `ffmpeg-the-third` (in-process libav); the CLI adapter is the
//! first implementation behind the same trait — swapping it does not touch
//! the core. Hardware accel flags are chosen per platform by ffmpeg itself.

use crate::error::{CoreError, Result};
use crate::model::{Media, Timeline};
use async_trait::async_trait;
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;
use uuid::Uuid;

#[async_trait]
pub trait VideoEngine: Send + Sync {
    /// Probe a local file's metadata (no decode).
    async fn probe(&self, file_path: &str) -> Result<Media>;
    /// Render the included clips of `timeline` to an MP4 at `out_path`.
    async fn render_mp4(&self, media: &Media, timeline: &Timeline, out_path: &str) -> Result<()>;
    /// Extract mono 16 kHz WAV (whisper input) to `out_path`.
    async fn extract_audio_wav(&self, media: &Media, out_path: &str) -> Result<()>;
}

pub fn ffmpeg_available() -> bool {
    ffmpeg_path().is_some() && ffprobe_path().is_some()
}

/// Resolve the ffmpeg binary: `FFMPEG_PATH` env, a bundled copy in
/// `<data dir>/bin/`, then PATH and the usual install locations. GUI apps on
/// macOS don't inherit the shell PATH, so checking Homebrew dirs explicitly
/// matters.
pub fn ffmpeg_path() -> Option<PathBuf> {
    resolve_binary("ffmpeg", "FFMPEG_PATH")
}

pub fn ffprobe_path() -> Option<PathBuf> {
    resolve_binary("ffprobe", "FFPROBE_PATH")
}

fn resolve_binary(name: &str, env_var: &str) -> Option<PathBuf> {
    if let Ok(p) = std::env::var(env_var) {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let exe = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    let bundled = crate::store::data_dir().join("bin").join(&exe);
    if bundled.is_file() {
        return Some(bundled);
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(&exe);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
        let candidate = Path::new(dir).join(&exe);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn ffmpeg_cmd() -> Result<Command> {
    let path = ffmpeg_path()
        .ok_or_else(|| CoreError::Unavailable("ffmpeg not found (PATH, Homebrew, or FFMPEG_PATH)".into()))?;
    Ok(Command::new(path))
}

pub struct FfmpegCli;

impl FfmpegCli {
    async fn run(cmd: &mut Command, what: &str) -> Result<String> {
        let out = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| CoreError::Unavailable(format!("{what}: {e}")))?;
        if !out.status.success() {
            return Err(CoreError::Other(format!(
                "{what} failed: {}",
                String::from_utf8_lossy(&out.stderr).chars().take(800).collect::<String>()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }
}

#[async_trait]
impl VideoEngine for FfmpegCli {
    async fn probe(&self, file_path: &str) -> Result<Media> {
        if !Path::new(file_path).exists() {
            return Err(CoreError::NotFound(format!("file {file_path}")));
        }
        let probe = ffprobe_path()
            .ok_or_else(|| CoreError::Unavailable("ffprobe not found (PATH, Homebrew, or FFPROBE_PATH)".into()))?;
        let mut cmd = Command::new(probe);
        cmd.args([
            "-v", "quiet", "-print_format", "json", "-show_format", "-show_streams", file_path,
        ]);
        let stdout = Self::run(&mut cmd, "ffprobe").await?;
        let v: serde_json::Value = serde_json::from_str(&stdout)
            .map_err(|e| CoreError::Other(format!("ffprobe parse: {e}")))?;
        let streams = v["streams"].as_array().cloned().unwrap_or_default();
        let video = streams
            .iter()
            .find(|s| s["codec_type"] == "video")
            .ok_or_else(|| CoreError::InvalidArg("no video stream".into()))?;
        let audio = streams.iter().find(|s| s["codec_type"] == "audio");
        let fps = parse_rate(video["r_frame_rate"].as_str().unwrap_or("30/1"));
        let duration = v["format"]["duration"]
            .as_str()
            .and_then(|d| d.parse::<f64>().ok())
            .unwrap_or(0.0);
        Ok(Media {
            id: Uuid::new_v4(),
            file_path: file_path.to_string(),
            duration,
            frame_rate: fps,
            width: video["width"].as_u64().unwrap_or(0) as u32,
            height: video["height"].as_u64().unwrap_or(0) as u32,
            audio_sample_rate: audio
                .and_then(|a| a["sample_rate"].as_str())
                .and_then(|s| s.parse().ok())
                .unwrap_or(48000),
            codec: video["codec_name"].as_str().unwrap_or("unknown").to_string(),
            imported_at: Utc::now(),
        })
    }

    async fn render_mp4(&self, media: &Media, timeline: &Timeline, out_path: &str) -> Result<()> {
        let clips: Vec<_> = timeline.included_clips().collect();
        if clips.is_empty() {
            return Err(CoreError::InvalidArg("timeline has no included clips".into()));
        }
        // Build a trim/concat filtergraph over the single source.
        let mut filter = String::new();
        for (i, c) in clips.iter().enumerate() {
            filter.push_str(&format!(
                "[0:v]trim=start={in_}:end={out},setpts=PTS-STARTPTS[v{i}];\
                 [0:a]atrim=start={in_}:end={out},asetpts=PTS-STARTPTS[a{i}];",
                in_ = c.source_in,
                out = c.source_out,
            ));
        }
        for i in 0..clips.len() {
            filter.push_str(&format!("[v{i}][a{i}]"));
        }
        filter.push_str(&format!("concat=n={}:v=1:a=1[v][a]", clips.len()));
        let mut cmd = ffmpeg_cmd()?;
        cmd.args([
            "-y", "-i", &media.file_path, "-filter_complex", &filter, "-map", "[v]", "-map",
            "[a]", "-c:v", "libx264", "-preset", "fast", "-crf", "18", "-c:a", "aac", out_path,
        ]);
        Self::run(&mut cmd, "ffmpeg render").await?;
        Ok(())
    }

    async fn extract_audio_wav(&self, media: &Media, out_path: &str) -> Result<()> {
        let mut cmd = ffmpeg_cmd()?;
        cmd.args([
            "-y", "-i", &media.file_path, "-vn", "-ac", "1", "-ar", "16000", "-c:a",
            "pcm_s16le", out_path,
        ]);
        Self::run(&mut cmd, "ffmpeg audio extract").await?;
        Ok(())
    }
}

fn parse_rate(s: &str) -> f64 {
    let mut it = s.splitn(2, '/');
    let num: f64 = it.next().and_then(|n| n.parse().ok()).unwrap_or(30.0);
    let den: f64 = it.next().and_then(|n| n.parse().ok()).unwrap_or(1.0);
    if den == 0.0 {
        30.0
    } else {
        num / den
    }
}

/// Demo-mode engine: fabricates plausible metadata, refuses to render.
pub struct MockVideoEngine;

#[async_trait]
impl VideoEngine for MockVideoEngine {
    async fn probe(&self, file_path: &str) -> Result<Media> {
        Ok(Media {
            id: Uuid::new_v4(),
            file_path: file_path.to_string(),
            duration: 300.0,
            frame_rate: 30.0,
            width: 3840,
            height: 2160,
            audio_sample_rate: 48000,
            codec: "h264 (demo)".into(),
            imported_at: Utc::now(),
        })
    }

    async fn render_mp4(&self, _m: &Media, _t: &Timeline, _o: &str) -> Result<()> {
        Err(CoreError::Unavailable(
            "MP4 render needs ffmpeg on PATH (demo mode active). NLE XML/EDL/SRT export still works.".into(),
        ))
    }

    async fn extract_audio_wav(&self, _m: &Media, _o: &str) -> Result<()> {
        Err(CoreError::Unavailable("audio extraction needs ffmpeg on PATH".into()))
    }
}

/// Resolves the real or mock engine PER CALL via `capabilities::probe()`, so
/// installing ffmpeg mid-session takes effect without restarting.
pub struct AutoVideoEngine;

#[async_trait]
impl VideoEngine for AutoVideoEngine {
    async fn probe(&self, file_path: &str) -> Result<Media> {
        if crate::capabilities::probe().media_ready() {
            FfmpegCli.probe(file_path).await
        } else {
            MockVideoEngine.probe(file_path).await
        }
    }

    async fn render_mp4(&self, media: &Media, timeline: &Timeline, out_path: &str) -> Result<()> {
        if crate::capabilities::probe().media_ready() {
            FfmpegCli.render_mp4(media, timeline, out_path).await
        } else {
            MockVideoEngine.render_mp4(media, timeline, out_path).await
        }
    }

    async fn extract_audio_wav(&self, media: &Media, out_path: &str) -> Result<()> {
        if crate::capabilities::probe().media_ready() {
            FfmpegCli.extract_audio_wav(media, out_path).await
        } else {
            MockVideoEngine.extract_audio_wav(media, out_path).await
        }
    }
}
