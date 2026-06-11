//! Capture: record camera + mic through ffmpeg's avfoundation input.
//! M1 scope — camera-only sessions; the same session machinery grows screen
//! (M2) and dual capture (M3). One active session at a time; liveness is
//! honest (try_wait, same lesson as the managed LLM sidecar).

use crate::error::{CoreError, Result};
use crate::events::{ProgressTask, SharedSink};
use serde::Serialize;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};

#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct CaptureDevice {
    pub index: u32,
    pub name: String,
    /// avfoundation lists displays as video devices ("Capture screen N").
    pub is_screen: bool,
}

#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct CaptureDevices {
    pub cameras: Vec<CaptureDevice>,
    pub screens: Vec<CaptureDevice>,
    pub microphones: Vec<CaptureDevice>,
}

#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct RecordingStatus {
    pub recording: bool,
    /// Paused between takes (session alive, no ffmpeg running).
    pub paused: bool,
    /// 1-based current/most recent take number.
    pub take: u32,
    /// Seconds in the CURRENT take (0 while paused).
    pub elapsed_s: f64,
    /// Seconds across all finished takes + the current one.
    pub total_s: f64,
    pub out_path: Option<String>,
}

/// One file in ~/Movies/RoughCut — the recordings library.
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct RecordingFile {
    pub path: String,
    pub name: String,
    pub size_mb: f64,
    /// RFC3339; newest first in the listing.
    pub modified: String,
    pub duration_s: f64,
}

/// Parse `ffmpeg -f avfoundation -list_devices true -i ""` stderr.
/// (ffmpeg exits non-zero for this invocation by design — the listing IS
/// the output.)
pub fn parse_devices(stderr: &str) -> CaptureDevices {
    let mut cameras = vec![];
    let mut screens = vec![];
    let mut microphones = vec![];
    let mut section = "";
    for line in stderr.lines() {
        if line.contains("AVFoundation video devices") {
            section = "video";
            continue;
        }
        if line.contains("AVFoundation audio devices") {
            section = "audio";
            continue;
        }
        let Some(idx_start) = line.find("] [") else { continue };
        let rest = &line[idx_start + 3..];
        let Some(close) = rest.find(']') else { continue };
        let Ok(index) = rest[..close].parse::<u32>() else { continue };
        let name = rest[close + 1..].trim().to_string();
        if name.is_empty() {
            continue;
        }
        let is_screen = name.starts_with("Capture screen");
        let dev = CaptureDevice { index, name, is_screen };
        match section {
            "video" if is_screen => screens.push(dev),
            "video" => cameras.push(dev),
            "audio" => microphones.push(dev),
            _ => {}
        }
    }
    CaptureDevices { cameras, screens, microphones }
}

/// Enumerate devices (cached ~20s — the Setup panel polls).
pub async fn list_devices() -> Result<CaptureDevices> {
    {
        let cache = device_cache().lock().unwrap();
        if let Some((at, devices)) = cache.as_ref() {
            if at.elapsed().as_secs() < 20 {
                return Ok(devices.clone());
            }
        }
    }
    let ffmpeg = super::video::ffmpeg_path()
        .ok_or_else(|| CoreError::Unavailable("ffmpeg not found".into()))?;
    let out = Command::new(ffmpeg)
        .args(["-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", ""])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| CoreError::Unavailable(format!("ffmpeg device list: {e}")))?;
    let devices = parse_devices(&String::from_utf8_lossy(&out.stderr));
    *device_cache().lock().unwrap() = Some((std::time::Instant::now(), devices.clone()));
    Ok(devices)
}

#[allow(clippy::type_complexity)]
fn device_cache() -> &'static Mutex<Option<(std::time::Instant, CaptureDevices)>> {
    static C: std::sync::OnceLock<Mutex<Option<(std::time::Instant, CaptureDevices)>>> =
        std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

struct Session {
    /// None while paused between takes.
    child: Option<Child>,
    started: std::time::Instant,
    /// Wall seconds accumulated in finished takes.
    finished_s: f64,
    /// Finished take files, in order.
    takes: Vec<PathBuf>,
    /// Path of the take being written (current child's output).
    current: PathBuf,
    camera: u32,
    microphone: u32,
    /// Base name (no extension) all takes share.
    base: String,
}

fn active() -> &'static Mutex<Option<Session>> {
    static A: std::sync::OnceLock<Mutex<Option<Session>>> = std::sync::OnceLock::new();
    A.get_or_init(Default::default)
}

fn recordings_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join("Movies/RoughCut")
}

/// A capture mode the device actually supports.
#[derive(Debug, Clone, Copy)]
struct Mode {
    w: u32,
    h: u32,
    fps_min: f64,
    fps_max: f64,
}

/// avfoundation only lists a camera's modes in the ERROR for an impossible
/// request — so ask for 9999x9999 and parse the refusal. Cached per index.
async fn probe_modes(camera: u32) -> Vec<Mode> {
    {
        let cache = mode_cache().lock().unwrap();
        if let Some(modes) = cache.get(&camera) {
            return modes.clone();
        }
    }
    let Some(ffmpeg) = super::video::ffmpeg_path() else { return vec![] };
    let out = Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-f", "avfoundation",
            "-video_size", "9999x9999",
            "-i", &format!("{camera}:none"),
            "-t", "0.1",
            "-f", "null", "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await;
    let mut modes = vec![];
    if let Ok(out) = out {
        for line in String::from_utf8_lossy(&out.stderr).lines() {
            // "  1920x1080@[60.000000 60.000000]fps"
            let line = line.trim();
            let Some((dims, rest)) = line.split_once("@[") else { continue };
            let dims = dims.rsplit(' ').next().unwrap_or(dims);
            let Some((w, h)) = dims.split_once('x') else { continue };
            let (Ok(w), Ok(h)) = (w.trim().parse(), h.trim().parse()) else { continue };
            let Some(range) = rest.strip_suffix("]fps") else { continue };
            let mut parts = range.split_whitespace();
            let (Some(lo), Some(hi)) = (parts.next(), parts.next()) else { continue };
            let (Ok(fps_min), Ok(fps_max)) = (lo.parse(), hi.parse()) else { continue };
            modes.push(Mode { w, h, fps_min, fps_max });
        }
    }
    mode_cache().lock().unwrap().insert(camera, modes.clone());
    modes
}

fn mode_cache() -> &'static Mutex<std::collections::HashMap<u32, Vec<Mode>>> {
    static C: std::sync::OnceLock<Mutex<std::collections::HashMap<u32, Vec<Mode>>>> =
        std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// Best mode for a talking head: true landscape beats square beats portrait
/// (cameras offer square Center-Stage modes whose AREA beats 1080p), then
/// closest to 16:9, then largest. fps clamps toward 30 — 60fps doubles file
/// size for no spoken-video gain.
fn pick_mode(modes: &[Mode]) -> Option<(Mode, f64)> {
    let best = modes.iter().max_by_key(|m| {
        let landscape = (m.w > m.h) as u64;
        let aspect = m.w as f64 / m.h.max(1) as f64;
        // 0 deviation = perfect 16:9; subtract from a big constant so
        // closer-to-16:9 ranks higher inside an integer key.
        let aspect_rank = 1_000_000u64.saturating_sub((aspect - 16.0 / 9.0).abs().mul_add(100_000.0, 0.0) as u64);
        (landscape, aspect_rank, m.w as u64 * m.h as u64)
    })?;
    let fps = 30.0_f64.clamp(best.fps_min, best.fps_max);
    Some((*best, fps))
}

/// Spawn one ffmpeg take writing to `out`. Shared by start and resume.
async fn spawn_take(sink: &SharedSink, camera: u32, microphone: u32, out: &PathBuf) -> Result<Child> {
    let ffmpeg = super::video::ffmpeg_path()
        .ok_or_else(|| CoreError::Unavailable("ffmpeg not found".into()))?;
    // Cameras lie about defaults (a 1920x1080-only camera fell back to
    // 1080x1920 portrait when asked for an unsupported rate) — request a
    // mode the device actually advertises.
    let picked = pick_mode(&probe_modes(camera).await);
    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-nostats".into(),
        "-f".into(), "avfoundation".into(),
    ];
    if let Some((mode, fps)) = picked {
        args.push("-framerate".into());
        args.push(format!("{fps}"));
        args.push("-video_size".into());
        args.push(format!("{}x{}", mode.w, mode.h));
    } else {
        args.push("-framerate".into());
        args.push("30".into());
    }
    let mut child = Command::new(ffmpeg)
        .args(args)
        .args([
            "-i", &format!("{camera}:{microphone}"),
            "-c:v", "h264_videotoolbox",
            "-b:v", "6M",
            "-pix_fmt", "yuv420p",
            "-c:a", "aac",
            "-b:a", "192k",
            // Fragmented MP4: the index is written incrementally, so a
            // crash/kill mid-take loses nothing already on disk (the reason
            // OBS records MKV — this is the WebKit-playable equivalent).
            // finish() normalizes takes into a clean faststart MP4.
            "-movflags", "+frag_keyframe+empty_moov",
            "-progress", "pipe:1",
            &out.to_string_lossy(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| CoreError::Unavailable(format!("ffmpeg capture: {e}")))?;
    if let Some(stdout) = child.stdout.take() {
        crate::progress::stream_progress(
            stdout,
            sink.clone(),
            ProgressTask::Recording,
            None,
            crate::progress::Delimiter::Newline,
            move |line| {
                let us = line.strip_prefix("out_time_us=")?.parse::<f64>().ok()?;
                let s = us / 1_000_000.0;
                Some((0.0, format!("recording… {:.0}:{:02.0}", (s / 60.0).floor(), s % 60.0)))
            },
        );
    }
    Ok(child)
}

/// Gracefully finalize a take ('q' lets ffmpeg write the moov index).
async fn finalize_take(mut child: Child) -> Result<()> {
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"q\n").await;
        let _ = stdin.flush().await;
    }
    match tokio::time::timeout(std::time::Duration::from_secs(10), child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(status)) => Err(CoreError::Other(format!("ffmpeg capture exited with {status}"))),
        Ok(Err(e)) => Err(CoreError::Other(format!("ffmpeg capture: {e}"))),
        Err(_) => {
            let _ = child.start_kill();
            Err(CoreError::Other(
                "capture did not finalize within 10s — killed; the take may be unusable".into(),
            ))
        }
    }
}

/// Start a recording session (take 1). The first run triggers macOS camera
/// and microphone permission prompts attributed to the app. One session at
/// a time.
pub async fn start_camera(sink: &SharedSink, camera: u32, microphone: u32) -> Result<String> {
    {
        let mut guard = active().lock().unwrap();
        if let Some(session) = guard.as_mut() {
            let alive = session
                .child
                .as_mut()
                .map(|c| c.try_wait().ok().flatten().is_none())
                .unwrap_or(true); // paused sessions count as active
            if alive {
                return Err(CoreError::InvalidArg("a recording is already running".into()));
            }
            *guard = None;
        }
    }
    let dir = recordings_dir();
    std::fs::create_dir_all(&dir)?;
    let base = format!("recording-{}", chrono::Local::now().format("%Y%m%d-%H%M%S"));
    let current = dir.join(format!("{base}-take1.mp4"));
    let child = spawn_take(sink, camera, microphone, &current).await?;
    *active().lock().unwrap() = Some(Session {
        child: Some(child),
        started: std::time::Instant::now(),
        finished_s: 0.0,
        takes: vec![],
        current: current.clone(),
        camera,
        microphone,
        base,
    });
    Ok(current.to_string_lossy().into_owned())
}

/// Pause: finalize the current take; the session stays alive for resume().
pub async fn pause() -> Result<RecordingStatus> {
    let (child, elapsed) = {
        let mut guard = active().lock().unwrap();
        let session = guard
            .as_mut()
            .ok_or_else(|| CoreError::InvalidArg("no recording is running".into()))?;
        let child = session
            .child
            .take()
            .ok_or_else(|| CoreError::InvalidArg("already paused".into()))?;
        (child, session.started.elapsed().as_secs_f64())
    };
    let result = finalize_take(child).await;
    let mut guard = active().lock().unwrap();
    if let Some(session) = guard.as_mut() {
        session.finished_s += elapsed;
        let take = session.current.clone();
        if result.is_ok() && take.is_file() {
            session.takes.push(take);
        }
    }
    result?;
    drop(guard);
    Ok(status())
}

/// Resume after pause: a new take file, same devices.
pub async fn resume(sink: &SharedSink) -> Result<RecordingStatus> {
    let (camera, microphone, next) = {
        let guard = active().lock().unwrap();
        let session = guard
            .as_ref()
            .ok_or_else(|| CoreError::InvalidArg("no recording session".into()))?;
        if session.child.is_some() {
            return Err(CoreError::InvalidArg("not paused".into()));
        }
        let n = session.takes.len() + 1 + 1; // finished + the new one
        (
            session.camera,
            session.microphone,
            recordings_dir().join(format!("{}-take{}.mp4", session.base, n)),
        )
    };
    let child = spawn_take(sink, camera, microphone, &next).await?;
    {
        let mut guard = active().lock().unwrap();
        let session = guard
            .as_mut()
            .ok_or_else(|| CoreError::InvalidArg("session vanished during resume".into()))?;
        session.child = Some(child);
        session.current = next;
        session.started = std::time::Instant::now();
    }
    Ok(status())
}

pub fn status() -> RecordingStatus {
    let mut guard = active().lock().unwrap();
    let Some(session) = guard.as_mut() else {
        return RecordingStatus {
            recording: false,
            paused: false,
            take: 0,
            elapsed_s: 0.0,
            total_s: 0.0,
            out_path: None,
        };
    };
    let take = (session.takes.len() + 1) as u32;
    match session.child.as_mut() {
        None => RecordingStatus {
            recording: false,
            paused: true,
            take: session.takes.len() as u32,
            elapsed_s: 0.0,
            total_s: session.finished_s,
            out_path: Some(session.current.to_string_lossy().into_owned()),
        },
        Some(child) => match child.try_wait() {
            Ok(None) => {
                let elapsed = session.started.elapsed().as_secs_f64();
                RecordingStatus {
                    recording: true,
                    paused: false,
                    take,
                    elapsed_s: elapsed,
                    total_s: session.finished_s + elapsed,
                    out_path: Some(session.current.to_string_lossy().into_owned()),
                }
            }
            _ => {
                let path = session.current.to_string_lossy().into_owned();
                *guard = None;
                RecordingStatus {
                    recording: false,
                    paused: false,
                    take: 0,
                    elapsed_s: 0.0,
                    total_s: 0.0,
                    out_path: Some(path),
                }
            }
        },
    }
}

/// Finish the session: finalize the current take (if recording), then
/// stream-copy concat every take into ONE file. Single-take sessions just
/// return the take. Take files are kept in the library either way.
pub async fn stop() -> Result<String> {
    let session_exists = active().lock().unwrap().is_some();
    if !session_exists {
        return Err(CoreError::InvalidArg("no recording is running".into()));
    }
    // Finalize the in-flight take (no-op when paused).
    let recording = active().lock().unwrap().as_ref().map(|s| s.child.is_some()).unwrap_or(false);
    if recording {
        pause().await?;
    }
    let (takes, base) = {
        let mut guard = active().lock().unwrap();
        let session = guard.take().ok_or_else(|| CoreError::InvalidArg("session vanished".into()))?;
        (session.takes, session.base)
    };
    match takes.len() {
        0 => Err(CoreError::Other("capture produced no usable takes".into())),
        // Single take still goes through concat: it rewrites the fragmented
        // container into a normal faststart MP4 (instant, stream copy).
        _ => {
            let out = recordings_dir().join(format!("{base}.mp4"));
            concat(&takes, &out).await?;
            Ok(out.to_string_lossy().into_owned())
        }
    }
}

/// Lossless concat (`-f concat -c copy`) — instant, and safe because every
/// input came from the same encoder settings. For library combines we
/// verify codec/dimensions first.
async fn concat(inputs: &[PathBuf], out: &PathBuf) -> Result<()> {
    let ffmpeg = super::video::ffmpeg_path()
        .ok_or_else(|| CoreError::Unavailable("ffmpeg not found".into()))?;
    let list_path = std::env::temp_dir().join(format!(
        "roughcut-concat-{}.txt",
        uuid::Uuid::new_v4().simple()
    ));
    let list: String = inputs
        .iter()
        .map(|p| format!("file '{}'\n", p.to_string_lossy().replace('\'', "'\\''")))
        .collect();
    std::fs::write(&list_path, list)?;
    let result = Command::new(ffmpeg)
        .args([
            "-hide_banner", "-y",
            "-f", "concat",
            "-safe", "0",
            "-i", &list_path.to_string_lossy(),
            "-c", "copy",
            "-movflags", "+faststart",
            &out.to_string_lossy(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| CoreError::Other(format!("concat: {e}")))?;
    let _ = std::fs::remove_file(&list_path);
    if !result.status.success() {
        return Err(CoreError::Other(format!(
            "concat failed: {}",
            String::from_utf8_lossy(&result.stderr).chars().take(400).collect::<String>()
        )));
    }
    Ok(())
}

/// The recordings library: every mp4 in ~/Movies/RoughCut, newest first.
pub async fn list_recordings() -> Result<Vec<RecordingFile>> {
    let dir = recordings_dir();
    let mut files: Vec<(std::time::SystemTime, PathBuf, u64)> = vec![];
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("mp4") {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            files.push((meta.modified().unwrap_or(std::time::UNIX_EPOCH), path, meta.len()));
        }
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files.truncate(50);
    let mut out = vec![];
    for (modified, path, size) in files {
        let duration_s = probe_duration(&path).await.unwrap_or(0.0);
        out.push(RecordingFile {
            name: path.file_stem().and_then(|n| n.to_str()).unwrap_or("recording").to_string(),
            path: path.to_string_lossy().into_owned(),
            size_mb: size as f64 / 1_048_576.0,
            modified: chrono::DateTime::<chrono::Utc>::from(modified).to_rfc3339(),
            duration_s,
        });
    }
    Ok(out)
}

async fn probe_duration(path: &PathBuf) -> Result<f64> {
    let ffprobe = super::video::ffprobe_path()
        .ok_or_else(|| CoreError::Unavailable("ffprobe not found".into()))?;
    let out = Command::new(ffprobe)
        .args([
            "-v", "quiet",
            "-show_entries", "format=duration:stream=codec_name,width,height",
            "-of", "json",
            &path.to_string_lossy(),
        ])
        .output()
        .await
        .map_err(|e| CoreError::Other(format!("ffprobe: {e}")))?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    Ok(v["format"]["duration"].as_str().and_then(|d| d.parse().ok()).unwrap_or(0.0))
}

/// Combine library recordings into one file (stream copy). Inputs must
/// share codec + dimensions — stream-copy concat of mismatched files plays
/// broken, so we refuse instead.
pub async fn combine_recordings(paths: &[String]) -> Result<String> {
    if paths.len() < 2 {
        return Err(CoreError::InvalidArg("pick at least two recordings".into()));
    }
    let ffprobe = super::video::ffprobe_path()
        .ok_or_else(|| CoreError::Unavailable("ffprobe not found".into()))?;
    let mut signature: Option<String> = None;
    for p in paths {
        if !std::path::Path::new(p).is_file() {
            return Err(CoreError::NotFound(format!("file {p}")));
        }
        let out = Command::new(&ffprobe)
            .args([
                "-v", "quiet",
                "-select_streams", "v:0",
                "-show_entries", "stream=codec_name,width,height",
                "-of", "csv=p=0",
                p,
            ])
            .output()
            .await
            .map_err(|e| CoreError::Other(format!("ffprobe: {e}")))?;
        let sig = String::from_utf8_lossy(&out.stdout).trim().to_string();
        match &signature {
            None => signature = Some(sig),
            Some(first) if *first != sig => {
                return Err(CoreError::InvalidArg(format!(
                    "recordings don't match ({first} vs {sig}) — combine needs the same                      codec and resolution"
                )));
            }
            _ => {}
        }
    }
    let inputs: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    let out = recordings_dir().join(format!(
        "combined-{}.mp4",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ));
    concat(&inputs, &out).await?;
    Ok(out.to_string_lossy().into_owned())
}

/// Delete a library recording. Only files inside ~/Movies/RoughCut — this
/// is not a general file-deletion endpoint.
pub fn delete_recording(path: &str) -> Result<()> {
    let target = PathBuf::from(path);
    let dir = recordings_dir().canonicalize().unwrap_or_else(|_| recordings_dir());
    let canonical = target
        .canonicalize()
        .map_err(|_| CoreError::NotFound(format!("file {path}")))?;
    if !canonical.starts_with(&dir) {
        return Err(CoreError::InvalidArg("not a RoughCut recording".into()));
    }
    std::fs::remove_file(&canonical)?;
    Ok(())
}

/// Kill any live capture (app exit hook) — better a truncated file than a
/// zombie ffmpeg holding the camera.
pub fn abort_on_exit() {
    if let Some(mut session) = active().lock().unwrap().take() {
        if let Some(child) = session.child.as_mut() {
            let _ = child.start_kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_devices;

    #[test]
    fn parses_real_avfoundation_listing() {
        let stderr = r#"[AVFoundation indev @ 0x84cc18140] AVFoundation video devices:
[AVFoundation indev @ 0x84cc18140] [0] OBS Virtual Camera
[AVFoundation indev @ 0x84cc18140] [1] MacBook Pro Camera
[AVFoundation indev @ 0x84cc18140] [2] iphone (6) Camera
[AVFoundation indev @ 0x84cc18140] [3] MacBook Pro Desk View Camera
[AVFoundation indev @ 0x84cc18140] [5] Capture screen 0
[AVFoundation indev @ 0x84cc18140] AVFoundation audio devices:
[AVFoundation indev @ 0x84cc18140] [0] iphone (6) Microphone
[AVFoundation indev @ 0x84cc18140] [1] MacBook Pro Microphone
[in#0 @ 0x84cc18000] Error opening input: Input/output error"#;
        let d = parse_devices(stderr);
        assert_eq!(d.cameras.len(), 4);
        assert_eq!(d.cameras[1].name, "MacBook Pro Camera");
        assert_eq!(d.screens.len(), 1);
        assert!(d.screens[0].is_screen);
        assert_eq!(d.screens[0].index, 5);
        assert_eq!(d.microphones.len(), 2);
        assert_eq!(d.microphones[1].index, 1);
    }
}
