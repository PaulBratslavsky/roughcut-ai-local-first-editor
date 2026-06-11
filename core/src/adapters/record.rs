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
    pub elapsed_s: f64,
    pub out_path: Option<String>,
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
    child: Child,
    started: std::time::Instant,
    out_path: PathBuf,
}

fn active() -> &'static Mutex<Option<Session>> {
    static A: std::sync::OnceLock<Mutex<Option<Session>>> = std::sync::OnceLock::new();
    A.get_or_init(Default::default)
}

fn recordings_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join("Movies/RoughCut")
}

/// Start a camera+mic recording. The first run triggers macOS camera and
/// microphone permission prompts attributed to the app. One session at a
/// time; errors if one is already live.
pub async fn start_camera(sink: &SharedSink, camera: u32, microphone: u32) -> Result<String> {
    {
        let mut guard = active().lock().unwrap();
        if let Some(session) = guard.as_mut() {
            if session.child.try_wait().ok().flatten().is_none() {
                return Err(CoreError::InvalidArg("a recording is already running".into()));
            }
            *guard = None; // previous session died on its own
        }
    }
    let ffmpeg = super::video::ffmpeg_path()
        .ok_or_else(|| CoreError::Unavailable("ffmpeg not found".into()))?;
    let dir = recordings_dir();
    std::fs::create_dir_all(&dir)?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let out_path = dir.join(format!("recording-{stamp}.mp4"));

    let mut child = Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-nostats",
            "-f", "avfoundation",
            "-framerate", "30",
            "-i", &format!("{camera}:{microphone}"),
            "-c:v", "h264_videotoolbox",
            "-b:v", "6M",
            "-pix_fmt", "yuv420p",
            "-c:a", "aac",
            "-b:a", "192k",
            // graceful 'q' on stdin finalizes the file; faststart rewrites
            // the index so the recording is immediately web-playable
            "-movflags", "+faststart",
            "-progress", "pipe:1",
            &out_path.to_string_lossy(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| CoreError::Unavailable(format!("ffmpeg capture: {e}")))?;

    // Elapsed feed for any UI that wants it (the recorder shows its own
    // clock; this keeps the export-style chip honest if one subscribes).
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

    *active().lock().unwrap() = Some(Session {
        child,
        started: std::time::Instant::now(),
        out_path: out_path.clone(),
    });
    Ok(out_path.to_string_lossy().into_owned())
}

pub fn status() -> RecordingStatus {
    let mut guard = active().lock().unwrap();
    match guard.as_mut() {
        None => RecordingStatus { recording: false, elapsed_s: 0.0, out_path: None },
        Some(session) => match session.child.try_wait() {
            Ok(None) => RecordingStatus {
                recording: true,
                elapsed_s: session.started.elapsed().as_secs_f64(),
                out_path: Some(session.out_path.to_string_lossy().into_owned()),
            },
            _ => {
                let path = session.out_path.to_string_lossy().into_owned();
                *guard = None;
                RecordingStatus { recording: false, elapsed_s: 0.0, out_path: Some(path) }
            }
        },
    }
}

/// Stop the active recording gracefully ('q' lets ffmpeg finalize the moov
/// index); escalates to kill after a timeout. Returns the finished file.
pub async fn stop() -> Result<String> {
    let mut session = active()
        .lock()
        .unwrap()
        .take()
        .ok_or_else(|| CoreError::InvalidArg("no recording is running".into()))?;
    if let Some(mut stdin) = session.child.stdin.take() {
        let _ = stdin.write_all(b"q\n").await;
        let _ = stdin.flush().await;
    }
    let finished = tokio::time::timeout(std::time::Duration::from_secs(10), session.child.wait())
        .await;
    match finished {
        Ok(Ok(status)) if status.success() => {}
        Ok(Ok(status)) => {
            return Err(CoreError::Other(format!("ffmpeg capture exited with {status}")));
        }
        Ok(Err(e)) => return Err(CoreError::Other(format!("ffmpeg capture: {e}"))),
        Err(_) => {
            let _ = session.child.start_kill();
            return Err(CoreError::Other(
                "capture did not finalize within 10s — killed; the file may be unusable".into(),
            ));
        }
    }
    if !session.out_path.is_file() {
        return Err(CoreError::Other("capture produced no file".into()));
    }
    Ok(session.out_path.to_string_lossy().into_owned())
}

/// Kill any live capture (app exit hook) — better a truncated file than a
/// zombie ffmpeg holding the camera.
pub fn abort_on_exit() {
    if let Some(mut session) = active().lock().unwrap().take() {
        let _ = session.child.start_kill();
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
