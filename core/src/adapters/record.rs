//! Recording sessions: takes, pause/resume, finish, and the recordings
//! library. Capture itself is native (`super::capture`, ADR-0006); ffmpeg
//! only stitches takes and evens out mic levels at the end. One active
//! session at a time; liveness is honest (a dead take reports why).

use super::capture::{self, LiveTake, TakeSpec, VideoSource};
use crate::error::{CoreError, Result};
use crate::events::{send, CoreEvent, ProgressTask, SharedSink};
use serde::Serialize;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;
use tokio::process::Command;

#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct CaptureDevice {
    /// Position in its list; what the recorder passes back to start.
    pub index: u32,
    /// Stable identity: AVCaptureDevice uniqueID, or the display ID.
    pub id: String,
    pub name: String,
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
    /// Paused between takes (session alive, nothing capturing).
    pub paused: bool,
    /// 1-based current/most recent take number.
    pub take: u32,
    /// Seconds in the CURRENT take (0 while paused).
    pub elapsed_s: f64,
    /// Seconds across all finished takes + the current one.
    pub total_s: f64,
    pub out_path: Option<String>,
    /// Why the last take stopped on its own (device unplugged, camera taken
    /// over, disk full). None after a normal pause.
    pub error: Option<String>,
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

/// Screen Recording TCC, asked directly (a denied capture just delivers no
/// frames).
#[cfg(target_os = "macos")]
mod tcc {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        pub fn CGPreflightScreenCaptureAccess() -> bool;
        pub fn CGRequestScreenCaptureAccess() -> bool;
    }
}

/// Is Screen Recording granted to this app? (Always true off-macOS.)
pub fn screen_permission() -> bool {
    #[cfg(target_os = "macos")]
    unsafe {
        tcc::CGPreflightScreenCaptureAccess()
    }
    #[cfg(not(target_os = "macos"))]
    true
}

/// Trigger the one-time macOS prompt. Returns the (possibly new) status —
/// macOS only ever shows this prompt ONCE; afterwards it's System Settings,
/// and a grant takes effect on app relaunch.
pub fn request_screen_permission() -> bool {
    #[cfg(target_os = "macos")]
    unsafe {
        tcc::CGRequestScreenCaptureAccess()
    }
    #[cfg(not(target_os = "macos"))]
    true
}

fn to_devices(list: Vec<capture::Device>) -> Vec<CaptureDevice> {
    list.into_iter()
        .enumerate()
        .map(|(i, d)| CaptureDevice { index: i as u32, id: d.id, name: d.name, is_screen: false })
        .collect()
}

/// Enumerate devices. Cheap (no subprocess), but the Setup panel polls, so
/// a listing is reused for a few seconds. The last listing is also what a
/// start request's indexes refer to.
pub async fn list_devices() -> Result<CaptureDevices> {
    {
        let cache = device_cache().lock().unwrap();
        if let Some((at, devices)) = cache.as_ref() {
            if at.elapsed().as_secs() < 5 {
                return Ok(devices.clone());
            }
        }
    }
    let devices = tokio::task::spawn_blocking(|| CaptureDevices {
        cameras: to_devices(capture::cameras()),
        microphones: to_devices(capture::microphones()),
        screens: capture::displays()
            .into_iter()
            .enumerate()
            .map(|(i, d)| CaptureDevice { index: i as u32, id: d.id.to_string(), name: d.name, is_screen: true })
            .collect(),
    })
    .await
    .map_err(|e| CoreError::Other(format!("device listing: {e}")))?;
    *device_cache().lock().unwrap() = Some((std::time::Instant::now(), devices.clone()));
    Ok(devices)
}

#[allow(clippy::type_complexity)]
fn device_cache() -> &'static Mutex<Option<(std::time::Instant, CaptureDevices)>> {
    static C: std::sync::OnceLock<Mutex<Option<(std::time::Instant, CaptureDevices)>>> =
        std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// The devices a session records, resolved once at start (indexes can
/// shift when something is plugged in; IDs don't).
#[derive(Debug, Clone)]
struct Devices {
    video: VideoSource,
    microphone: String,
    overlay: Option<u32>,
}

/// Map the recorder's list indexes to device IDs, using the listing the UI
/// was shown (or a fresh one if there is none).
async fn resolve(video_index: u32, microphone: u32, is_screen: bool, screen_index: Option<u32>) -> Result<Devices> {
    let cached = device_cache().lock().unwrap().as_ref().map(|(_, d)| d.clone());
    let d = match cached {
        Some(d) => d,
        None => list_devices().await?,
    };
    let pick = |list: &[CaptureDevice], index: u32, what: &str| {
        list.iter()
            .find(|x| x.index == index)
            .map(|x| x.id.clone())
            .ok_or_else(|| CoreError::InvalidArg(format!("no {what} #{index} — refresh the device list")))
    };
    let display = |index: u32| -> Result<u32> {
        pick(&d.screens, index, "display")?
            .parse()
            .map_err(|_| CoreError::InvalidArg("bad display id".into()))
    };
    let video = if is_screen {
        VideoSource::Display(display(video_index)?)
    } else {
        VideoSource::Camera(pick(&d.cameras, video_index, "camera")?)
    };
    Ok(Devices {
        video,
        microphone: pick(&d.microphones, microphone, "microphone")?,
        overlay: screen_index.map(display).transpose()?,
    })
}

/// The recorder's live display preview: the newest frame of the display at
/// `display_index` as `[width u32 LE][height u32 LE][RGBA…]`, or empty when
/// there's nothing new (or a take is starting/running — the preview never
/// competes with a recording). Starts the preview on first call; it stops
/// on `stop_screen_preview`, when a take starts, or after 3 s unpolled.
pub async fn screen_preview(display_index: u32) -> Result<Vec<u8>> {
    let cached = device_cache().lock().unwrap().as_ref().map(|(_, d)| d.clone());
    let devices = match cached {
        Some(d) => d,
        None => list_devices().await?,
    };
    let display: u32 = devices
        .screens
        .iter()
        .find(|s| s.index == display_index)
        .and_then(|s| s.id.parse().ok())
        .ok_or_else(|| CoreError::InvalidArg(format!("no display #{display_index}")))?;
    let frame = tokio::task::spawn_blocking(move || capture::preview_frame(display))
        .await
        .map_err(|e| CoreError::Other(format!("display preview: {e}")))??;
    Ok(frame.map(encode_preview).unwrap_or_default())
}

fn encode_preview(frame: capture::PreviewFrame) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + frame.rgba.len());
    out.extend_from_slice(&frame.width.to_le_bytes());
    out.extend_from_slice(&frame.height.to_le_bytes());
    out.extend_from_slice(&frame.rgba);
    out
}

pub async fn stop_screen_preview() {
    let _ = tokio::task::spawn_blocking(capture::preview_stop).await;
}

/// One take's output file(s): camera/primary always, screen when dual.
#[derive(Clone)]
struct Take {
    primary: PathBuf,
    screen: Option<PathBuf>,
}

impl Take {
    fn numbered(base: &str, n: usize, dual: bool) -> Take {
        Take {
            primary: recordings_dir().join(format!("{base}-take{n}.mp4")),
            screen: dual.then(|| recordings_dir().join(format!("{base}-screen-take{n}.mp4"))),
        }
    }
}

struct Session {
    /// The take being captured. None while paused between takes.
    live: Option<LiveTake>,
    /// A pause/stop is finalizing the current take — resume/stop must wait
    /// instead of interleaving.
    transitioning: bool,
    started: std::time::Instant,
    /// Seconds recorded in finished takes.
    finished_s: f64,
    /// Finished takes, in order.
    takes: Vec<Take>,
    /// The take being written by `live`.
    current: Take,
    devices: Devices,
    /// Base name (no extension) all takes share.
    base: String,
    /// Why the most recent take stopped on its own.
    last_error: Option<String>,
}

/// What a finished session hands back.
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct RecordingOutcome {
    pub path: String,
    /// Present for dual (camera+screen) sessions.
    pub screen_path: Option<String>,
}

fn active() -> &'static Mutex<Option<Session>> {
    static A: std::sync::OnceLock<Mutex<Option<Session>>> = std::sync::OnceLock::new();
    A.get_or_init(Default::default)
}

/// Refuse to start a take with under 2 GB free — running out mid-take
/// corrupts nothing (fragmented takes), but the user deserves the warning
/// BEFORE the red light, not after.
fn check_disk_space(dir: &std::path::Path) -> Result<()> {
    let out = std::process::Command::new("df")
        .args(["-Pk", &dir.to_string_lossy()])
        .output();
    let Ok(out) = out else { return Ok(()) }; // best effort
    let text = String::from_utf8_lossy(&out.stdout);
    let Some(line) = text.lines().nth(1) else { return Ok(()) };
    let Some(avail_kb) = line.split_whitespace().nth(3).and_then(|v| v.parse::<u64>().ok())
    else {
        return Ok(());
    };
    let free_gb = avail_kb as f64 / 1_048_576.0;
    if free_gb < 2.0 {
        return Err(CoreError::Unavailable(format!(
            "only {free_gb:.1} GB free on disk — free up space before recording \
             (a minute of capture is roughly 50-150 MB)"
        )));
    }
    Ok(())
}

fn recordings_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join("Movies/RoughCut")
}

/// Start capturing one take. Waits until every stream is delivering, so a
/// device that can't be used fails HERE with a reason, not as a dead take.
async fn start_take(sink: &SharedSink, devices: &Devices, take: &Take) -> Result<LiveTake> {
    let spec = TakeSpec {
        video: devices.video.clone(),
        microphone: devices.microphone.clone(),
        overlay: devices.overlay,
        primary_path: take.primary.clone(),
        screen_path: take.screen.clone(),
    };
    let live = tokio::task::spawn_blocking(move || LiveTake::start(spec))
        .await
        .map_err(|e| CoreError::Other(format!("capture start: {e}")))??;
    send(sink, CoreEvent::progress(ProgressTask::Recording, None, 0.0, "recording…"));
    Ok(live)
}

/// Close a take's files (blocking work, off the async runtime).
async fn finish_take(live: LiveTake) -> Result<()> {
    tokio::task::spawn_blocking(move || live.finish())
        .await
        .map_err(|e| CoreError::Other(format!("capture finish: {e}")))?
}

/// Start a recording session (take 1). First runs trigger the macOS
/// camera+mic (or screen-recording) permission prompts. One session at a
/// time. `screen_index` = dual capture: that display records alongside the
/// camera, two synced files per take.
pub async fn start_capture_full(
    sink: &SharedSink,
    video_index: u32,
    microphone: u32,
    is_screen: bool,
    screen_index: Option<u32>,
) -> Result<String> {
    let stale = {
        let mut guard = active().lock().unwrap();
        match guard.as_ref() {
            // A dead take is replaced; paused or live sessions count as active.
            Some(s) if s.live.as_ref().is_some_and(|l| l.failure().is_some()) => guard.take(),
            Some(_) => return Err(CoreError::InvalidArg("a recording is already running".into())),
            None => None,
        }
    };
    if let Some(mut stale) = stale {
        if let Some(live) = stale.live.take() {
            let _ = finish_take(live).await;
        }
    }
    let devices = resolve(video_index, microphone, is_screen, screen_index).await?;
    let dir = recordings_dir();
    std::fs::create_dir_all(&dir)?;
    check_disk_space(&dir)?;
    let kind = if screen_index.is_some() {
        "presentation"
    } else if is_screen {
        "screen"
    } else {
        "recording"
    };
    let base = format!("{kind}-{}", chrono::Local::now().format("%Y%m%d-%H%M%S"));
    let current = Take::numbered(&base, 1, devices.overlay.is_some());
    let live = start_take(sink, &devices, &current).await?;
    let primary = current.primary.to_string_lossy().into_owned();
    // The lock was released while the take started; another start may have
    // won. (No guard may live across an await: commands must be Send.)
    let rejected = {
        let mut guard = active().lock().unwrap();
        if guard.is_some() {
            Some(live)
        } else {
            *guard = Some(Session {
                live: Some(live),
                transitioning: false,
                started: std::time::Instant::now(),
                finished_s: 0.0,
                takes: vec![],
                current,
                devices,
                base,
                last_error: None,
            });
            None
        }
    };
    if let Some(live) = rejected {
        let _ = finish_take(live).await;
        return Err(CoreError::InvalidArg("a recording is already running".into()));
    }
    Ok(primary)
}

pub async fn start_capture(
    sink: &SharedSink,
    video_index: u32,
    microphone: u32,
    is_screen: bool,
) -> Result<String> {
    start_capture_full(sink, video_index, microphone, is_screen, None).await
}

/// Back-compat shim for camera-only callers.
pub async fn start_camera(sink: &SharedSink, camera: u32, microphone: u32) -> Result<String> {
    start_capture_full(sink, camera, microphone, false, None).await
}

/// Pause: finalize the current take; the session stays alive for resume().
/// Also how a take that died on its own gets closed and kept.
pub async fn pause() -> Result<RecordingStatus> {
    let (live, current) = {
        let mut guard = active().lock().unwrap();
        let session = guard
            .as_mut()
            .ok_or_else(|| CoreError::InvalidArg("no recording is running".into()))?;
        if session.transitioning {
            return Err(CoreError::InvalidArg("a pause/stop is already in flight".into()));
        }
        let live = session
            .live
            .take()
            .ok_or_else(|| CoreError::InvalidArg("already paused".into()))?;
        session.transitioning = true;
        (live, session.current.clone())
    };
    let died = live.failure();
    let recorded = live.duration_s();
    let result = finish_take(live).await;
    let mut guard = active().lock().unwrap();
    if let Some(session) = guard.as_mut() {
        session.finished_s += recorded;
        let mut take = current;
        // A take whose screen file never materialized degrades to
        // camera-only instead of poisoning the final screen concat.
        if take.screen.as_ref().is_some_and(|s| !s.is_file()) {
            take.screen = None;
        }
        if result.is_ok() && take.primary.is_file() {
            session.takes.push(take);
        }
        session.transitioning = false;
        if died.is_some() {
            session.last_error = died.clone();
        }
    }
    drop(guard);
    // A take that died is already reported via status(); closing it is the
    // point of this call, so its finish error isn't news.
    if died.is_none() {
        result?;
    }
    Ok(status())
}

/// Resume after pause (or after a take died): a new take file, same devices.
pub async fn resume(sink: &SharedSink) -> Result<RecordingStatus> {
    let dead = {
        let guard = active().lock().unwrap();
        let session = guard
            .as_ref()
            .ok_or_else(|| CoreError::InvalidArg("no recording session".into()))?;
        if session.transitioning {
            return Err(CoreError::InvalidArg("a pause/stop is still finalizing".into()));
        }
        match &session.live {
            Some(live) if live.failure().is_none() => {
                return Err(CoreError::InvalidArg("not paused".into()))
            }
            Some(_) => true,
            None => false,
        }
    };
    if dead {
        pause().await?;
    }
    let (devices, next) = {
        let guard = active().lock().unwrap();
        let session = guard
            .as_ref()
            .ok_or_else(|| CoreError::InvalidArg("no recording session".into()))?;
        // Name past every take so far, kept or not.
        let n = session.takes.len().max(take_number(&session.current)) + 1;
        (session.devices.clone(), Take::numbered(&session.base, n, session.devices.overlay.is_some()))
    };
    let live = match start_take(sink, &devices, &next).await {
        Ok(live) => live,
        Err(e) => {
            if let Some(session) = active().lock().unwrap().as_mut() {
                session.last_error = Some(e.to_string());
            }
            return Err(e);
        }
    };
    // The lock was released while the take started: another resume (or a
    // stop) may have won the race. Never orphan a running capture.
    let rejected = {
        let mut guard = active().lock().unwrap();
        match guard.as_mut() {
            None => Some((live, "session vanished during resume")),
            Some(s) if s.live.is_some() => Some((live, "not paused")),
            Some(session) => {
                session.live = Some(live);
                session.current = next;
                session.started = std::time::Instant::now();
                session.last_error = None;
                None
            }
        }
    };
    if let Some((live, why)) = rejected {
        let _ = finish_take(live).await;
        return Err(CoreError::InvalidArg(why.into()));
    }
    Ok(status())
}

/// "…-take3.mp4" → 3.
fn take_number(take: &Take) -> usize {
    take.primary
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.rsplit("-take").next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

pub fn status() -> RecordingStatus {
    let guard = active().lock().unwrap();
    let Some(session) = guard.as_ref() else {
        return RecordingStatus {
            recording: false,
            paused: false,
            take: 0,
            elapsed_s: 0.0,
            total_s: 0.0,
            out_path: None,
            error: None,
        };
    };
    let out_path = Some(session.current.primary.to_string_lossy().into_owned());
    match &session.live {
        None => RecordingStatus {
            recording: false,
            paused: true,
            take: session.takes.len() as u32,
            elapsed_s: 0.0,
            total_s: session.finished_s,
            out_path,
            error: session.last_error.clone(),
        },
        Some(live) => match live.failure() {
            None => {
                let elapsed = session.started.elapsed().as_secs_f64();
                RecordingStatus {
                    recording: true,
                    paused: false,
                    take: (session.takes.len() + 1) as u32,
                    elapsed_s: elapsed,
                    total_s: session.finished_s + elapsed,
                    out_path,
                    error: None,
                }
            }
            // The take died (device gone, camera taken over, disk full).
            // What it wrote is kept: present as paused so the user can
            // resume (new take) or finish (stitch what exists). The next
            // pause/resume/stop closes its file.
            Some(why) => RecordingStatus {
                recording: false,
                paused: true,
                take: (session.takes.len() + 1) as u32,
                elapsed_s: 0.0,
                total_s: session.finished_s + live.duration_s(),
                out_path,
                error: Some(why),
            },
        },
    }
}

/// Finish the session: finalize the current take (if recording), then
/// concat every take into ONE file per stream. Take files stay in the
/// library either way.
pub async fn stop() -> Result<RecordingOutcome> {
    let capturing = {
        let guard = active().lock().unwrap();
        let session = guard
            .as_ref()
            .ok_or_else(|| CoreError::InvalidArg("no recording is running".into()))?;
        // Taking the session now would drop the take a pause is closing.
        if session.transitioning {
            return Err(CoreError::InvalidArg("a pause is still finalizing — try again".into()));
        }
        session.live.is_some()
    };
    // Finalize the in-flight take. A finalize failure must NOT abort:
    // finished takes are still worth stitching.
    let mut finalize_note: Option<String> = None;
    if capturing {
        if let Err(e) = pause().await {
            finalize_note = Some(e.to_string());
        }
    }
    let (takes, base, last_error) = {
        let mut guard = active().lock().unwrap();
        let session = guard.take().ok_or_else(|| CoreError::InvalidArg("session vanished".into()))?;
        (session.takes, session.base, session.last_error)
    };
    if takes.is_empty() {
        let note = finalize_note.or(last_error);
        return Err(CoreError::Other(match note {
            Some(note) => format!("nothing usable was recorded ({note})"),
            None => "nothing usable was recorded".into(),
        }));
    }
    // Primary stream (always), mic levels evened out.
    let primaries: Vec<PathBuf> = takes.iter().map(|t| t.primary.clone()).collect();
    let out = recordings_dir().join(format!("{base}.mp4"));
    concat(&primaries, &out, AudioPass::Normalize).await?;
    // Screen stream (dual sessions). A screen-side failure degrades to a
    // camera-only outcome — never discard a good primary over the overlay.
    let screens: Vec<PathBuf> = takes.iter().filter_map(|t| t.screen.clone()).collect();
    let screen_path = if screens.len() == takes.len() && !screens.is_empty() {
        let sout = recordings_dir().join(format!("{base}-screen.mp4"));
        match concat(&screens, &sout, AudioPass::Copy).await {
            Ok(()) => Some(sout.to_string_lossy().into_owned()),
            Err(_) => None,
        }
    } else {
        None
    };
    Ok(RecordingOutcome { path: out.to_string_lossy().into_owned(), screen_path })
}

/// What concat does to the audio track.
#[derive(Debug, Clone, Copy, PartialEq)]
enum AudioPass {
    /// Stream copy (library combines, video-only screen files).
    Copy,
    /// Re-encode through a dynamic normalizer: macOS default input levels
    /// are whisper-quiet (live capture measured 2.4% peak speech), so lift
    /// voice to a usable level, like every screen recorder's mic boost.
    Normalize,
}

const NORMALIZE: &str = "dynaudnorm=f=300:g=11";

/// Concat takes into one file. Same-size takes stream-copy the video
/// (instant); takes of different sizes (another app changed the camera's
/// format between takes) are scaled to the first take's size and
/// re-encoded, since stream-copied mixed sizes play broken.
async fn concat(inputs: &[PathBuf], out: &PathBuf, audio: AudioPass) -> Result<()> {
    let ffmpeg = super::video::ffmpeg_path()
        .ok_or_else(|| CoreError::Unavailable("ffmpeg not found".into()))?;
    let mut sizes = vec![];
    for input in inputs {
        sizes.push(probe_video_size(input).await);
    }
    let first = sizes.first().copied().flatten();
    let uniform = sizes.iter().all(|s| *s == first);
    let has_audio = audio == AudioPass::Normalize;
    let list_path = std::env::temp_dir().join(format!(
        "roughcut-concat-{}.txt",
        uuid::Uuid::new_v4().simple()
    ));
    let args = match (uniform, first) {
        (false, Some((w, h))) => reencode_args(inputs, out, (w, h), has_audio),
        _ => {
            let list: String = inputs
                .iter()
                .map(|p| format!("file '{}'\n", p.to_string_lossy().replace('\'', "'\\''")))
                .collect();
            std::fs::write(&list_path, list)?;
            copy_args(&list_path, out, audio)
        }
    };
    let result = Command::new(ffmpeg)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| CoreError::Other(format!("concat: {e}")));
    let _ = std::fs::remove_file(&list_path);
    let result = result?;
    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        let tail: String = stderr.chars().rev().take(400).collect::<String>().chars().rev().collect();
        return Err(CoreError::Other(format!("concat failed: {tail}")));
    }
    Ok(())
}

/// `-f concat` stream copy, optionally re-encoding audio through the
/// normalizer.
fn copy_args(list: &std::path::Path, out: &std::path::Path, audio: AudioPass) -> Vec<String> {
    let mut args: Vec<String> = ["-hide_banner", "-y", "-f", "concat", "-safe", "0", "-i"]
        .into_iter()
        .map(String::from)
        .collect();
    args.push(list.to_string_lossy().into_owned());
    match audio {
        AudioPass::Copy => args.extend(["-c", "copy"].map(String::from)),
        AudioPass::Normalize => args.extend(
            ["-map", "0:v", "-map", "0:a", "-c:v", "copy", "-af", NORMALIZE, "-ac", "1", "-c:a", "aac", "-b:a", "192k"]
                .map(String::from),
        ),
    }
    args.extend(["-movflags", "+faststart"].map(String::from));
    args.push(out.to_string_lossy().into_owned());
    args
}

/// Mixed-size takes: scale/pad each to `size`, concat, re-encode.
fn reencode_args(inputs: &[PathBuf], out: &std::path::Path, size: (u32, u32), has_audio: bool) -> Vec<String> {
    let (w, h) = size;
    let mut args: Vec<String> = vec!["-hide_banner".into(), "-y".into()];
    for input in inputs {
        args.push("-i".into());
        args.push(input.to_string_lossy().into_owned());
    }
    let mut graph = String::new();
    let mut joined = String::new();
    for i in 0..inputs.len() {
        graph.push_str(&format!(
            "[{i}:v]scale={w}:{h}:force_original_aspect_ratio=decrease,\
             pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30[v{i}];"
        ));
        joined.push_str(&format!("[v{i}]"));
        if has_audio {
            joined.push_str(&format!("[{i}:a]"));
        }
    }
    let n = inputs.len();
    if has_audio {
        graph.push_str(&format!("{joined}concat=n={n}:v=1:a=1[v][a0];[a0]{NORMALIZE}[a]"));
    } else {
        graph.push_str(&format!("{joined}concat=n={n}:v=1:a=0[v]"));
    }
    args.extend(["-filter_complex".into(), graph, "-map".into(), "[v]".into()]);
    if has_audio {
        args.extend(["-map", "[a]", "-ac", "1", "-c:a", "aac", "-b:a", "192k"].map(String::from));
    }
    args.extend(
        ["-c:v", "h264_videotoolbox", "-b:v", "8M", "-pix_fmt", "yuv420p", "-movflags", "+faststart"]
            .map(String::from),
    );
    args.push(out.to_string_lossy().into_owned());
    args
}

async fn probe_video_size(path: &PathBuf) -> Option<(u32, u32)> {
    let ffprobe = super::video::ffprobe_path()?;
    let out = Command::new(ffprobe)
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .await
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (w, h) = text.trim().split_once(',')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
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
                // ALL streams: a video-only file must not pass as a match
                // for camera takes (stream-copy concat would desync), and
                // channel counts must agree too.
                "-show_entries", "stream=codec_type,codec_name,width,height,channels",
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
    concat(&inputs, &out, AudioPass::Copy).await?;
    Ok(out.to_string_lossy().into_owned())
}

/// Delete a library recording. Only files inside ~/Movies/RoughCut — this
/// is not a general file-deletion endpoint.
pub fn delete_recording(path: &str) -> Result<()> {
    let target = PathBuf::from(path);
    let dir = recordings_dir().canonicalize().unwrap_or_else(|_| recordings_dir());
    let Ok(canonical) = target.canonicalize() else {
        return Ok(()); // already gone — deletion is idempotent
    };
    if !canonical.starts_with(&dir) {
        return Err(CoreError::InvalidArg("not a RoughCut recording".into()));
    }
    std::fs::remove_file(&canonical)?;
    Ok(())
}

/// App exit: close any live take properly — its file stays playable.
pub fn abort_on_exit() {
    let session = active().lock().unwrap().take();
    if let Some(mut session) = session {
        if let Some(live) = session.live.take() {
            let _ = live.finish();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_frames_carry_their_size() {
        let out = encode_preview(capture::PreviewFrame { width: 2, height: 1, rgba: vec![1, 2, 3, 4, 5, 6, 7, 8] });
        assert_eq!(&out[..8], &[2, 0, 0, 0, 1, 0, 0, 0]);
        assert_eq!(&out[8..], &[1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn take_numbers_parse() {
        let t = Take::numbered("presentation-20261029-170000", 3, true);
        assert_eq!(take_number(&t), 3);
        assert!(t.screen.unwrap().ends_with("presentation-20261029-170000-screen-take3.mp4"));
        assert!(Take::numbered("recording-x", 1, false).screen.is_none());
    }

    #[test]
    fn copy_concat_normalizes_only_when_asked() {
        let list = std::path::Path::new("/tmp/list.txt");
        let out = std::path::Path::new("/tmp/out.mp4");
        let copy = copy_args(list, out, AudioPass::Copy).join(" ");
        assert!(copy.contains("-c copy"));
        assert!(!copy.contains("dynaudnorm"));
        let norm = copy_args(list, out, AudioPass::Normalize).join(" ");
        assert!(norm.contains("-c:v copy"));
        assert!(norm.contains("-af dynaudnorm=f=300:g=11 -ac 1 -c:a aac"));
        assert!(norm.ends_with("+faststart /tmp/out.mp4"));
    }

    #[test]
    fn mixed_size_takes_are_scaled_and_reencoded() {
        let inputs = vec![PathBuf::from("/a/t1.mp4"), PathBuf::from("/a/t2.mp4")];
        let out = std::path::Path::new("/a/out.mp4");
        let args = reencode_args(&inputs, out, (1920, 1080), true);
        let graph = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        assert!(graph.contains("[1:v]scale=1920:1080:force_original_aspect_ratio=decrease"));
        assert!(graph.contains("[v0][0:a][v1][1:a]concat=n=2:v=1:a=1[v][a0];[a0]dynaudnorm"));
        assert!(args.join(" ").contains("-map [a] -ac 1 -c:a aac"));
        let video_only = reencode_args(&inputs, out, (1920, 1080), false);
        let graph = &video_only[video_only.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        assert!(graph.ends_with("[v0][v1]concat=n=2:v=1:a=0[v]"));
        assert!(!video_only.contains(&"[a]".to_string()));
    }
}
