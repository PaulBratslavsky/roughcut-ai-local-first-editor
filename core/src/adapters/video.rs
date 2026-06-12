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

/// Derived view data for the timeline: audio peaks + a thumbnail filmstrip.
/// FILE PATHS, not payloads — the frontend streams them over the asset
/// protocol (binary never crosses JSON IPC; see docs/05).
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, serde::Serialize)]
pub struct MediaAssets {
    /// Binary file of u8 peaks (0-255), `peaks_per_second` per second.
    pub peaks_path: Option<String>,
    pub peaks_per_second: u32,
    pub thumbnails: Vec<ThumbnailRef>,
    /// Web-playable copy when the source itself isn't (mp4 with the moov
    /// index at the END streams as an endless black frame in WebKit; ffmpeg
    /// remuxes it losslessly with +faststart). None = play the source.
    pub playback_path: Option<String>,
}

#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, serde::Serialize)]
pub struct ThumbnailRef {
    /// Source time (seconds) the frame represents.
    pub time: f64,
    pub path: String,
}

pub const PEAKS_PER_SECOND: u32 = 50;

#[async_trait]
pub trait VideoEngine: Send + Sync {
    /// Probe a local file's metadata (no decode).
    async fn probe(&self, file_path: &str) -> Result<Media>;
    /// Render the included clips of `timeline` to an MP4 at `out_path`,
    /// emitting `progress` events (task "export") as ffmpeg advances.
    async fn render_mp4(
        &self,
        media: &Media,
        timeline: &Timeline,
        out_path: &str,
        audio_offset_s: f64,
        sink: &crate::events::SharedSink,
    ) -> Result<()>;
    /// Extract mono 16 kHz WAV (whisper input) to `out_path`.
    async fn extract_audio_wav(&self, media: &Media, out_path: &str) -> Result<()>;
    /// Waveform peaks + thumbnail filmstrip, generated once and cached in
    /// `<data dir>/cache/<media id>/`.
    async fn media_assets(&self, media: &Media) -> Result<MediaAssets>;
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
    async fn run_binary(cmd: &mut Command, what: &str) -> Result<Vec<u8>> {
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
        Ok(out.stdout)
    }

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

    async fn render_mp4(
        &self,
        media: &Media,
        timeline: &Timeline,
        out_path: &str,
        audio_offset_s: f64,
        sink: &crate::events::SharedSink,
    ) -> Result<()> {
        let clips: Vec<_> = timeline.included_clips().collect();
        if clips.is_empty() {
            return Err(CoreError::InvalidArg("timeline has no included clips".into()));
        }
        let total_us = (timeline.included_duration() * 1_000_000.0).max(1.0);
        // Build a trim/concat filtergraph over the single source. Each clip's
        // audio gets a 20ms fade at both edges so joins never click — video
        // stays a hard cut, timing is untouched.
        let mut filter = String::new();
        // Global A/V sync nudge, applied ONCE before the per-clip trims:
        // positive = delay audio (silence pad), negative = audio starts
        // |offset| early (trim). The per-clip atrims read the shifted stream.
        let audio_src = if audio_offset_s.abs() > 0.001 {
            if audio_offset_s > 0.0 {
                let ms = (audio_offset_s * 1000.0).round() as i64;
                filter.push_str(&format!("[0:a]adelay={ms}:all=1[ash];"));
            } else {
                filter.push_str(&format!(
                    "[0:a]atrim=start={:.3},asetpts=PTS-STARTPTS[ash];",
                    -audio_offset_s
                ));
            }
            "[ash]"
        } else {
            "[0:a]"
        };
        for (i, c) in clips.iter().enumerate() {
            let dur = c.duration();
            let fade = (0.02_f64).min(dur / 4.0);
            filter.push_str(&format!(
                "[0:v]trim=start={in_}:end={out},setpts=PTS-STARTPTS[v{i}];\
                 {audio_src}atrim=start={in_}:end={out},asetpts=PTS-STARTPTS,\
                 afade=t=in:st=0:d={fade:.3},afade=t=out:st={fo:.3}:d={fade:.3}[a{i}];",
                in_ = c.source_in,
                out = c.source_out,
                fo = (dur - fade).max(0.0),
                audio_src = audio_src,
            ));
        }
        for i in 0..clips.len() {
            filter.push_str(&format!("[v{i}][a{i}]"));
        }
        filter.push_str(&format!("concat=n={}:v=1:a=1[v][a]", clips.len()));
        let mut cmd = ffmpeg_cmd()?;
        cmd.args([
            "-y", "-nostats", "-progress", "pipe:1",
            "-i", &media.file_path, "-filter_complex", &filter, "-map", "[v]", "-map",
            "[a]", "-c:v", "libx264", "-preset", "fast", "-crf", "18", "-c:a", "aac", out_path,
        ]);
        // Stream ffmpeg's key=value progress lines into UI events.
        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| CoreError::Unavailable(format!("ffmpeg render: {e}")))?;
        let progress_task = child.stdout.take().map(|stdout| {
            crate::progress::stream_progress(
                stdout,
                sink.clone(),
                crate::events::ProgressTask::Export,
                None,
                crate::progress::Delimiter::Newline,
                move |line| {
                    let us = line.strip_prefix("out_time_us=")?.parse::<f64>().ok()?;
                    let frac = (us / total_us).clamp(0.0, 0.99);
                    Some((frac, format!("rendering MP4… {}%", (frac * 100.0) as u8)))
                },
            )
        });
        let out = child
            .wait_with_output()
            .await
            .map_err(|e| CoreError::Other(format!("ffmpeg render: {e}")))?;
        if let Some(t) = progress_task {
            let _ = t.await; // drain the tail
        }
        if !out.status.success() {
            return Err(CoreError::Other(format!(
                "ffmpeg render failed: {}",
                String::from_utf8_lossy(&out.stderr).chars().take(800).collect::<String>()
            )));
        }
        crate::events::send(
            sink,
            crate::events::CoreEvent::progress(crate::events::ProgressTask::Export, None, 1.0, "render complete"),
        );
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

    async fn media_assets(&self, media: &Media) -> Result<MediaAssets> {
        // Cache by file identity (path + size + mtime), NOT media id: every
        // project pointing at the same footage — duplicates, re-imports,
        // MCP-created projects — shares one set of peaks/thumbnails.
        let cache = crate::store::data_dir().join("cache").join(asset_cache_key(media));
        std::fs::create_dir_all(&cache)?;

        // ---- waveform peaks (u8 per 1/PEAKS_PER_SECOND s) -------------------
        let peaks_file = cache.join("peaks.bin");
        if !peaks_file.is_file() {
            const RATE: u32 = 8000;
            let mut cmd = ffmpeg_cmd()?;
            cmd.args([
                "-i", &media.file_path, "-vn", "-ac", "1", "-ar", &RATE.to_string(), "-f",
                "s16le", "pipe:1",
            ]);
            let pcm = Self::run_binary(&mut cmd, "ffmpeg peaks decode").await?;
            let peaks = compute_peaks(&pcm, RATE, PEAKS_PER_SECOND);
            std::fs::write(&peaks_file, &peaks)?;
        }

        // ---- thumbnail filmstrip --------------------------------------------
        // Interval scales with duration: ~1 thumb per 1-10s, max ~240 frames.
        let interval = (media.duration / 240.0).clamp(1.0, 10.0);
        let first = cache.join("thumb_0001.jpg");
        if !first.is_file() {
            let mut cmd = ffmpeg_cmd()?;
            cmd.args([
                "-y",
                "-hwaccel", "auto",
                "-i", &media.file_path,
                "-vf", &format!("fps=1/{interval},scale=160:-2"),
                "-q:v", "7",
                &cache.join("thumb_%04d.jpg").to_string_lossy(),
            ]);
            Self::run(&mut cmd, "ffmpeg thumbnails").await?;
        }
        let mut thumbnails = vec![];
        for i in 1.. {
            let path = cache.join(format!("thumb_{i:04}.jpg"));
            if !path.is_file() {
                break;
            }
            thumbnails.push(ThumbnailRef {
                time: (i - 1) as f64 * interval,
                path: path.to_string_lossy().into_owned(),
            });
        }
        // Playable copy if a PREVIOUS remux produced one. Building a missing
        // one is slow on multi-GB sources — the engine kicks that off in the
        // background (ensure_playable_copy) so peaks/thumbnails return now.
        let play = cache.join("play.mp4");
        let playback_path = play.is_file().then(|| play.to_string_lossy().into_owned());

        Ok(MediaAssets {
            peaks_path: Some(peaks_file.to_string_lossy().into_owned()),
            peaks_per_second: PEAKS_PER_SECOND,
            thumbnails,
            playback_path,
        })
    }
}

/// Build the web-playable copy when the source needs one (moov after mdat).
/// Lossless stream copy with +faststart, written to `.part` then renamed, so
/// a crash never leaves a half-copy that looks playable. Idempotent and
/// deduplicated; Ok(None) = the source plays as-is.
pub async fn ensure_playable_copy(media: &Media) -> Result<Option<String>> {
    if !needs_faststart(&media.file_path) {
        return Ok(None);
    }
    let cache = crate::store::data_dir().join("cache").join(asset_cache_key(media));
    std::fs::create_dir_all(&cache)?;
    let play = cache.join("play.mp4");
    if play.is_file() {
        return Ok(Some(play.to_string_lossy().into_owned()));
    }
    // One remux per cache key at a time (RAII: released even on panic).
    {
        static IN_FLIGHT: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
            std::sync::OnceLock::new();
        let set = IN_FLIGHT.get_or_init(Default::default);
        let Some(_claim) = crate::inflight::try_claim(set, asset_cache_key(media)) else {
            return Ok(None); // someone else is building it; caller retries later
        };
        let result = async {
            let part = cache.join("play.mp4.part");
            let mut cmd = ffmpeg_cmd()?;
            cmd.args([
                "-y",
                "-i", &media.file_path,
                "-c", "copy",
                "-movflags", "+faststart",
                "-f", "mp4",
                &part.to_string_lossy(),
            ]);
            FfmpegCli::run(&mut cmd, "ffmpeg faststart remux").await?;
            std::fs::rename(&part, &play)?;
            Ok::<_, crate::error::CoreError>(Some(play.to_string_lossy().into_owned()))
        }
        .await;
        result
    }
}

/// True when an mp4/mov's `moov` index sits AFTER the `mdat` payload —
/// fine for ffmpeg, an endless black frame for progressive playback in
/// WebKit. Box-header walk only; reads a few dozen bytes.
fn needs_faststart(path: &str) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut off: u64 = 0;
    let mut saw_mdat = false;
    let mut header = [0u8; 16];
    while off + 8 <= len {
        if f.seek(SeekFrom::Start(off)).is_err() || f.read_exact(&mut header[..8]).is_err() {
            return false;
        }
        let mut size = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as u64;
        let typ: [u8; 4] = header[4..8].try_into().unwrap();
        if size == 1 {
            if f.read_exact(&mut header[8..16]).is_err() {
                return false;
            }
            size = u64::from_be_bytes(header[8..16].try_into().unwrap());
        }
        if size < 8 {
            return false; // not an MP4 box structure
        }
        match &typ {
            b"moov" => return saw_mdat,
            b"mdat" => saw_mdat = true,
            _ => {}
        }
        off = off.saturating_add(size);
    }
    false
}

/// Stable identity for derived assets: hash of path + size + mtime, so the
/// cache survives re-imports and invalidates when the file changes.
pub fn asset_cache_key(media: &Media) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"v4:"); // peaks floor 4 + capture gain era // peaks normalization — regenerate older assets
    hasher.update(media.file_path.as_bytes());
    if let Ok(meta) = std::fs::metadata(&media.file_path) {
        hasher.update(meta.len().to_le_bytes());
        if let Ok(modified) = meta.modified() {
            if let Ok(d) = modified.duration_since(std::time::UNIX_EPOCH) {
                hasher.update(d.as_secs().to_le_bytes());
            }
        }
    }
    format!("{:x}", hasher.finalize())[..16].to_string()
}

/// Max |sample| per bucket, scaled to u8. Pure — unit-tested without ffmpeg.
pub fn compute_peaks(pcm_s16le: &[u8], sample_rate: u32, peaks_per_second: u32) -> Vec<u8> {
    let bucket = (sample_rate / peaks_per_second).max(1) as usize;
    let mut peaks = Vec::with_capacity(pcm_s16le.len() / 2 / bucket + 1);
    let mut max: i32 = 0;
    let mut n = 0usize;
    for chunk in pcm_s16le.chunks_exact(2) {
        let s = i16::from_le_bytes([chunk[0], chunk[1]]) as i32;
        max = max.max(s.abs());
        n += 1;
        if n == bucket {
            peaks.push(((max * 255) / 32768).min(255) as u8);
            max = 0;
            n = 0;
        }
    }
    if n > 0 {
        peaks.push(((max * 255) / 32768).min(255) as u8);
    }
    // Normalize toward the file's own loudest moment: a quiet laptop mic
    // otherwise renders as a flat line. Floor of 8 (~3%) — whisper can
    // transcribe speech well below the old floor of 24, and a transcribable
    // take deserves a visible waveform; true silence stays flat.
    let loudest = peaks.iter().copied().max().unwrap_or(0) as u32;
    if loudest >= 4 && loudest < 240 {
        let scale_num = 240u32;
        for v in &mut peaks {
            *v = ((*v as u32 * scale_num) / loudest).min(255) as u8;
        }
    }
    peaks
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

    async fn render_mp4(
        &self,
        _m: &Media,
        _t: &Timeline,
        _o: &str,
        _offset: f64,
        _sink: &crate::events::SharedSink,
    ) -> Result<()> {
        Err(CoreError::Unavailable(
            "MP4 render needs ffmpeg on PATH (demo mode active). NLE XML/EDL/SRT export still works.".into(),
        ))
    }

    async fn extract_audio_wav(&self, _m: &Media, _o: &str) -> Result<()> {
        Err(CoreError::Unavailable("audio extraction needs ffmpeg on PATH".into()))
    }

    async fn media_assets(&self, _media: &Media) -> Result<MediaAssets> {
        // No real audio/video to derive from — the frontend falls back to its
        // synthetic waveform.
        Ok(MediaAssets {
            peaks_path: None,
            peaks_per_second: PEAKS_PER_SECOND,
            thumbnails: vec![],
            playback_path: None,
        })
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

    async fn render_mp4(
        &self,
        media: &Media,
        timeline: &Timeline,
        out_path: &str,
        audio_offset_s: f64,
        sink: &crate::events::SharedSink,
    ) -> Result<()> {
        if crate::capabilities::probe().media_ready() {
            FfmpegCli.render_mp4(media, timeline, out_path, audio_offset_s, sink).await
        } else {
            MockVideoEngine.render_mp4(media, timeline, out_path, audio_offset_s, sink).await
        }
    }

    async fn extract_audio_wav(&self, media: &Media, out_path: &str) -> Result<()> {
        if crate::capabilities::probe().media_ready() {
            FfmpegCli.extract_audio_wav(media, out_path).await
        } else {
            MockVideoEngine.extract_audio_wav(media, out_path).await
        }
    }

    async fn media_assets(&self, media: &Media) -> Result<MediaAssets> {
        if crate::capabilities::probe().media_ready() {
            FfmpegCli.media_assets(media).await
        } else {
            MockVideoEngine.media_assets(media).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::compute_peaks;

    #[test]
    fn peaks_bucket_max_and_scale() {
        // 100 samples at 100 Hz, 10 peaks/s => bucket of 10 samples.
        let mut pcm = vec![];
        for i in 0..100i16 {
            let v: i16 = if i == 25 { 16384 } else if i == 95 { -32768 } else { 100 };
            pcm.extend_from_slice(&v.to_le_bytes());
        }
        let peaks = compute_peaks(&pcm, 100, 10);
        assert_eq!(peaks.len(), 10);
        assert_eq!(peaks[2], 127); // 16384/32768*255
        assert_eq!(peaks[9], 255); // clamped full-scale negative
        assert!(peaks[0] <= 1);
    }
}
