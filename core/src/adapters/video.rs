//! VideoEngine: probe media, render the current cut, extract audio for STT.
//!
//! Production implementation shells out to ffmpeg/ffprobe. The build spec
//! calls for `ffmpeg-the-third` (in-process libav); the CLI adapter is the
//! first implementation behind the same trait — swapping it does not touch
//! the core. Hardware accel flags are chosen per platform by ffmpeg itself.

use crate::error::{CoreError, Result};
use crate::model::{Media, PlaySpan};
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

/// Everything one MP4 render needs. Grew from positional params — layout
/// compositing (screen + camera bubble) made a struct overdue.
pub struct RenderJob<'a> {
    pub media: &'a Media,
    /// What plays, grouped by piece, in output order. Each piece is read from
    /// its own input seeked to just before it, so a render decodes about what
    /// it uses instead of everything from 0:00. The main cut is one piece.
    pub pieces: &'a [Vec<PlaySpan>],
    pub out_path: &'a str,
    pub audio_offset_s: f64,
    /// Dual-capture compositing: the synced screen file + how to frame it.
    pub screen: Option<(&'a Media, &'a crate::model::Layout)>,
    /// Render 9:16 (1080×1920): a full-height crop of the frame, positioned
    /// across it at this fraction (0 = left, 0.5 = centred, 1 = right).
    pub vertical_crop_x: Option<f64>,
}

#[async_trait]
pub trait VideoEngine: Send + Sync {
    /// Probe a local file's metadata (no decode).
    async fn probe(&self, file_path: &str) -> Result<Media>;
    /// Render the job's spans, in order, to an MP4 at `out_path`, emitting
    /// `progress` events (task "export") as ffmpeg advances.
    async fn render_mp4(&self, job: &RenderJob<'_>, sink: &crate::events::SharedSink)
        -> Result<()>;
    /// Extract mono 16 kHz WAV (whisper input) to `out_path`.
    async fn extract_audio_wav(&self, media: &Media, out_path: &str) -> Result<()>;
    /// One still frame at `at` seconds, `width` px wide (height keeps the
    /// aspect), written as PNG to `out_path`. Face finding samples these.
    async fn extract_frame(&self, media: &Media, at: f64, width: u32, out_path: &str) -> Result<()>;
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

    async fn render_mp4(&self, job: &RenderJob<'_>, sink: &crate::events::SharedSink) -> Result<()> {
        let out_path = job.out_path;
        let plan = render_plan(job)?;
        let total_us = (plan.total_s * 1_000_000.0).max(1.0);
        let mut cmd = ffmpeg_cmd()?;
        cmd.args(["-y", "-hide_banner", "-nostats", "-progress", "pipe:1"]);
        for (seek, file) in &plan.inputs {
            if *seek > 0.0 {
                cmd.args(["-ss", &format!("{seek:.3}")]);
            }
            cmd.args(["-i", file]);
        }
        cmd.args([
            "-filter_complex", &plan.filter, "-map", "[v]", "-map", "[a]",
            "-c:v", "libx264", "-preset", "fast", "-crf", "18", "-c:a", "aac", out_path,
        ]);
        // Stream ffmpeg's key=value progress lines into UI events. A render
        // whose caller went away (a dropped request) stops with it.
        let mut child = cmd
            .kill_on_drop(true)
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
            // ffmpeg reports the cause last: keep the tail, not the head.
            let stderr = String::from_utf8_lossy(&out.stderr);
            let tail: String = {
                let chars: Vec<char> = stderr.trim_end().chars().collect();
                chars[chars.len().saturating_sub(800)..].iter().collect()
            };
            return Err(CoreError::Other(format!("ffmpeg render failed: {tail}")));
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

    async fn extract_frame(&self, media: &Media, at: f64, width: u32, out_path: &str) -> Result<()> {
        let mut cmd = ffmpeg_cmd()?;
        cmd.args([
            "-y", "-ss", &format!("{:.3}", at.max(0.0)), "-i", &media.file_path,
            "-frames:v", "1", "-vf", &format!("scale={width}:-2"), out_path,
        ]);
        Self::run(&mut cmd, "ffmpeg frame grab").await?;
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
            let raw = decode_raw_peaks(&media.file_path).await?;
            // Raw levels persist for ANALYSIS (silence detection needs real
            // energy); the display copy is normalized for visibility.
            std::fs::write(cache.join("peaks_raw.bin"), &raw)?;
            let mut peaks = raw;
            normalize_peaks(&mut peaks);
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
    let fix_index = needs_faststart(&media.file_path);
    // CoreAudio/WebKit refuse >2-channel AAC outright (the MacBook mic array
    // records 3ch) — those files play SILENT; downmix to stereo in the copy.
    let fix_audio = audio_channels(&media.file_path).await.unwrap_or(0) > 2;
    if !fix_index && !fix_audio {
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
            cmd.arg("-y").args(["-i", &media.file_path, "-c:v", "copy"]);
            if fix_audio {
                cmd.args(["-c:a", "aac", "-ac", "2", "-b:a", "192k"]);
            } else {
                cmd.args(["-c:a", "copy"]);
            }
            cmd.args(["-movflags", "+faststart", "-f", "mp4", &part.to_string_lossy()]);
            FfmpegCli::run(&mut cmd, "ffmpeg playable remux").await?;
            std::fs::rename(&part, &play)?;
            Ok::<_, crate::error::CoreError>(Some(play.to_string_lossy().into_owned()))
        }
        .await;
        result
    }
}

/// Audio channel count of the first audio stream (0 when probing fails).
async fn audio_channels(path: &str) -> Result<u32> {
    let probe = ffprobe_path()
        .ok_or_else(|| CoreError::Unavailable("ffprobe not found".into()))?;
    let mut cmd = Command::new(probe);
    cmd.args([
        "-v", "quiet",
        "-select_streams", "a:0",
        "-show_entries", "stream=channels",
        "-of", "csv=p=0",
        path,
    ]);
    let out = FfmpegCli::run(&mut cmd, "ffprobe channels").await?;
    Ok(out.trim().parse().unwrap_or(0))
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

/// Max |sample| per bucket, scaled to u8 — RAW levels, no normalization
/// (analysis wants real energy, not display scaling). Pure.
pub fn raw_peaks(pcm_s16le: &[u8], sample_rate: u32, peaks_per_second: u32) -> Vec<u8> {
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
    peaks
}

/// Normalize toward the file's own loudest moment: a quiet laptop mic
/// otherwise renders as a flat line. Floor of 4 (~1.5%) — live capture
/// measured real speech peaking at 6/255; true silence stays flat.
pub fn normalize_peaks(peaks: &mut [u8]) {
    let loudest = peaks.iter().copied().max().unwrap_or(0) as u32;
    if loudest >= 4 && loudest < 240 {
        for v in peaks.iter_mut() {
            *v = ((*v as u32 * 240) / loudest).min(255) as u8;
        }
    }
}

/// Display peaks: raw buckets, normalized. Pure — unit-tested without ffmpeg.
pub fn compute_peaks(pcm_s16le: &[u8], sample_rate: u32, peaks_per_second: u32) -> Vec<u8> {
    let mut peaks = raw_peaks(pcm_s16le, sample_rate, peaks_per_second);
    normalize_peaks(&mut peaks);
    peaks
}

/// Decode mono PCM and bucket to raw u8 peaks.
pub async fn decode_raw_peaks(file_path: &str) -> Result<Vec<u8>> {
    const RATE: u32 = 8000;
    let mut cmd = ffmpeg_cmd()?;
    cmd.args([
        "-i", file_path, "-vn", "-ac", "1", "-ar", &RATE.to_string(), "-f", "s16le", "pipe:1",
    ]);
    let pcm = FfmpegCli::run_binary(&mut cmd, "ffmpeg peaks decode").await?;
    Ok(raw_peaks(&pcm, RATE, PEAKS_PER_SECOND))
}

/// Raw analysis peaks for `media`, from cache or a one-off decode (caches
/// that predate raw peaks get backfilled). None = no ffmpeg/decoder.
pub async fn load_raw_peaks(media: &Media) -> Option<Vec<u8>> {
    let cache = crate::store::data_dir().join("cache").join(asset_cache_key(media));
    let raw_file = cache.join("peaks_raw.bin");
    if let Ok(bytes) = std::fs::read(&raw_file) {
        return Some(bytes);
    }
    let raw = decode_raw_peaks(&media.file_path).await.ok()?;
    let _ = std::fs::create_dir_all(&cache);
    let _ = std::fs::write(&raw_file, &raw);
    Some(raw)
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

    async fn render_mp4(&self, _job: &RenderJob<'_>, _sink: &crate::events::SharedSink) -> Result<()> {
        Err(CoreError::Unavailable(
            "MP4 render needs ffmpeg on PATH (demo mode active). NLE XML/EDL/SRT export still works.".into(),
        ))
    }

    async fn extract_audio_wav(&self, _m: &Media, _o: &str) -> Result<()> {
        Err(CoreError::Unavailable("audio extraction needs ffmpeg on PATH".into()))
    }

    async fn extract_frame(&self, _m: &Media, _at: f64, _w: u32, _o: &str) -> Result<()> {
        Err(CoreError::Unavailable("frame grabs need ffmpeg on PATH (demo mode active)".into()))
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

    async fn render_mp4(&self, job: &RenderJob<'_>, sink: &crate::events::SharedSink) -> Result<()> {
        if crate::capabilities::probe().media_ready() {
            FfmpegCli.render_mp4(job, sink).await
        } else {
            MockVideoEngine.render_mp4(job, sink).await
        }
    }

    async fn extract_audio_wav(&self, media: &Media, out_path: &str) -> Result<()> {
        if crate::capabilities::probe().media_ready() {
            FfmpegCli.extract_audio_wav(media, out_path).await
        } else {
            MockVideoEngine.extract_audio_wav(media, out_path).await
        }
    }

    async fn extract_frame(&self, media: &Media, at: f64, width: u32, out_path: &str) -> Result<()> {
        if crate::capabilities::probe().media_ready() {
            FfmpegCli.extract_frame(media, at, width, out_path).await
        } else {
            MockVideoEngine.extract_frame(media, at, width, out_path).await
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

/// One render's ffmpeg inputs and filter graph, built without running ffmpeg.
struct RenderPlan {
    /// `(seek seconds, file)` per input, in input order: one camera input per
    /// piece, then (when compositing) one screen input per piece.
    inputs: Vec<(f64, String)>,
    filter: String,
    /// Output length, seconds.
    total_s: f64,
}

/// Footage read before each piece: a second for the decoder to settle, plus
/// the A/V delay, so delayed audio starts on real audio rather than silence.
fn preroll(audio_offset_s: f64) -> f64 {
    1.0 + audio_offset_s.max(0.0)
}

/// Pieces at most this far apart share one input: decoding a short gap costs
/// less than another decoder of the same file.
const SHARED_INPUT_GAP_S: f64 = 30.0;
/// The most inputs of the source one render opens (one decoder each).
const MAX_INPUTS: usize = 12;
/// A screen input is never seeked closer than this to its end, so it always
/// has frames to clone (`tpad`) when the camera runs longer.
const SCREEN_TAIL_S: f64 = 3.0;

/// Which pieces share an input: runs of consecutive pieces, each starting at
/// or after the previous one's end and at most `SHARED_INPUT_GAP_S` later.
/// Past `MAX_INPUTS` runs, in-order neighbours with the smallest gaps merge
/// whatever the gap. Pieces that play out of source order never share (an
/// input decodes forward only).
fn input_groups(pieces: &[&Vec<PlaySpan>]) -> Vec<Vec<usize>> {
    let start = |i: usize| pieces[i].iter().map(|s| s.source_in).fold(f64::INFINITY, f64::min);
    let end = |i: usize| pieces[i].iter().map(|s| s.source_out).fold(f64::NEG_INFINITY, f64::max);
    let mut groups: Vec<Vec<usize>> = vec![];
    for i in 0..pieces.len() {
        match groups.last_mut() {
            Some(g) => {
                let gap = start(i) - end(*g.last().unwrap());
                if gap >= -1e-6 && gap <= SHARED_INPUT_GAP_S {
                    g.push(i);
                } else {
                    groups.push(vec![i]);
                }
            }
            None => groups.push(vec![i]),
        }
    }
    while groups.len() > MAX_INPUTS {
        let smallest = (0..groups.len() - 1)
            .map(|j| (j, start(groups[j + 1][0]) - end(*groups[j].last().unwrap())))
            .filter(|(_, gap)| *gap >= -1e-6)
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let Some((j, _)) = smallest else { break };
        let next = groups.remove(j + 1);
        groups[j].extend(next);
    }
    groups
}

/// Seconds rounded down to the millisecond: the exact value `-ss` receives,
/// so the trims below are measured from the same zero ffmpeg uses.
fn to_ms(t: f64) -> f64 {
    (t.max(0.0) * 1000.0).floor() / 1000.0
}

/// Build the inputs and trim/concat filter graph for a render. Each group of
/// pieces (`input_groups`) has its own input seeked to `start − preroll`; its
/// spans' trims are measured from that seek point. Video trims sit half a
/// frame early, so a span edge that falls exactly on a frame never depends on
/// float rounding: the frame AT the start is kept, the frame at the end isn't.
/// Each span's audio gets a 20 ms fade at both edges so joins never click —
/// video stays a hard cut, timing is untouched.
fn render_plan(job: &RenderJob<'_>) -> Result<RenderPlan> {
    let media = job.media;
    let pieces: Vec<&Vec<PlaySpan>> = job.pieces.iter().filter(|p| !p.is_empty()).collect();
    if pieces.is_empty() {
        return Err(CoreError::InvalidArg("timeline has no included clips".into()));
    }
    let half_frame = 0.5 / if media.frame_rate > 0.0 { media.frame_rate } else { 30.0 };
    let groups = input_groups(&pieces);
    let n_inputs = groups.len();
    // Dual-capture compositing: bake the SAME layout the preview shows.
    let compositing = job.screen.filter(|(_, l)| l.mode != "camera");
    let pre = preroll(job.audio_offset_s);
    let seeks: Vec<f64> = groups
        .iter()
        .map(|g| {
            let start = pieces[g[0]].iter().map(|s| s.source_in).fold(f64::INFINITY, f64::min);
            to_ms(start - pre)
        })
        .collect();
    let mut inputs: Vec<(f64, String)> = seeks.iter().map(|&t| (t, media.file_path.clone())).collect();
    // The screen seeks with the camera, but never into its last seconds; the
    // difference is taken off its timestamps so the two stay in step.
    let screen_seeks: Vec<f64> = match compositing {
        Some((screen, _)) => seeks.iter().map(|&t| to_ms(t.min(screen.duration - SCREEN_TAIL_S))).collect(),
        None => vec![],
    };
    if let Some((screen, _)) = compositing {
        inputs.extend(screen_seeks.iter().map(|&t| (t, screen.file_path.clone())));
    }

    let mut filter = String::new();
    let mut n = 0; // spans so far, across inputs
    let mut total_s = 0.0;
    for (k, group) in groups.iter().enumerate() {
        let spans: Vec<PlaySpan> = group.iter().flat_map(|&i| pieces[i].iter().copied()).collect();
        let m = spans.len();
        let (audio_graph, audio_srcs) = audio_sources(job.audio_offset_s, k, m);
        filter.push_str(&audio_graph);
        // Video source per span: raw camera, or the composed frame.
        let video_srcs: Vec<String> = match compositing {
            Some((screen, layout)) => {
                let shift = seeks[k] - screen_seeks[k];
                composite_graph(&mut filter, media, screen, layout, k, n_inputs + k, shift, m)
            }
            None => vec![format!("[{k}:v]"); m],
        };
        for (i, span) in spans.iter().enumerate() {
            let dur = span.duration();
            total_s += dur;
            let fade = (0.02_f64).min(dur / 4.0);
            let (a, b) = (span.source_in - seeks[k], span.source_out - seeks[k]);
            filter.push_str(&format!(
                "{vsrc}trim=start={va:.6}:end={vb:.6},setpts=PTS-STARTPTS[v{n}];\
                 {asrc}atrim=start={a:.6}:end={b:.6},asetpts=PTS-STARTPTS,\
                 afade=t=in:st=0:d={fade:.3},afade=t=out:st={fo:.3}:d={fade:.3}[a{n}];",
                vsrc = video_srcs[i],
                asrc = audio_srcs[i],
                va = (a - half_frame).max(0.0),
                vb = (b - half_frame).max(0.0),
                fo = (dur - fade).max(0.0),
            ));
            n += 1;
        }
    }
    for i in 0..n {
        filter.push_str(&format!("[v{i}][a{i}]"));
    }
    if let Some(x) = job.vertical_crop_x {
        // Reframe once, after the cuts: one crop for the whole clip.
        filter.push_str(&format!(
            "concat=n={n}:v=1:a=1[vcat][a];\
             [vcat]crop=w=trunc(ih*9/16/2)*2:h=ih:x=(iw-ow)*{x:.4}:y=0,\
             scale=1080:1920,setsar=1[v]",
            x = x.clamp(0.0, 1.0),
        ));
    } else {
        filter.push_str(&format!("concat=n={n}:v=1:a=1[v][a]"));
    }
    Ok(RenderPlan { inputs, filter, total_s })
}

/// The composed frame (screen + camera bubble, or screen only) for input
/// group `k`, read from camera input `cam` and screen input `scr`, split once
/// per span. `shift` is how much earlier the screen input was seeked than the
/// camera's; its timestamps move back by that much.
#[allow(clippy::too_many_arguments)]
fn composite_graph(
    filter: &mut String,
    media: &Media,
    screen: &Media,
    layout: &crate::model::Layout,
    cam: usize,
    scr: usize,
    shift: f64,
    spans: usize,
) -> Vec<String> {
    let k = cam;
    let w = (screen.width.max(2) / 2 * 2) as i64;
    let h = (screen.height.max(2) / 2 * 2) as i64;
    let realign = if shift > 0.0005 { format!("setpts=PTS-{shift:.6}/TB,") } else { String::new() };
    // The screen file is routinely a hair shorter than the camera (start
    // latency); trims past its EOF would emit EMPTY video that concat happily
    // desyncs. Clone-pad its last frame out to the camera's duration.
    let pad = (media.duration - screen.duration).max(0.0) + SCREEN_TAIL_S + 1.0;
    let screen_out = if layout.mode == "screen" { format!("[comp{k}]") } else { format!("[scr{k}]") };
    filter.push_str(&format!(
        "[{scr}:v]{realign}scale={w}:{h}:force_original_aspect_ratio=decrease,\
         pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1,\
         tpad=stop_mode=clone:stop_duration={pad:.3}{screen_out};"
    ));
    if layout.mode != "screen" {
        // The face bubble: scaled camera with a round / rounded-rect alpha
        // mask (geq on the small pip only — cheap), overlaid at the chosen
        // corner with a 3% inset.
        let pw = ((layout.size.clamp(0.10, 0.45) * w as f64) as i64 / 2 * 2).max(64);
        let scale_chain = if layout.shape == "round" {
            format!("scale={pw}:{pw}:force_original_aspect_ratio=increase,crop={pw}:{pw}")
        } else {
            format!("scale={pw}:-2")
        };
        let mask = if layout.shape == "round" {
            "if(lte((X-W/2)*(X-W/2)+(Y-H/2)*(Y-H/2),(W/2)*(W/2)),255,0)".to_string()
        } else {
            let r = (pw / 14).max(10);
            format!("if(lte(pow(max(abs(X-W/2)-(W/2-{r}),0),2)+pow(max(abs(Y-H/2)-(H/2-{r}),0),2),{r}*{r}),255,0)")
        };
        let inset_x = (w as f64 * 0.03) as i64;
        let inset_y = (h as f64 * 0.03) as i64;
        let x = if layout.corner.contains('l') { inset_x.to_string() } else { format!("main_w-overlay_w-{inset_x}") };
        let y = if layout.corner.contains('t') { inset_y.to_string() } else { format!("main_h-overlay_h-{inset_y}") };
        filter.push_str(&format!(
            "[{cam}:v]{scale_chain},format=rgba,\
             geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='{mask}'[cam{k}];\
             [scr{k}][cam{k}]overlay={x}:{y}[comp{k}];"
        ));
    }
    // A labeled stream is consumed once — fan out per span.
    let labels: Vec<String> = (0..spans).map(|i| format!("[cc{k}_{i}]")).collect();
    filter.push_str(&format!("[comp{k}]split={spans}{};", labels.concat()));
    labels
}

/// The per-span audio sources for input `input`, plus the filtergraph prefix
/// that produces them. The global A/V sync nudge is applied ONCE per input,
/// before the per-span trims: positive = delay audio (silence pad), negative =
/// audio starts |offset| early (trim).
///
/// The shifted stream is a filter output, and an output label must be read by
/// exactly one filter: reusing it once per span makes ffmpeg feed every span
/// after the first audio from the wrong place. So it is `asplit` into one copy
/// per span. A raw input like `[0:a]` may be read any number of times.
fn audio_sources(audio_offset_s: f64, input: usize, spans: usize) -> (String, Vec<String>) {
    if audio_offset_s.abs() <= 0.001 {
        return (String::new(), vec![format!("[{input}:a]"); spans]);
    }
    let mut graph = if audio_offset_s > 0.0 {
        let ms = (audio_offset_s * 1000.0).round() as i64;
        format!("[{input}:a]adelay={ms}:all=1[ash{input}];")
    } else {
        format!("[{input}:a]atrim=start={:.3},asetpts=PTS-STARTPTS[ash{input}];", -audio_offset_s)
    };
    let labels: Vec<String> = (0..spans).map(|i| format!("[as{input}_{i}]")).collect();
    graph.push_str(&format!("[ash{input}]asplit={}{};", spans, labels.concat()));
    (graph, labels)
}

#[cfg(test)]
mod tests {
    use super::{audio_sources, compute_peaks, render_plan, RenderJob};
    use crate::model::{Layout, Media, PlaySpan};

    #[test]
    fn shifted_audio_is_split_so_every_span_reads_its_own_copy() {
        // No offset: every span reads the raw input, nothing to split.
        let (graph, srcs) = audio_sources(0.0, 0, 3);
        assert!(graph.is_empty());
        assert_eq!(srcs, vec!["[0:a]"; 3]);

        // With an offset each span gets a distinct label from one asplit —
        // a filter output label can be consumed only once.
        for offset in [0.2, -0.15] {
            let (graph, srcs) = audio_sources(offset, 1, 3);
            assert!(graph.contains("[ash1]asplit=3[as1_0][as1_1][as1_2];"), "{graph}");
            assert_eq!(srcs, vec!["[as1_0]", "[as1_1]", "[as1_2]"]);
            assert_eq!(graph.matches("[ash1]").count(), 2, "produced once, read once: {graph}");
        }
        assert!(audio_sources(0.2, 0, 1).0.contains("[0:a]adelay=200"));
        assert!(audio_sources(-0.15, 0, 1).0.contains("atrim=start=0.150"));
    }

    fn media(path: &str) -> Media {
        Media {
            id: uuid::Uuid::new_v4(),
            file_path: path.into(),
            duration: 3600.0,
            frame_rate: 30.0,
            width: 1920,
            height: 1080,
            audio_sample_rate: 48000,
            codec: "h264".into(),
            imported_at: chrono::Utc::now(),
        }
    }

    fn span(a: f64, b: f64) -> PlaySpan {
        PlaySpan { source_in: a, source_out: b }
    }

    fn job<'a>(m: &'a Media, pieces: &'a [Vec<PlaySpan>], offset: f64) -> RenderJob<'a> {
        RenderJob { media: m, pieces, out_path: "/tmp/out.mp4", audio_offset_s: offset, screen: None, vertical_crop_x: None }
    }

    #[test]
    fn a_late_piece_seeks_instead_of_decoding_from_the_start() {
        let m = media("/v.mp4");
        // Minute 40, with a cut inside: two spans in one piece.
        let pieces = vec![vec![span(2400.0, 2410.0), span(2412.0, 2430.0)]];
        let plan = render_plan(&job(&m, &pieces, 0.0)).unwrap();
        assert_eq!(plan.inputs, vec![(2399.0, "/v.mp4".to_string())], "one second of preroll");
        // Audio exact; video half a frame (30 fps) early.
        assert!(plan.filter.contains("[0:a]atrim=start=1.000000:end=11.000000"), "{}", plan.filter);
        assert!(plan.filter.contains("[0:v]trim=start=0.983333:end=10.983333"), "{}", plan.filter);
        assert!(plan.filter.contains("[0:v]trim=start=12.983333:end=30.983333"), "{}", plan.filter);
        assert!(plan.filter.ends_with("[v0][a0][v1][a1]concat=n=2:v=1:a=1[v][a]"), "{}", plan.filter);
        assert!((plan.total_s - 28.0).abs() < 1e-9);
    }

    #[test]
    fn each_piece_reads_its_own_input_in_play_order() {
        let m = media("/v.mp4");
        // The payoff (1:45) plays before the setup (0:30).
        let pieces = vec![vec![span(105.0, 125.0)], vec![span(30.0, 50.0)]];
        let plan = render_plan(&job(&m, &pieces, 0.0)).unwrap();
        assert_eq!(plan.inputs.iter().map(|i| i.0).collect::<Vec<_>>(), vec![104.0, 29.0]);
        assert!(plan.filter.contains("[0:v]trim=start=0.983333:end=20.983333"));
        assert!(plan.filter.contains("[1:v]trim=start=0.983333:end=20.983333"));
        assert!(plan.filter.contains("[0:a]atrim") && plan.filter.contains("[1:a]atrim"));
    }

    #[test]
    fn a_delayed_track_gets_more_preroll_and_a_split_per_input() {
        let m = media("/v.mp4");
        let pieces = vec![vec![span(100.0, 110.0)], vec![span(200.0, 210.0), span(215.0, 220.0)]];
        let plan = render_plan(&job(&m, &pieces, 0.5)).unwrap();
        assert_eq!(plan.inputs.iter().map(|i| i.0).collect::<Vec<_>>(), vec![98.5, 198.5]);
        assert!(plan.filter.contains("[0:a]adelay=500:all=1[ash0];[ash0]asplit=1[as0_0];"), "{}", plan.filter);
        assert!(plan.filter.contains("[1:a]adelay=500:all=1[ash1];[ash1]asplit=2[as1_0][as1_1];"));
        // An early piece can't seek before 0.
        let early = vec![vec![span(0.2, 10.0)]];
        assert_eq!(render_plan(&job(&m, &early, -0.3)).unwrap().inputs[0].0, 0.0);
    }

    #[test]
    fn compositing_adds_a_screen_input_per_piece() {
        let m = media("/cam.mp4");
        let scr = media("/screen.mp4");
        let layout = Layout::default();
        let pieces = vec![vec![span(100.0, 110.0)], vec![span(10.0, 20.0)]];
        let mut j = job(&m, &pieces, 0.0);
        j.screen = Some((&scr, &layout));
        j.vertical_crop_x = Some(0.5);
        let plan = render_plan(&j).unwrap();
        let files: Vec<&str> = plan.inputs.iter().map(|i| i.1.as_str()).collect();
        assert_eq!(files, vec!["/cam.mp4", "/cam.mp4", "/screen.mp4", "/screen.mp4"]);
        assert_eq!(plan.inputs[2].0, plan.inputs[0].0, "screen and camera seek together");
        assert!(plan.filter.contains("[2:v]scale=") && plan.filter.contains("[3:v]scale="));
        assert!(plan.filter.contains("[scr0][cam0]overlay") && plan.filter.contains("[scr1][cam1]overlay"));
        assert!(plan.filter.contains("[cc1_0]trim="));
        assert!(plan.filter.contains("crop=w=trunc(ih*9/16/2)*2"));
    }

    #[test]
    fn nearby_pieces_share_an_input_and_inputs_are_capped() {
        let m = media("/v.mp4");
        let near = vec![vec![span(100.0, 110.0)], vec![span(120.0, 130.0)]];
        let plan = render_plan(&job(&m, &near, 0.0)).unwrap();
        assert_eq!(plan.inputs.len(), 1, "a 10 s gap is cheaper to decode than a second decoder");
        assert!(plan.filter.contains("[0:v]trim=start=20.983333:end=30.983333"), "{}", plan.filter);
        // Twenty pieces 100 s apart, in order: merged down to the cap.
        let far: Vec<Vec<PlaySpan>> = (0..20).map(|i| vec![span(i as f64 * 100.0, i as f64 * 100.0 + 5.0)]).collect();
        let plan = render_plan(&job(&m, &far, 0.0)).unwrap();
        assert_eq!(plan.inputs.len(), super::MAX_INPUTS);
        assert!(plan.filter.ends_with("concat=n=20:v=1:a=1[v][a]"));
    }

    #[test]
    fn the_seek_ffmpeg_gets_is_the_seek_the_trims_use() {
        // A piece starting on frame 2 of second 100 at 30 fps: the seek is
        // rounded to the millisecond once, and the trims measure from it.
        let m = media("/v.mp4");
        let start = 100.0 + 2.0 / 30.0;
        let pieces = vec![vec![span(start, start + 10.0)]];
        let plan = render_plan(&job(&m, &pieces, 0.0)).unwrap();
        assert_eq!(plan.inputs[0].0, 99.066);
        assert!(plan.filter.contains("[0:a]atrim=start=1.000667:"), "{}", plan.filter);
        assert!(plan.filter.contains("[0:v]trim=start=0.984000:"), "{}", plan.filter);
    }

    #[test]
    fn an_advanced_track_trims_each_input_and_keeps_the_base_preroll() {
        let m = media("/v.mp4");
        let pieces = vec![vec![span(100.0, 110.0)], vec![span(300.0, 310.0)]];
        let plan = render_plan(&job(&m, &pieces, -0.2)).unwrap();
        assert_eq!(plan.inputs.iter().map(|i| i.0).collect::<Vec<_>>(), vec![99.0, 299.0]);
        assert!(plan.filter.contains("[0:a]atrim=start=0.200,asetpts=PTS-STARTPTS[ash0]"), "{}", plan.filter);
        assert!(plan.filter.contains("[1:a]atrim=start=0.200,asetpts=PTS-STARTPTS[ash1]"), "{}", plan.filter);
    }

    #[test]
    fn a_screen_never_seeks_into_its_last_seconds() {
        let m = media("/cam.mp4");
        let mut scr = media("/screen.mp4");
        scr.duration = 180.0;
        let layout = Layout { mode: "screen".into(), ..Layout::default() };
        let pieces = vec![vec![span(179.5, 185.0)]];
        let mut j = job(&m, &pieces, 0.0);
        j.screen = Some((&scr, &layout));
        let plan = render_plan(&j).unwrap();
        assert_eq!(plan.inputs[0].0, 178.5);
        assert_eq!(plan.inputs[1].0, 177.0, "three seconds before the screen's end");
        assert!(plan.filter.contains("[1:v]setpts=PTS-1.500000/TB,scale="), "{}", plan.filter);
        assert!(plan.filter.contains("tpad=stop_mode=clone"));
    }

    #[test]
    fn nothing_to_play_is_an_error() {
        let m = media("/v.mp4");
        assert!(render_plan(&job(&m, &[vec![]], 0.0)).is_err());
    }

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
