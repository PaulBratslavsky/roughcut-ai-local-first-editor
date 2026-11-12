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
    /// Web-playable copy when the source itself isn't (an OBS .mkv doesn't
    /// open at all in WebKit; audio with more than 2 channels plays silent).
    /// None = play the source, or the copy isn't built yet.
    pub playback_path: Option<String>,
    /// Why building the playable copy failed in this run, if it did.
    pub playback_error: Option<String>,
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
    /// Render 9:16 (1080×1920): a 9:16 window of the frame, `crop_zoom`
    /// times closer than the full height, positioned at `crop_x` across
    /// (0 = left, 0.5 = centred, 1 = right) and `crop_y` down.
    pub vertical_crop: Option<crate::model::ClipFraming>,
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
    /// `<data dir>/cache/<asset_cache_key>/` (see `crate::asset_cache`).
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
        Self::output(cmd, what).await
    }

    async fn run(cmd: &mut Command, what: &str) -> Result<String> {
        Self::output(cmd, what).await.map(|out| String::from_utf8_lossy(&out).to_string())
    }

    /// stdout of a run that succeeded. A failure is logged with the end of
    /// stderr, where ffmpeg says what went wrong.
    async fn output(cmd: &mut Command, what: &str) -> Result<Vec<u8>> {
        let started = std::time::Instant::now();
        let out = match cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).output().await {
            Ok(out) => out,
            Err(e) => {
                let err = CoreError::Unavailable(format!("{what}: {e}"));
                log_failure(what, started, None, "", Some(&err));
                return Err(err);
            }
        };
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            log_failure(what, started, out.status.code(), &stderr, None);
            return Err(CoreError::Other(format!(
                "{what} failed: {}",
                stderr.chars().take(800).collect::<String>()
            )));
        }
        Ok(out.stdout)
    }
}

/// An `ffmpeg_failed` line in the app log. A warning: some callers expect
/// failures and carry on; the step that gives up logs the error.
fn log_failure(what: &str, started: std::time::Instant, exit_code: Option<i32>, stderr: &str, error: Option<&CoreError>) {
    crate::applog::write(
        crate::applog::Level::Warn,
        crate::applog::Event::FfmpegFailed {
            what: what.to_string(),
            exit_code,
            ms: started.elapsed().as_millis() as u64,
            stderr_tail: crate::applog::Tail::new(stderr),
            error: error.map(crate::applog::ErrText::from),
        },
    );
}

/// The last `n` characters of a tool's stderr (its cause comes last).
fn stderr_tail(stderr: &str, n: usize) -> String {
    let chars: Vec<char> = stderr.trim_end().chars().collect();
    chars[chars.len().saturating_sub(n)..].iter().collect()
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
            "-v", "error", "-print_format", "json", "-show_format", "-show_streams", file_path,
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
        let started = std::time::Instant::now();
        let mut child = match cmd.kill_on_drop(true).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
            Ok(child) => child,
            Err(e) => {
                let err = CoreError::Unavailable(format!("ffmpeg render: {e}"));
                log_failure("ffmpeg render", started, None, "", Some(&err));
                return Err(err);
            }
        };
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
            log_failure("ffmpeg render", started, out.status.code(), &stderr, None);
            return Err(CoreError::Other(format!("ffmpeg render failed: {}", stderr_tail(&stderr, 800))));
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
            // The first audio track (OBS's track 1), like the render and the
            // preview copy; ffmpeg's own pick is the track with most channels.
            "-y", "-i", &media.file_path, "-map", "0:a:0", "-vn", "-ac", "1", "-ar", "16000",
            "-c:a", "pcm_s16le", out_path,
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
        let cache = crate::asset_cache::ensure_dir_for(media)?;

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
        let playback_path = playable_copy_path(media);
        let playback_error = if playback_path.is_none() { playable_copy_failure(media) } else { None };

        Ok(MediaAssets {
            peaks_path: Some(peaks_file.to_string_lossy().into_owned()),
            peaks_per_second: PEAKS_PER_SECOND,
            thumbnails,
            playback_path,
            playback_error,
        })
    }
}

/// The preview copy, if one was already built for this source.
pub fn playable_copy_path(media: &Media) -> Option<String> {
    let play = crate::asset_cache::dir_for(media).join("play.mp4");
    play.is_file().then(|| play.to_string_lossy().into_owned())
}

/// Preview copies that failed in this run, by cache key, with the reason.
/// Kept in memory, not on disk: within a run a failure isn't retried on
/// every asset refetch; after a restart (say, with ffmpeg fixed) it is.
fn playable_failures() -> &'static std::sync::Mutex<std::collections::HashMap<String, String>> {
    static FAILED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, String>>> =
        std::sync::OnceLock::new();
    FAILED.get_or_init(Default::default)
}

/// Why this source's preview copy failed in this run, if it did.
pub fn playable_copy_failure(media: &Media) -> Option<String> {
    playable_failures().lock().unwrap().get(&asset_cache_key(media)).cloned()
}

/// Build the web-playable copy when the source needs one: audio WebKit can't
/// play, HEVC without Apple's `hvc1` tag, or a container WebKit can't open at
/// all (OBS's `.mkv`, `.webm`, `.avi`, …). An MP4 whose index sits at the end
/// plays as it is (the shell's `rcmedia` protocol serves every read the
/// player asks for), so it gets no full-size copy. Stream copy where the codecs
/// allow it, with +faststart, written to `.part` then renamed, so a crash
/// never leaves a half-copy that looks playable. Idempotent and deduplicated;
/// Ok(None) = the source plays as-is (or another task is building the copy).
/// A failure is remembered for the rest of the run (`playable_copy_failure`)
/// and returned again instead of re-running ffmpeg. Only the preview uses the
/// copy: transcription, render and export read the original through ffmpeg.
pub async fn ensure_playable_copy(media: &Media) -> Result<Option<String>> {
    if let Some(play) = playable_copy_path(media) {
        return Ok(Some(play));
    }
    if let Some(reason) = playable_copy_failure(media) {
        return Err(CoreError::Other(reason));
    }
    let result = build_playable_copy(media).await;
    if let Err(e) = &result {
        playable_failures().lock().unwrap().insert(asset_cache_key(media), e.to_string());
    }
    result
}

async fn build_playable_copy(media: &Media) -> Result<Option<String>> {
    let Some(fix) = playable_fix(&container_info(&media.file_path).await?) else {
        return Ok(None);
    };
    let cache = crate::asset_cache::ensure_dir_for(media)?;
    let play = cache.join("play.mp4");
    // One copy per cache key at a time (RAII: released even on panic).
    static IN_FLIGHT: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    let set = IN_FLIGHT.get_or_init(Default::default);
    let Some(_claim) = crate::inflight::try_claim(set, asset_cache_key(media)) else {
        return Ok(None); // someone else is building it; caller retries later
    };
    if play.is_file() {
        return Ok(Some(play.to_string_lossy().into_owned())); // built while we probed
    }
    let part = cache.join("play.mp4.part");
    let mut cmd = ffmpeg_cmd()?;
    cmd.args(["-hide_banner", "-v", "error", "-y", "-i", &media.file_path]);
    cmd.args(playable_args(&fix));
    cmd.arg(&part);
    if let Err(e) = FfmpegCli::run(&mut cmd, "ffmpeg playable copy").await {
        // A re-encode's partial output can be gigabytes.
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    std::fs::rename(&part, &play)?;
    Ok(Some(play.to_string_lossy().into_owned()))
}

/// What ffprobe says about a file's container and its first video and audio
/// streams: enough to decide whether WebKit can play it as-is.
#[derive(Debug, Clone, Default, PartialEq)]
struct ContainerInfo {
    /// ffprobe `format_name`: "mov,mp4,m4a,3gp,3g2,mj2", "matroska,webm", "avi", …
    format_name: String,
    video_codec: Option<String>,
    /// The video's sample-entry tag: `avc1`, `hvc1`, `hev1`, …
    video_tag: Option<String>,
    audio_codec: Option<String>,
    audio_channels: u32,
}

impl ContainerInfo {
    /// MP4 and MOV (and .m4v / .3gp): ffprobe names them all "mov,mp4,…".
    fn is_mp4(&self) -> bool {
        self.format_name.split(',').any(|f| f == "mp4" || f == "mov")
    }
}

/// How to build the preview copy.
#[derive(Debug, Clone, PartialEq)]
struct PlayableFix {
    /// Convert the video to H.264 (a codec WebKit can't play, in a container
    /// it can't open); otherwise the video is copied as-is.
    reencode_video: bool,
    /// HEVC copied into MP4 needs the `hvc1` tag, or Apple players refuse it.
    tag_hvc1: bool,
    audio: AudioFix,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum AudioFix {
    /// No audio stream.
    None,
    Copy,
    /// To stereo AAC: CoreAudio/WebKit refuse >2-channel AAC outright (the
    /// MacBook mic array records 3ch — those files play SILENT), and audio
    /// that isn't AAC (OBS can write Opus or PCM) doesn't play from an MP4.
    StereoAac,
}

async fn container_info(path: &str) -> Result<ContainerInfo> {
    let probe = ffprobe_path()
        .ok_or_else(|| CoreError::Unavailable("ffprobe not found".into()))?;
    let mut cmd = Command::new(probe);
    cmd.args([
        "-v", "error",
        "-show_entries", "format=format_name:stream=codec_type,codec_name,codec_tag_string,channels",
        "-of", "json",
        path,
    ]);
    let out = FfmpegCli::run(&mut cmd, "ffprobe container").await?;
    parse_container_info(&out)
        .ok_or_else(|| CoreError::Other(format!("ffprobe container: unreadable output for {path}")))
}

/// None when the output isn't ffprobe JSON with a format name.
fn parse_container_info(json: &str) -> Option<ContainerInfo> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let format_name = v["format"]["format_name"].as_str()?.to_string();
    let streams = v["streams"].as_array().cloned().unwrap_or_default();
    let first = |kind: &str| streams.iter().find(|s| s["codec_type"] == kind).cloned();
    let video = first("video");
    let audio = first("audio");
    Some(ContainerInfo {
        format_name,
        video_codec: video.as_ref().and_then(|s| s["codec_name"].as_str().map(str::to_string)),
        video_tag: video.and_then(|s| s["codec_tag_string"].as_str().map(str::to_string)),
        audio_codec: audio.as_ref().and_then(|s| s["codec_name"].as_str().map(str::to_string)),
        audio_channels: audio.and_then(|s| s["channels"].as_u64()).unwrap_or(0) as u32,
    })
}

/// None = the original plays in the preview as-is.
fn playable_fix(info: &ContainerInfo) -> Option<PlayableFix> {
    let is_mp4 = info.is_mp4();
    let hevc = info.video_codec.as_deref() == Some("hevc");
    let audio = match info.audio_codec.as_deref() {
        None => AudioFix::None,
        Some(_) if info.audio_channels > 2 => AudioFix::StereoAac,
        Some("aac") => AudioFix::Copy,
        // An MP4's own audio has always been copied; only other containers
        // bring codecs that need converting.
        Some(_) if is_mp4 => AudioFix::Copy,
        Some(_) => AudioFix::StereoAac,
    };
    if is_mp4 {
        // Wherever its index sits: WebKit reads a late `moov` fine through
        // `rcmedia`. Only audio it can't play, or HEVC tagged `hev1` (Apple
        // players need `hvc1`), needs a copy.
        let untagged_hevc = hevc && info.video_tag.as_deref() != Some("hvc1");
        if audio != AudioFix::StereoAac && !untagged_hevc {
            return None;
        }
        return Some(PlayableFix { reencode_video: false, tag_hvc1: hevc, audio });
    }
    let plays = matches!(info.video_codec.as_deref(), None | Some("h264") | Some("hevc"));
    Some(PlayableFix { reencode_video: !plays, tag_hvc1: hevc, audio })
}

/// ffmpeg arguments between `-i <source>` and the output path.
fn playable_args(fix: &PlayableFix) -> Vec<&'static str> {
    // The first video and the first audio track: the ones `ContainerInfo`
    // describes and the render plays (`[0:a]`). ffmpeg's own pick would be
    // the audio track with the most channels.
    // Optional maps: the ffprobe-failed fallback copies without knowing
    // whether there is any audio.
    let mut args = vec!["-map", "0:v:0?"];
    if fix.audio != AudioFix::None {
        args.extend(["-map", "0:a:0?"]);
    }
    if fix.reencode_video {
        // yuv420p needs even dimensions (a browser screen recording can be 1365x767).
        args.extend(["-c:v", "libx264", "-preset", "fast", "-crf", "18", "-pix_fmt", "yuv420p"]);
        args.extend(["-vf", "scale=trunc(iw/2)*2:trunc(ih/2)*2"]);
    } else {
        args.extend(["-c:v", "copy"]);
        if fix.tag_hvc1 {
            args.extend(["-tag:v", "hvc1"]);
        }
    }
    match fix.audio {
        AudioFix::None => args.push("-an"),
        AudioFix::Copy => args.extend(["-c:a", "copy"]),
        AudioFix::StereoAac => args.extend(["-c:a", "aac", "-ac", "2", "-b:a", "192k"]),
    }
    // Subtitle and data tracks (OBS chapters, mkv attachments) aren't mapped,
    // and an MP4 couldn't hold most of them anyway.
    args.extend(["-sn", "-dn", "-movflags", "+faststart", "-f", "mp4"]);
    args
}

/// Join files end to end without re-encoding (`-f concat -c copy`) into a new
/// file in `<data dir>/media/`; the inputs are untouched. Stream copy of files
/// whose codecs, frame size or audio format differ plays broken, so they are
/// refused instead. Only the first video and first audio track are joined.
/// The result is an MP4 when every input is MP4/MOV; otherwise it is an
/// `.mkv`, so the preview copy (`ensure_playable_copy`) still fixes what an
/// MP4 couldn't play (HEVC's tag, Opus or PCM audio). Used by "add footage to
/// the end" (`Editor::append_media`).
pub async fn concat_media(paths: &[String]) -> Result<String> {
    if paths.len() < 2 {
        return Err(CoreError::InvalidArg("pick at least two files to join".into()));
    }
    let ffprobe = ffprobe_path()
        .ok_or_else(|| CoreError::Unavailable("ffprobe not found".into()))?;
    let mut cmd = ffmpeg_cmd()?;
    let mut signature: Option<String> = None;
    let mut all_mp4 = true;
    for p in paths {
        if !Path::new(p).is_file() {
            return Err(CoreError::NotFound(format!("file {p}")));
        }
        let mut probe = Command::new(&ffprobe);
        probe.args([
            "-v", "error",
            "-show_entries",
            "format=format_name:stream=codec_type,codec_name,width,height,channels,sample_rate",
            "-of", "json",
            p,
        ]);
        let out = FfmpegCli::run(&mut probe, "ffprobe streams").await?;
        let (is_mp4, sig) = join_signature(&out)
            .ok_or_else(|| CoreError::Other(format!("ffprobe streams: unreadable output for {p}")))?;
        all_mp4 &= is_mp4;
        match &signature {
            None => signature = Some(sig),
            Some(first) if *first != sig => {
                return Err(CoreError::InvalidArg(format!(
                    "the files don't match ({first} vs {sig}): adding footage needs the same \
                     codec, resolution and audio format"
                )));
            }
            _ => {}
        }
    }
    let dir = crate::store::data_dir().join("media");
    std::fs::create_dir_all(&dir)?;
    // The random suffix keeps two joins in the same second from writing the
    // same file (ffmpeg runs with -y; the first may be another project's media).
    let out = dir.join(format!(
        "joined-{}-{}.{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S"),
        &Uuid::new_v4().simple().to_string()[..8],
        if all_mp4 { "mp4" } else { "mkv" },
    ));
    let list_path = std::env::temp_dir().join(format!(
        "roughcut-concat-{}.txt",
        uuid::Uuid::new_v4().simple()
    ));
    let list: String = paths
        .iter()
        .map(|p| format!("file '{}'\n", p.replace('\'', "'\\''")))
        .collect();
    std::fs::write(&list_path, list)?;
    cmd.args(["-hide_banner", "-v", "error", "-y", "-f", "concat", "-safe", "0", "-i"])
        .arg(&list_path)
        .args(["-map", "0:v:0", "-map", "0:a:0?", "-c", "copy"]);
    if all_mp4 {
        cmd.args(["-movflags", "+faststart"]);
    }
    cmd.arg(&out).stdin(Stdio::null());
    let result = FfmpegCli::run(&mut cmd, "ffmpeg join").await;
    let _ = std::fs::remove_file(&list_path);
    if let Err(e) = result {
        let _ = std::fs::remove_file(&out);
        return Err(e);
    }
    Ok(out.to_string_lossy().into_owned())
}

/// (is MP4/MOV, what must match to join by stream copy): the first video's
/// codec and size, the first audio's codec, channels and sample rate.
fn join_signature(json: &str) -> Option<(bool, String)> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let format = v["format"]["format_name"].as_str()?;
    let is_mp4 = format.split(',').any(|f| f == "mp4" || f == "mov");
    let streams = v["streams"].as_array().cloned().unwrap_or_default();
    let first = |kind: &str| streams.iter().find(|s| s["codec_type"] == kind).cloned();
    let s = |x: &serde_json::Value, k: &str| match &x[k] {
        serde_json::Value::String(t) => t.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => "?".into(),
    };
    let video = first("video").map_or("no video".to_string(), |x| {
        format!("{} {}x{}", s(&x, "codec_name"), s(&x, "width"), s(&x, "height"))
    });
    let audio = first("audio").map_or("no audio".to_string(), |x| {
        format!("{} {}ch {}Hz", s(&x, "codec_name"), s(&x, "channels"), s(&x, "sample_rate"))
    });
    Some((is_mp4, format!("{video} + {audio}")))
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
        // The first audio track, as transcription and the render use.
        "-i", file_path, "-map", "0:a:0", "-vn", "-ac", "1", "-ar", &RATE.to_string(), "-f",
        "s16le", "pipe:1",
    ]);
    let pcm = FfmpegCli::run_binary(&mut cmd, "ffmpeg peaks decode").await?;
    Ok(raw_peaks(&pcm, RATE, PEAKS_PER_SECOND))
}

/// Raw analysis peaks for `media`, from cache or a one-off decode (caches
/// that predate raw peaks get backfilled). None = no ffmpeg/decoder.
pub async fn load_raw_peaks(media: &Media) -> Option<Vec<u8>> {
    let raw_file = crate::asset_cache::dir_for(media).join("peaks_raw.bin");
    if let Ok(bytes) = std::fs::read(&raw_file) {
        return Some(bytes);
    }
    let raw = decode_raw_peaks(&media.file_path).await.ok()?;
    if crate::asset_cache::ensure_dir_for(media).is_ok() {
        let _ = std::fs::write(&raw_file, &raw);
    }
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
            playback_error: None,
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
    if let Some(f) = job.vertical_crop {
        // Reframe once, after the cuts: one crop for the whole clip. Unzoomed
        // it is the full height; zoomed, 1/zoom of it, placed up and down too.
        let f = f.clamped();
        let crop = if f.crop_zoom <= 1.0 {
            format!("crop=w=trunc(ih*9/16/2)*2:h=ih:x=(iw-ow)*{x:.4}:y=0", x = f.crop_x)
        } else {
            format!(
                "crop=w=trunc(ih/{z:.4}*9/16/2)*2:h=trunc(ih/{z:.4}/2)*2:x=(iw-ow)*{x:.4}:y=(ih-oh)*{y:.4}",
                z = f.crop_zoom,
                x = f.crop_x,
                y = f.crop_y,
            )
        };
        filter.push_str(&format!(
            "concat=n={n}:v=1:a=1[vcat][a];[vcat]{crop},scale=1080:1920:flags=lanczos,setsar=1[v]"
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
        RenderJob { media: m, pieces, out_path: "/tmp/out.mp4", audio_offset_s: offset, screen: None, vertical_crop: None }
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
        j.vertical_crop = Some(crate::model::ClipFraming::CENTRED);
        let plan = render_plan(&j).unwrap();
        let files: Vec<&str> = plan.inputs.iter().map(|i| i.1.as_str()).collect();
        assert_eq!(files, vec!["/cam.mp4", "/cam.mp4", "/screen.mp4", "/screen.mp4"]);
        assert_eq!(plan.inputs[2].0, plan.inputs[0].0, "screen and camera seek together");
        assert!(plan.filter.contains("[2:v]scale=") && plan.filter.contains("[3:v]scale="));
        assert!(plan.filter.contains("[scr0][cam0]overlay") && plan.filter.contains("[scr1][cam1]overlay"));
        assert!(plan.filter.contains("[cc1_0]trim="));
        assert!(plan.filter.contains("crop=w=trunc(ih*9/16/2)*2:h=ih:x=(iw-ow)*0.5000:y=0,scale=1080:1920"));
    }

    #[test]
    fn a_zoomed_vertical_crop_takes_a_smaller_window_placed_both_ways() {
        let m = media("/cam.mp4");
        let pieces = vec![vec![span(10.0, 20.0)]];
        let mut j = job(&m, &pieces, 0.0);
        j.vertical_crop = Some(crate::model::ClipFraming { crop_x: 0.25, crop_y: 0.1, crop_zoom: 2.0 });
        let plan = render_plan(&j).unwrap();
        assert!(
            plan.filter.contains(
                "[vcat]crop=w=trunc(ih/2.0000*9/16/2)*2:h=trunc(ih/2.0000/2)*2:x=(iw-ow)*0.2500:y=(ih-oh)*0.1000,\
                 scale=1080:1920:flags=lanczos,setsar=1[v]"
            ),
            "{}",
            plan.filter
        );
        // Out-of-range framing is clamped, not passed to ffmpeg.
        j.vertical_crop = Some(crate::model::ClipFraming { crop_x: 7.0, crop_y: -1.0, crop_zoom: 9.0 });
        let plan = render_plan(&j).unwrap();
        assert!(plan.filter.contains("crop=w=trunc(ih/3.0000*9/16/2)*2") && plan.filter.contains("x=(iw-ow)*1.0000:y=(ih-oh)*0.0000"));
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

#[cfg(test)]
mod playable_tests {
    use super::{
        join_signature, parse_container_info, playable_args, playable_fix, AudioFix, ContainerInfo,
        PlayableFix,
    };

    fn info(format: &str, video: Option<&str>, audio: Option<&str>, channels: u32) -> ContainerInfo {
        ContainerInfo {
            format_name: format.into(),
            video_codec: video.map(str::to_string),
            video_tag: None,
            audio_codec: audio.map(str::to_string),
            audio_channels: channels,
        }
    }

    const MP4: &str = "mov,mp4,m4a,3gp,3g2,mj2";
    const MKV: &str = "matroska,webm";

    #[test]
    fn a_normal_mp4_plays_as_is() {
        // Wherever its index sits: there's no index flag to pass any more.
        assert_eq!(playable_fix(&info(MP4, Some("h264"), Some("aac"), 2)), None);
        // An MP4's own audio was never converted unless it had >2 channels.
        assert_eq!(playable_fix(&info(MP4, Some("h264"), Some("pcm_s16le"), 2)), None);
        let tagged = ContainerInfo { video_tag: Some("hvc1".into()), ..info(MP4, Some("hevc"), Some("aac"), 2) };
        assert_eq!(playable_fix(&tagged), None, "HEVC already tagged hvc1");
    }

    #[test]
    fn mp4_copies_only_for_audio_or_the_hevc_tag() {
        let hev1 = ContainerInfo { video_tag: Some("hev1".into()), ..info(MP4, Some("hevc"), Some("aac"), 2) };
        let fix = playable_fix(&hev1).unwrap();
        assert_eq!(fix, PlayableFix { reencode_video: false, tag_hvc1: true, audio: AudioFix::Copy });
        let three_ch = playable_fix(&info(MP4, Some("h264"), Some("aac"), 3)).unwrap();
        assert_eq!(three_ch.audio, AudioFix::StereoAac);
        assert!(!three_ch.reencode_video);
    }

    #[test]
    fn obs_mkv_is_copied_into_an_mp4() {
        let fix = playable_fix(&info(MKV, Some("h264"), Some("aac"), 2)).unwrap();
        assert_eq!(fix, PlayableFix { reencode_video: false, tag_hvc1: false, audio: AudioFix::Copy });
        let args = playable_args(&fix);
        assert!(args.windows(2).any(|w| w == ["-c:v", "copy"]), "{args:?}");
        assert!(args.windows(2).any(|w| w == ["-c:a", "copy"]), "{args:?}");
        assert!(args.contains(&"-sn") && args.contains(&"+faststart"));
        assert_eq!(args.last(), Some(&"mp4"));
    }

    #[test]
    fn hevc_gets_the_apple_tag() {
        let fix = playable_fix(&info(MKV, Some("hevc"), Some("aac"), 2)).unwrap();
        assert!(fix.tag_hvc1 && !fix.reencode_video);
        assert!(playable_args(&fix).windows(2).any(|w| w == ["-tag:v", "hvc1"]));
    }

    #[test]
    fn other_audio_in_other_containers_becomes_aac() {
        for codec in ["opus", "pcm_s16le", "flac", "mp3"] {
            let fix = playable_fix(&info(MKV, Some("h264"), Some(codec), 2)).unwrap();
            assert_eq!(fix.audio, AudioFix::StereoAac, "{codec}");
        }
        let fix = playable_fix(&info("avi", Some("h264"), None, 0)).unwrap();
        assert_eq!(fix.audio, AudioFix::None);
        assert!(playable_args(&fix).contains(&"-an"));
    }

    #[test]
    fn video_webkit_cant_play_is_converted() {
        for codec in ["vp9", "vp8", "mpeg4", "av1"] {
            let fix = playable_fix(&info(MKV, Some(codec), Some("opus"), 2)).unwrap();
            assert!(fix.reencode_video, "{codec}");
            assert!(playable_args(&fix).contains(&"libx264"));
        }
    }

    #[test]
    fn parses_ffprobe_json() {
        let json = r#"{"streams":[
            {"codec_name":"h264","codec_type":"video"},
            {"codec_name":"aac","codec_type":"audio","channels":2},
            {"codec_name":"aac","codec_type":"audio","channels":1}],
            "format":{"format_name":"matroska,webm"}}"#;
        assert_eq!(parse_container_info(json), Some(info(MKV, Some("h264"), Some("aac"), 2)));
        let tagged = r#"{"streams":[{"codec_name":"hevc","codec_tag_string":"hev1","codec_type":"video"}],
            "format":{"format_name":"mov,mp4,m4a,3gp,3g2,mj2"}}"#;
        assert_eq!(parse_container_info(tagged).unwrap().video_tag.as_deref(), Some("hev1"));
        // Unreadable output is an error, never "not an MP4, no audio" (that
        // would build a silent copy of a file that was fine).
        assert_eq!(parse_container_info("not json"), None);
        assert_eq!(parse_container_info(r#"{"streams":[]}"#), None);
    }

    #[test]
    fn copies_map_the_first_tracks_and_reencodes_to_even_sizes() {
        let fix = playable_fix(&info(MKV, Some("h264"), Some("aac"), 2)).unwrap();
        let args = playable_args(&fix);
        assert!(args.windows(2).any(|w| w == ["-map", "0:v:0?"]), "{args:?}");
        assert!(args.windows(2).any(|w| w == ["-map", "0:a:0?"]), "{args:?}");
        let silent = playable_fix(&info(MKV, Some("h264"), None, 0)).unwrap();
        assert!(!playable_args(&silent).contains(&"0:a:0?"));
        let vp9 = playable_fix(&info(MKV, Some("vp9"), Some("opus"), 2)).unwrap();
        assert!(playable_args(&vp9).contains(&"scale=trunc(iw/2)*2:trunc(ih/2)*2"));
    }

    #[test]
    fn join_signature_compares_first_tracks_only() {
        let one = r#"{"streams":[
            {"codec_type":"video","codec_name":"h264","width":1920,"height":1080},
            {"codec_type":"audio","codec_name":"aac","channels":2,"sample_rate":"48000"}],
            "format":{"format_name":"matroska,webm"}}"#;
        // OBS with a second audio track: same first tracks, so it still joins
        // a file made by an earlier join (which keeps only the first track).
        let two = r#"{"streams":[
            {"codec_type":"video","codec_name":"h264","width":1920,"height":1080},
            {"codec_type":"audio","codec_name":"aac","channels":2,"sample_rate":"48000"},
            {"codec_type":"audio","codec_name":"aac","channels":6,"sample_rate":"48000"}],
            "format":{"format_name":"mov,mp4,m4a,3gp,3g2,mj2"}}"#;
        let (a_mp4, a) = join_signature(one).unwrap();
        let (b_mp4, b) = join_signature(two).unwrap();
        assert!(!a_mp4 && b_mp4);
        assert_eq!(a, b);
        assert_eq!(a, "h264 1920x1080 + aac 2ch 48000Hz");
        let resampled = one.replace("48000", "44100");
        assert_ne!(join_signature(&resampled).unwrap().1, a, "sample rate must match");
        assert_eq!(join_signature("{}"), None);
    }
}
