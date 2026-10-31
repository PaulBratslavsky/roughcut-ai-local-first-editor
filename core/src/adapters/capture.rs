//! Native capture: camera or display + microphone through AVFoundation,
//! encoded by Apple's hardware H.264 encoder (AVAssetWriter). ADR-0006.
//!
//! This replaced ffmpeg's avfoundation input. ffmpeg locks a camera's
//! configuration before it records and gives up when it can't, so any app
//! holding that lock (OBS with a custom-resolution camera source, for one)
//! made every take die on start. Here the lock is optional: when the camera
//! is free we pick the best mode as before; when another app holds it we
//! share the camera at whatever format it's running, like FaceTime does.
//!
//! One take = one capture session (video + mic), plus a second session for
//! the display in dual capture, and one writer per output file. Every stream
//! carries host-clock timestamps and every writer starts at the same source
//! time (`t0`), so camera, mic and display stay in sync by construction.

#[cfg(not(target_os = "macos"))]
use crate::error::CoreError;
use crate::error::Result;
use std::path::PathBuf;

/// A camera or microphone (`id` = AVCaptureDevice uniqueID).
#[derive(Debug, Clone)]
pub struct Device {
    pub id: String,
    pub name: String,
}

/// An attached display (`id` = CGDirectDisplayID).
#[derive(Debug, Clone)]
pub struct Display {
    pub id: u32,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VideoSource {
    Camera(String),
    Display(u32),
}

/// Everything one take records.
#[derive(Debug, Clone)]
pub struct TakeSpec {
    pub video: VideoSource,
    /// Microphone uniqueID. Its audio lives in the primary file.
    pub microphone: String,
    /// Dual capture: this display ALSO records, video-only, to `screen_path`.
    pub overlay: Option<u32>,
    pub primary_path: PathBuf,
    pub screen_path: Option<PathBuf>,
}

/// A capture mode a camera advertises (one frame-rate range of one format).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mode {
    pub w: u32,
    pub h: u32,
    pub fps_min: f64,
    pub fps_max: f64,
}

/// Best mode for a talking head: true landscape beats square beats portrait
/// (cameras offer square Center-Stage modes whose AREA beats 1080p), then
/// closest to 16:9, then largest. Among equal sizes, the range closest to
/// 30 fps wins — 60 fps doubles file size for no spoken-video gain.
pub fn pick_mode(modes: &[Mode]) -> Option<usize> {
    let key = |m: &Mode| {
        let landscape = (m.w > m.h) as u64;
        let aspect = m.w as f64 / m.h.max(1) as f64;
        // 0 deviation = perfect 16:9; subtract from a big constant so
        // closer-to-16:9 ranks higher inside an integer key.
        let aspect_rank = 1_000_000u64.saturating_sub(((aspect - 16.0 / 9.0).abs() * 100_000.0) as u64);
        (landscape, aspect_rank, m.w as u64 * m.h as u64)
    };
    let best = modes.iter().map(key).max()?;
    modes
        .iter()
        .enumerate()
        .filter(|(_, m)| key(m) == best)
        .min_by(|(_, a), (_, b)| {
            let da = (target_fps(a) - 30.0).abs();
            let db = (target_fps(b) - 30.0).abs();
            da.total_cmp(&db).then(a.fps_max.total_cmp(&b.fps_max))
        })
        .map(|(i, _)| i)
}

/// The frame rate we ask a mode for: 30, clamped into what it supports.
/// (max/min, not `clamp`: a device reporting min > max mustn't panic.)
pub fn target_fps(m: &Mode) -> f64 {
    30.0_f64.max(m.fps_min).min(m.fps_max)
}

/// Encoded size: even dimensions (H.264), scaled down to fit 4K so 5K/6K
/// displays stay inside the encoder's limits.
pub fn encode_size(w: u32, h: u32) -> (u32, u32) {
    let (w, h) = (w.max(2) as f64, h.max(2) as f64);
    let (max_w, max_h) = if w >= h { (3840.0, 2160.0) } else { (2160.0, 3840.0) };
    let scale = (max_w / w).min(max_h / h).min(1.0);
    let even = |v: f64| ((v * scale) as u32 / 2 * 2).max(2);
    (even(w), even(h))
}

/// Target bitrate: bits per pixel per frame at 30 fps, clamped.
pub fn bitrate(w: u32, h: u32, bpp: f64, min: u32, max: u32) -> u32 {
    let raw = w as f64 * h as f64 * 30.0 * bpp;
    (raw as u32).clamp(min, max)
}

/// Where a display sits, for naming it.
#[derive(Debug, Clone, Copy)]
pub struct DisplayGeom {
    pub builtin: bool,
    pub main: bool,
    pub px_w: u32,
    pub px_h: u32,
    /// Bounds in global points (CoreGraphics: y grows downward).
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Human labels: "Built-in display · 3024×1964 · main",
/// "Display 2 · 2560×1440 · left". Position is relative to the main display,
/// which tells two identical monitors apart.
pub fn label_displays(ds: &[DisplayGeom]) -> Vec<String> {
    let main = ds.iter().find(|d| d.main).copied();
    ds.iter()
        .enumerate()
        .map(|(i, d)| {
            let base = if d.builtin { "Built-in display".to_string() } else { format!("Display {}", i + 1) };
            let mut label = format!("{base} · {}×{}", d.px_w, d.px_h);
            if d.main {
                if ds.len() > 1 {
                    label.push_str(" · main");
                }
            } else if let Some(m) = main {
                let dx = (d.x + d.w / 2.0) - (m.x + m.w / 2.0);
                let dy = (d.y + d.h / 2.0) - (m.y + m.h / 2.0);
                let side = if dx.abs() >= dy.abs() {
                    if dx < 0.0 { "left" } else { "right" }
                } else if dy < 0.0 {
                    "above"
                } else {
                    "below"
                };
                label.push_str(" · ");
                label.push_str(side);
            }
            label
        })
        .collect()
}

/// One downscaled display frame for the recorder's preview, RGBA.
#[derive(Debug, Clone)]
pub struct PreviewFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Preview frames are at most this wide (the stage is ~1000 px; a
/// half-resolution picture reads fine and keeps IPC small).
pub const PREVIEW_MAX_WIDTH: u32 = 640;

pub use imp::{cameras, displays, microphones, preview_frame, preview_stop, LiveTake};

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use crate::error::CoreError;
    use block2::RcBlock;
    use dispatch2::{DispatchQueue, DispatchRetained};
    use objc2::rc::{autoreleasepool, Retained};
    use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass, Message};
    use objc2_av_foundation::{
        AVAssetWriter, AVAssetWriterInput, AVAssetWriterStatus, AVAuthorizationStatus,
        AVCaptureAudioDataOutput, AVCaptureAudioDataOutputSampleBufferDelegate, AVCaptureConnection,
        AVCaptureDevice, AVCaptureDeviceDiscoverySession, AVCaptureDeviceInput,
        AVCaptureDevicePosition, AVCaptureInput, AVCaptureOutput, AVCaptureScreenInput,
        AVCaptureSession, AVCaptureSessionErrorKey, AVCaptureSessionRuntimeErrorNotification,
        AVCaptureVideoDataOutput, AVCaptureVideoDataOutputSampleBufferDelegate,
    };
    use objc2_core_media::{CMClock, CMSampleBuffer, CMTime, CMVideoFormatDescriptionGetDimensions};
    use objc2_foundation::{
        NSArray, NSDictionary, NSError, NSNotification, NSNotificationCenter, NSNumber,
        NSOperatingSystemVersion, NSProcessInfo, NSString, NSURL,
    };
    use std::ptr::NonNull;
    use std::sync::{mpsc, Arc, Mutex, MutexGuard};
    use std::time::{Duration, Instant};

    // AVFoundation constant VALUES (checked against the SDK). Literals keep
    // newer-OS symbols out of the binary's link table.
    const MEDIA_VIDEO: &str = "vide";
    const MEDIA_AUDIO: &str = "soun";
    const FILE_MP4: &str = "public.mpeg-4";
    const PIXEL_420V: u32 = 875_704_438; // kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
    const FORMAT_LPCM: u32 = 1_819_304_813; // kAudioFormatLinearPCM
    const FORMAT_AAC: u32 = 1_633_772_320; // kAudioFormatMPEG4AAC

    /// No frames for this long = the source died (unplugged, taken over).
    const STALL: Duration = Duration::from_secs(5);
    /// How long start() waits for every stream to deliver its first sample.
    const FIRST_SAMPLES: Duration = Duration::from_secs(8);
    /// Video frames closer together than this are skipped (≈ 30 fps cap,
    /// with slack so a true 30 fps source never loses frames).
    const MIN_FRAME_GAP: f64 = 0.030;

    fn ns(s: &str) -> Retained<NSString> {
        NSString::from_str(s)
    }

    fn obj<T: Message>(o: Retained<T>) -> Retained<AnyObject> {
        // SAFETY: an upcast to the root object type.
        unsafe { Retained::cast_unchecked(o) }
    }

    fn int(v: i64) -> Retained<AnyObject> {
        obj(NSNumber::numberWithLongLong(v))
    }

    fn float(v: f64) -> Retained<AnyObject> {
        obj(NSNumber::numberWithDouble(v))
    }

    fn boolean(v: bool) -> Retained<AnyObject> {
        obj(NSNumber::numberWithBool(v))
    }

    fn dict(pairs: Vec<(&str, Retained<AnyObject>)>) -> Retained<NSDictionary<NSString, AnyObject>> {
        let keys: Vec<Retained<NSString>> = pairs.iter().map(|(k, _)| ns(k)).collect();
        let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
        let values: Vec<Retained<AnyObject>> = pairs.into_iter().map(|(_, v)| v).collect();
        NSDictionary::from_retained_objects(&key_refs, &values)
    }

    /// Run AVFoundation calls that report misuse by throwing.
    fn guarded<R>(what: &str, f: impl FnOnce() -> R) -> Result<R> {
        objc2::exception::catch(std::panic::AssertUnwindSafe(f)).map_err(|e| {
            let detail = e.map(|e| format!("{e:?}")).unwrap_or_else(|| "unknown error".into());
            CoreError::Other(format!("{what}: {detail}"))
        })
    }

    /// "what: description (domain code; underlying domain code)" — Apple's
    /// descriptions alone are often just "The operation could not be
    /// completed".
    fn nserror(what: &str, e: &NSError) -> String {
        let mut text = format!("{what}: {} ({} {}", e.localizedDescription(), e.domain(), e.code());
        let underlying = e.userInfo().objectForKey(&ns("NSUnderlyingError"));
        if let Some(Ok(u)) = underlying.map(|u| u.downcast::<NSError>()) {
            text.push_str(&format!("; underlying {} {}", u.domain(), u.code()));
        }
        text.push(')');
        text
    }

    fn unavailable(msg: impl Into<String>) -> CoreError {
        CoreError::Unavailable(msg.into())
    }

    // ---- devices -------------------------------------------------------

    fn os_at_least(major: isize) -> bool {
        NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(NSOperatingSystemVersion {
            majorVersion: major,
            minorVersion: 0,
            patchVersion: 0,
        })
    }

    fn discover(types: &[&str], media: &str) -> Vec<Device> {
        autoreleasepool(|_| {
            let types: Vec<Retained<NSString>> = types.iter().map(|t| ns(t)).collect();
            let types = NSArray::from_retained_slice(&types);
            let media = ns(media);
            let devices = guarded("device discovery", || unsafe {
                AVCaptureDeviceDiscoverySession::discoverySessionWithDeviceTypes_mediaType_position(
                    &types,
                    Some(&*media),
                    AVCaptureDevicePosition::Unspecified,
                )
                .devices()
                .to_vec()
            })
            .unwrap_or_default();
            devices
                .iter()
                .map(|d| unsafe {
                    Device { id: d.uniqueID().to_string(), name: d.localizedName().to_string() }
                })
                .collect()
        })
    }

    pub fn cameras() -> Vec<Device> {
        let types: &[&str] = if os_at_least(14) {
            &[
                "AVCaptureDeviceTypeBuiltInWideAngleCamera",
                "AVCaptureDeviceTypeContinuityCamera",
                "AVCaptureDeviceTypeDeskViewCamera",
                "AVCaptureDeviceTypeExternal",
            ]
        } else if os_at_least(13) {
            &[
                "AVCaptureDeviceTypeBuiltInWideAngleCamera",
                "AVCaptureDeviceTypeDeskViewCamera",
                "AVCaptureDeviceTypeExternalUnknown",
            ]
        } else {
            &["AVCaptureDeviceTypeBuiltInWideAngleCamera", "AVCaptureDeviceTypeExternalUnknown"]
        };
        discover(types, MEDIA_VIDEO)
    }

    pub fn microphones() -> Vec<Device> {
        let types: &[&str] = if os_at_least(14) {
            &["AVCaptureDeviceTypeMicrophone"]
        } else {
            &["AVCaptureDeviceTypeBuiltInMicrophone", "AVCaptureDeviceTypeExternalUnknown"]
        };
        discover(types, MEDIA_AUDIO)
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CGPoint {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CGSize {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
        fn CGMainDisplayID() -> u32;
        fn CGDisplayIsBuiltin(display: u32) -> u32;
        fn CGDisplayBounds(display: u32) -> CGRect;
        fn CGDisplayCopyDisplayMode(display: u32) -> *mut std::ffi::c_void;
        fn CGDisplayModeGetPixelWidth(mode: *mut std::ffi::c_void) -> usize;
        fn CGDisplayModeGetPixelHeight(mode: *mut std::ffi::c_void) -> usize;
        fn CGDisplayModeRelease(mode: *mut std::ffi::c_void);
        fn CGPreflightScreenCaptureAccess() -> bool;
    }

    pub fn displays() -> Vec<Display> {
        let mut ids = [0u32; 16];
        let mut count = 0u32;
        // SAFETY: the buffer holds `ids.len()` entries.
        if unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) } != 0 {
            return vec![];
        }
        let ids = &ids[..count as usize];
        let main = unsafe { CGMainDisplayID() };
        let geoms: Vec<DisplayGeom> = ids
            .iter()
            .map(|&id| unsafe {
                let bounds = CGDisplayBounds(id);
                let mode = CGDisplayCopyDisplayMode(id);
                let (px_w, px_h) = if mode.is_null() {
                    (bounds.size.width as u32, bounds.size.height as u32)
                } else {
                    let px = (CGDisplayModeGetPixelWidth(mode) as u32, CGDisplayModeGetPixelHeight(mode) as u32);
                    CGDisplayModeRelease(mode);
                    px
                };
                DisplayGeom {
                    builtin: CGDisplayIsBuiltin(id) != 0,
                    main: id == main,
                    px_w,
                    px_h,
                    x: bounds.origin.x,
                    y: bounds.origin.y,
                    w: bounds.size.width,
                    h: bounds.size.height,
                }
            })
            .collect();
        ids.iter()
            .zip(label_displays(&geoms))
            .map(|(&id, name)| Display { id, name })
            .collect()
    }

    /// Ask for camera/mic access (prompting once if macOS hasn't asked yet).
    fn ensure_access(media: &str, what: &str, pane: &str) -> Result<()> {
        let media = ns(media);
        let denied = || {
            unavailable(format!(
                "macOS isn't letting RoughCut use the {what}. Turn it on in System Settings ▸ \
                 Privacy & Security ▸ {pane}, then try again."
            ))
        };
        match unsafe { AVCaptureDevice::authorizationStatusForMediaType(&media) } {
            AVAuthorizationStatus::Authorized => Ok(()),
            AVAuthorizationStatus::NotDetermined => {
                let (tx, rx) = mpsc::channel::<bool>();
                let block = RcBlock::new(move |granted: Bool| {
                    let _ = tx.send(granted.as_bool());
                });
                unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(&media, &block) };
                match rx.recv_timeout(Duration::from_secs(120)) {
                    Ok(true) => Ok(()),
                    _ => Err(denied()),
                }
            }
            _ => Err(denied()),
        }
    }

    // ---- per-take state --------------------------------------------------

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Stream {
        Video,
        Audio,
        Overlay,
    }

    /// One output file and its writer (created on the first video frame,
    /// once the size is known).
    struct Slot {
        path: PathBuf,
        with_audio: bool,
        /// (bits per pixel per frame, min bps, max bps)
        rate: (f64, u32, u32),
        writer: Option<Retained<AVAssetWriter>>,
        video: Option<Retained<AVAssetWriterInput>>,
        audio: Option<Retained<AVAssetWriterInput>>,
        first: Option<CMTime>,
        last: Option<CMTime>,
        started: bool,
    }

    impl Slot {
        fn new(path: PathBuf, with_audio: bool, rate: (f64, u32, u32)) -> Self {
            Slot { path, with_audio, rate, writer: None, video: None, audio: None, first: None, last: None, started: false }
        }

        fn create(&mut self, sample: &CMSampleBuffer) -> std::result::Result<(), String> {
            let desc = unsafe { sample.format_description() }.ok_or("a video frame arrived without a format")?;
            let dims = unsafe { CMVideoFormatDescriptionGetDimensions(&desc) };
            let (w, h) = encode_size(dims.width.max(2) as u32, dims.height.max(2) as u32);
            let url = NSURL::from_file_path(&self.path).ok_or("the recording path isn't a valid file path")?;
            let writer = unsafe { AVAssetWriter::assetWriterWithURL_fileType_error(&url, &ns(FILE_MP4)) }
                .map_err(|e| nserror("couldn't create the recording file", &e))?;
            unsafe {
                // Fragments every 2 s: a crash mid-take keeps what was written.
                writer.setMovieFragmentInterval(CMTime::new(2, 1));
            }
            let (bpp, min, max) = self.rate;
            let video_settings = dict(vec![
                ("AVVideoCodecKey", obj(ns("avc1"))),
                ("AVVideoWidthKey", int(w as i64)),
                ("AVVideoHeightKey", int(h as i64)),
                (
                    "AVVideoCompressionPropertiesKey",
                    obj(dict(vec![
                        ("AverageBitRate", int(bitrate(w, h, bpp, min, max) as i64)),
                        ("ExpectedFrameRate", int(30)),
                        ("MaxKeyFrameInterval", int(60)),
                        ("ProfileLevel", obj(ns("H264_High_AutoLevel"))),
                    ])),
                ),
            ]);
            self.video = Some(add_writer_input(&writer, MEDIA_VIDEO, &[video_settings])?);
            if self.with_audio {
                // Mono AAC; 192k when the encoder takes it, else 128k.
                let aac = |bits: i64| {
                    dict(vec![
                        ("AVFormatIDKey", int(FORMAT_AAC as i64)),
                        ("AVSampleRateKey", float(48_000.0)),
                        ("AVNumberOfChannelsKey", int(1)),
                        ("AVEncoderBitRateKey", int(bits)),
                    ])
                };
                self.audio = Some(add_writer_input(&writer, MEDIA_AUDIO, &[aac(192_000), aac(128_000)])?);
            }
            self.writer = Some(writer);
            Ok(())
        }

        fn begin(&mut self, t0: CMTime) -> std::result::Result<(), String> {
            let Some(writer) = &self.writer else { return Ok(()) };
            unsafe {
                if !writer.startWriting() {
                    return Err(writer_error(writer, "couldn't start writing the recording"));
                }
                writer.startSessionAtSourceTime(t0);
            }
            self.started = true;
            Ok(())
        }

        fn append(&mut self, stream: Stream, sample: &CMSampleBuffer, pts: CMTime) -> std::result::Result<(), String> {
            // Cap video at ~30 fps: a camera another app runs at 60 fps
            // would otherwise double encoder load and file size.
            if stream != Stream::Audio {
                if let Some(last) = self.last {
                    if seconds(pts) - seconds(last) < MIN_FRAME_GAP {
                        return Ok(());
                    }
                }
            }
            let input = if stream == Stream::Audio { &self.audio } else { &self.video };
            let (Some(writer), Some(input)) = (&self.writer, input) else { return Ok(()) };
            unsafe {
                // Real-time capture: if the encoder is behind, drop the
                // sample rather than queueing (the writer can't take it).
                if !input.isReadyForMoreMediaData() {
                    if debug() {
                        eprintln!("[capture] {stream:?} dropped: input not ready");
                    }
                    return Ok(());
                }
                if !input.appendSampleBuffer(sample) {
                    let what = match stream {
                        Stream::Video => "writing the video failed",
                        Stream::Audio => "writing the audio failed",
                        Stream::Overlay => "writing the screen video failed",
                    };
                    return Err(writer_error(writer, what));
                }
            }
            if stream != Stream::Audio {
                self.last = Some(pts);
            }
            Ok(())
        }

        /// Close the file. Err = nothing usable was written.
        fn finish(&mut self) -> std::result::Result<(), String> {
            let Some(writer) = self.writer.take() else {
                return Err("no video was captured".into());
            };
            if !self.started {
                unsafe { writer.cancelWriting() };
                let _ = std::fs::remove_file(&self.path);
                return Err("no video was captured".into());
            }
            unsafe {
                if let Some(v) = &self.video {
                    v.markAsFinished();
                }
                if let Some(a) = &self.audio {
                    a.markAsFinished();
                }
            }
            let (tx, rx) = mpsc::channel::<()>();
            let block = RcBlock::new(move || {
                let _ = tx.send(());
            });
            unsafe { writer.finishWritingWithCompletionHandler(&block) };
            if rx.recv_timeout(Duration::from_secs(30)).is_err() {
                return Err("the recording file didn't finish writing within 30 s".into());
            }
            match unsafe { writer.status() } {
                AVAssetWriterStatus::Completed => Ok(()),
                _ => Err(writer_error(&writer, "finishing the recording failed")),
            }
        }

        fn cancel(&mut self) {
            if let Some(writer) = self.writer.take() {
                if unsafe { writer.status() } == AVAssetWriterStatus::Writing {
                    unsafe { writer.cancelWriting() };
                }
            }
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn add_writer_input(
        writer: &AVAssetWriter,
        media: &str,
        candidates: &[Retained<NSDictionary<NSString, AnyObject>>],
    ) -> std::result::Result<Retained<AVAssetWriterInput>, String> {
        let media_ns = ns(media);
        for settings in candidates {
            if !unsafe { writer.canApplyOutputSettings_forMediaType(Some(&**settings), &media_ns) } {
                continue;
            }
            let input = guarded("encoder setup", || unsafe {
                AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(&media_ns, Some(&**settings))
            })
            .map_err(|e| e.to_string())?;
            unsafe {
                input.setExpectsMediaDataInRealTime(true);
                if !writer.canAddInput(&input) {
                    continue;
                }
                writer.addInput(&input);
            }
            return Ok(input);
        }
        Err(format!(
            "the {} encoder rejected every setting we tried",
            if media == MEDIA_AUDIO { "audio" } else { "video" }
        ))
    }

    fn writer_error(writer: &AVAssetWriter, what: &str) -> String {
        match unsafe { writer.error() } {
            Some(e) => nserror(what, &e),
            None => what.to_string(),
        }
    }

    fn before(a: CMTime, b: CMTime) -> bool {
        unsafe { a.compare(b) < 0 }
    }

    fn seconds(t: CMTime) -> f64 {
        unsafe { t.seconds() }
    }

    /// `ROUGHCUT_CAPTURE_DEBUG=1`: trace sample timestamps and drops to
    /// stderr (read once).
    fn debug() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("ROUGHCUT_CAPTURE_DEBUG").is_some())
    }

    /// Capture timestamps are on the host clock. Some shared cameras hand
    /// over a first frame stamped 0 (seen with a UVC camera OBS was using);
    /// taking it as the start would put the real frames ~uptime later and
    /// stall the writer on empty fragments. Anything more than 10 s from
    /// "now" is not a real capture time.
    fn plausible(pts: CMTime) -> bool {
        const VALID: u32 = 1; // kCMTimeFlags_Valid
        if pts.flags.0 & VALID == 0 {
            return false;
        }
        let now = unsafe { CMClock::host_time_clock().time() };
        (seconds(pts) - seconds(now)).abs() < 10.0
    }

    struct State {
        stopping: bool,
        failure: Option<String>,
        /// The shared start time every writer begins at.
        t0: Option<CMTime>,
        primary: Slot,
        overlay: Option<Slot>,
        audio_seen: bool,
        last_audio: Option<Instant>,
        last_video: Option<Instant>,
        debug_seen: std::collections::HashMap<String, u64>,
    }

    impl State {
        fn handle(&mut self, stream: Stream, sample: &CMSampleBuffer) -> std::result::Result<(), String> {
            let pts = unsafe { sample.presentation_time_stamp() };
            if !plausible(pts) {
                return Ok(());
            }
            if debug() {
                let n = self.debug_seen.entry(format!("{stream:?}")).or_insert(0);
                *n += 1;
                if *n <= 4 || *n % 60 == 0 {
                    eprintln!(
                        "[capture] {stream:?} #{n} pts={}/{} ({:.4}s) epoch={} t0={:?}",
                        { pts.value }, { pts.timescale }, seconds(pts), { pts.epoch }, self.t0.map(seconds)
                    );
                }
            }
            match stream {
                Stream::Audio => {
                    self.audio_seen = true;
                    self.last_audio = Some(Instant::now());
                }
                Stream::Video | Stream::Overlay => {
                    if stream == Stream::Video {
                        self.last_video = Some(Instant::now());
                    }
                    let Some(slot) = self.slot(stream) else { return Ok(()) };
                    if slot.writer.is_none() {
                        slot.create(sample)?;
                    }
                    if slot.first.is_none() {
                        slot.first = Some(pts);
                    }
                    self.try_begin()?;
                }
            }
            let Some(t0) = self.t0 else { return Ok(()) };
            if before(pts, t0) {
                return Ok(());
            }
            match self.slot(stream) {
                Some(slot) => slot.append(stream, sample, pts),
                None => Ok(()),
            }
        }

        fn slot(&mut self, stream: Stream) -> Option<&mut Slot> {
            match stream {
                Stream::Video | Stream::Audio => Some(&mut self.primary),
                Stream::Overlay => self.overlay.as_mut(),
            }
        }

        /// Once every video stream has a first frame, start all writers at
        /// the LATEST of those times — the moment every source is live.
        fn try_begin(&mut self) -> std::result::Result<(), String> {
            if self.t0.is_some() {
                return Ok(());
            }
            let Some(mut t0) = self.primary.first else { return Ok(()) };
            if let Some(overlay) = &self.overlay {
                let Some(o) = overlay.first else { return Ok(()) };
                if before(t0, o) {
                    t0 = o;
                }
            }
            self.primary.begin(t0)?;
            if let Some(overlay) = self.overlay.as_mut() {
                overlay.begin(t0)?;
            }
            self.t0 = Some(t0);
            Ok(())
        }
    }

    struct Shared(Mutex<State>);

    impl Shared {
        fn lock(&self) -> MutexGuard<'_, State> {
            self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
        }

        fn fail(&self, why: String) {
            let mut st = self.lock();
            if st.failure.is_none() && !st.stopping {
                st.failure = Some(why);
            }
        }

        fn on_sample(&self, stream: Stream, sample: &CMSampleBuffer) {
            let mut st = self.lock();
            if st.stopping || st.failure.is_some() {
                return;
            }
            if let Err(why) = st.handle(stream, sample) {
                st.failure = Some(why);
            }
        }
    }

    // SAFETY: the AVFoundation objects in `State` are only touched while the
    // mutex is held, and AVAssetWriter tolerates calls from any one thread at
    // a time.
    unsafe impl Send for State {}

    /// Where a delegate's samples go.
    enum Target {
        Take(Stream, Arc<Shared>),
        Preview(Arc<PreviewSink>),
    }

    struct DelegateIvars {
        target: Target,
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements and we don't
        // implement Drop.
        #[unsafe(super(NSObject))]
        #[name = "RoughCutCaptureDelegate"]
        #[ivars = DelegateIvars]
        struct CaptureDelegate;

        unsafe impl NSObjectProtocol for CaptureDelegate {}

        unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for CaptureDelegate {
            // Audio outputs call the same selector (declared below).
            #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
            fn did_output(
                &self,
                _output: &AVCaptureOutput,
                sample: &CMSampleBuffer,
                _connection: &AVCaptureConnection,
            ) {
                let target = &self.ivars().target;
                // Never unwind into AVFoundation's queue.
                let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match target {
                    Target::Take(stream, shared) => shared.on_sample(*stream, sample),
                    Target::Preview(sink) => sink.on_sample(sample),
                }));
                if let (Err(_), Target::Take(_, shared)) = (ok, target) {
                    shared.fail("internal error while recording".into());
                }
            }
        }

        unsafe impl AVCaptureAudioDataOutputSampleBufferDelegate for CaptureDelegate {}
    );

    impl CaptureDelegate {
        fn new(target: Target) -> Retained<Self> {
            let this = Self::alloc().set_ivars(DelegateIvars { target });
            unsafe { msg_send![super(this), init] }
        }
    }

    /// A take in progress. `finish()` closes the files; dropping it without
    /// finishing keeps what was written (or deletes it if nothing was).
    pub struct LiveTake {
        sessions: Vec<Retained<AVCaptureSession>>,
        observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
        // Kept alive for the sessions' lifetime.
        _outputs: Vec<Retained<AnyObject>>,
        _delegates: Vec<Retained<CaptureDelegate>>,
        _queues: Vec<DispatchRetained<DispatchQueue>>,
        shared: Arc<Shared>,
        camera_primary: bool,
        done: bool,
        /// Counts this take as live (the preview stays off meanwhile).
        _live: LiveCount,
    }

    /// Takes starting or running. The display preview refuses to run while
    /// any is, so it never competes with a recording for the display.
    static LIVE_TAKES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    struct LiveCount;

    impl LiveCount {
        fn new() -> Self {
            LIVE_TAKES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            LiveCount
        }
    }

    impl Drop for LiveCount {
        fn drop(&mut self) {
            LIVE_TAKES.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    // SAFETY: AVCaptureSession start/stop and notification removal are
    // thread-safe; everything else is only retained here.
    unsafe impl Send for LiveTake {}

    impl LiveTake {
        /// Open the devices, start capturing, and wait until every stream is
        /// delivering. Blocking (camera start-up takes up to a second).
        pub fn start(spec: TakeSpec) -> Result<LiveTake> {
            autoreleasepool(|_| Self::start_inner(spec))
        }

        fn start_inner(spec: TakeSpec) -> Result<LiveTake> {
            let live = LiveCount::new();
            preview_stop();
            let camera_primary = matches!(spec.video, VideoSource::Camera(_));
            if camera_primary {
                ensure_access(MEDIA_VIDEO, "camera", "Camera")?;
            }
            ensure_access(MEDIA_AUDIO, "microphone", "Microphone")?;
            if (!camera_primary || spec.overlay.is_some()) && !unsafe { CGPreflightScreenCaptureAccess() } {
                return Err(unavailable(
                    "macOS hasn't given RoughCut Screen Recording. Turn it on in System Settings ▸ \
                     Privacy & Security ▸ Screen & System Audio Recording, then relaunch RoughCut.",
                ));
            }

            // Camera at ~0.1 bit/pixel (1080p30 ≈ 6 Mbps); displays compress
            // better, so less per pixel but a higher floor for crisp text.
            let camera_rate = (0.1, 4_000_000, 16_000_000);
            let display_rate = (0.06, 6_000_000, 14_000_000);
            let primary_rate = if camera_primary { camera_rate } else { display_rate };
            let state = State {
                stopping: false,
                failure: None,
                t0: None,
                primary: Slot::new(spec.primary_path.clone(), true, primary_rate),
                overlay: match (spec.overlay, &spec.screen_path) {
                    (Some(_), Some(path)) => Some(Slot::new(path.clone(), false, display_rate)),
                    _ => None,
                },
                audio_seen: false,
                last_audio: None,
                last_video: None,
                debug_seen: Default::default(),
            };
            let mut take = LiveTake {
                sessions: vec![],
                observers: vec![],
                _outputs: vec![],
                _delegates: vec![],
                _queues: vec![],
                shared: Arc::new(Shared(Mutex::new(state))),
                camera_primary,
                done: false,
                _live: live,
            };

            // Primary session: the video source + the microphone.
            let session = unsafe { AVCaptureSession::new() };
            let mut camera = None;
            match &spec.video {
                VideoSource::Camera(id) => {
                    let device = unsafe { AVCaptureDevice::deviceWithUniqueID(&ns(id)) }
                        .ok_or_else(|| unavailable("that camera isn't connected any more. Pick another one."))?;
                    let input = unsafe { AVCaptureDeviceInput::deviceInputWithDevice_error(&device) }
                        .map_err(|e| unavailable(nserror("couldn't open the camera", &e)))?;
                    add_input(&session, &input, "camera")?;
                    camera = Some(device);
                }
                VideoSource::Display(display) => {
                    let input = display_input(*display)?;
                    add_input(&session, &input, "display")?;
                }
            }
            let mic = unsafe { AVCaptureDevice::deviceWithUniqueID(&ns(&spec.microphone)) }
                .ok_or_else(|| unavailable("that microphone isn't connected any more. Pick another one."))?;
            let mic_input = unsafe { AVCaptureDeviceInput::deviceInputWithDevice_error(&mic) }
                .map_err(|e| unavailable(nserror("couldn't open the microphone", &e)))?;
            add_input(&session, &mic_input, "microphone")?;
            take.add_video_output(&session, Stream::Video)?;
            take.add_audio_output(&session)?;
            take.watch(&session);
            take.sessions.push(session);

            // Dual capture: the display in its own session, same host clock.
            if let Some(display) = spec.overlay {
                let session = unsafe { AVCaptureSession::new() };
                let input = display_input(display)?;
                add_input(&session, &input, "display")?;
                take.add_video_output(&session, Stream::Overlay)?;
                take.watch(&session);
                take.sessions.push(session);
            }

            // Pick the camera's mode only if we can get the configuration
            // lock. Another app holding it (OBS) is fine: we share the camera
            // at its current format. Like ffmpeg, keep the lock until the
            // session runs so the format isn't reset, then let go.
            let locked = camera.as_deref().and_then(configure_camera);
            for session in &take.sessions {
                guarded("starting capture", || unsafe { session.startRunning() })?;
            }
            if let Some(device) = locked {
                unsafe { device.unlockForConfiguration() };
            }

            let deadline = Instant::now() + FIRST_SAMPLES;
            let outcome = loop {
                {
                    let st = take.shared.lock();
                    if let Some(why) = &st.failure {
                        break Err(CoreError::Other(why.clone()));
                    }
                    if st.t0.is_some() && st.audio_seen {
                        break Ok(());
                    }
                    if Instant::now() > deadline {
                        break Err(unavailable(missing_stream(&st, &spec)));
                    }
                }
                std::thread::sleep(Duration::from_millis(40));
            };
            if let Err(e) = outcome {
                // A take that never got going leaves no files behind.
                take.done = true;
                let _ = take.shutdown(true);
                return Err(e);
            }
            Ok(take)
        }

        fn add_video_output(&mut self, session: &AVCaptureSession, stream: Stream) -> Result<()> {
            let output = unsafe { AVCaptureVideoDataOutput::new() };
            let queue = DispatchQueue::new(&format!("dev.localfirst.roughcut.capture.{stream:?}"), None);
            let delegate = CaptureDelegate::new(Target::Take(stream, self.shared.clone()));
            let settings = dict(vec![("PixelFormatType", int(PIXEL_420V as i64))]);
            guarded("video output setup", || unsafe {
                output.setVideoSettings(Some(&*settings));
                output.setAlwaysDiscardsLateVideoFrames(true);
                output.setSampleBufferDelegate_queue(Some(ProtocolObject::from_ref(&*delegate)), Some(&*queue));
            })?;
            if !unsafe { session.canAddOutput(&output) } {
                return Err(CoreError::Other("couldn't attach the video output".into()));
            }
            unsafe { session.addOutput(&output) };
            self._outputs.push(obj(output));
            self._delegates.push(delegate);
            self._queues.push(queue);
            Ok(())
        }

        fn add_audio_output(&mut self, session: &AVCaptureSession) -> Result<()> {
            let output = unsafe { AVCaptureAudioDataOutput::new() };
            let queue = DispatchQueue::new("dev.localfirst.roughcut.capture.Audio", None);
            let delegate = CaptureDelegate::new(Target::Take(Stream::Audio, self.shared.clone()));
            // Mono 48 kHz float: the MacBook mic array reports THREE
            // channels, and 3-channel AAC plays silent outside ffmpeg.
            let settings = dict(vec![
                ("AVFormatIDKey", int(FORMAT_LPCM as i64)),
                ("AVSampleRateKey", float(48_000.0)),
                ("AVNumberOfChannelsKey", int(1)),
                ("AVLinearPCMBitDepthKey", int(32)),
                ("AVLinearPCMIsFloatKey", boolean(true)),
                ("AVLinearPCMIsBigEndianKey", boolean(false)),
                ("AVLinearPCMIsNonInterleaved", boolean(false)),
            ]);
            guarded("audio output setup", || unsafe {
                output.setAudioSettings(Some(&*settings));
                output.setSampleBufferDelegate_queue(Some(ProtocolObject::from_ref(&*delegate)), Some(&*queue));
            })?;
            if !unsafe { session.canAddOutput(&output) } {
                return Err(CoreError::Other("couldn't attach the audio output".into()));
            }
            unsafe { session.addOutput(&output) };
            self._outputs.push(obj(output));
            self._delegates.push(delegate);
            self._queues.push(queue);
            Ok(())
        }

        /// Runtime errors (device unplugged, media services reset) end the
        /// take with macOS's own explanation.
        fn watch(&mut self, session: &AVCaptureSession) {
            let shared = self.shared.clone();
            let block = RcBlock::new(move |note: NonNull<NSNotification>| {
                let note = unsafe { note.as_ref() };
                shared.fail(format!("macOS stopped the capture: {}", runtime_error_text(note)));
            });
            let object: &AnyObject = session;
            let token = unsafe {
                NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
                    Some(AVCaptureSessionRuntimeErrorNotification),
                    Some(object),
                    None,
                    &block,
                )
            };
            self.observers.push(token);
        }

        /// Why the take died, if it did. A stalled camera or mic counts:
        /// cameras and mics deliver continuously (displays only on change,
        /// so a still screen isn't a stall).
        pub fn failure(&self) -> Option<String> {
            let st = self.shared.lock();
            if let Some(why) = &st.failure {
                return Some(why.clone());
            }
            let stalled = |t: Option<Instant>| t.is_some_and(|t| t.elapsed() > STALL);
            if self.camera_primary && stalled(st.last_video) {
                return Some("the camera stopped sending video (unplugged, or taken over by another app)".into());
            }
            if stalled(st.last_audio) {
                return Some("the microphone stopped sending audio (unplugged?)".into());
            }
            None
        }

        /// Seconds of video written so far.
        pub fn duration_s(&self) -> f64 {
            let st = self.shared.lock();
            match (st.t0, st.primary.last) {
                (Some(t0), Some(last)) => (seconds(last) - seconds(t0)).max(0.0),
                _ => 0.0,
            }
        }

        /// Stop capturing and close the files. Err = the primary file is
        /// unusable; a failed display file is deleted (the take degrades to
        /// camera-only).
        pub fn finish(mut self) -> Result<()> {
            self.done = true;
            self.shutdown(false)
        }

        fn shutdown(&mut self, discard: bool) -> Result<()> {
            autoreleasepool(|_| {
                self.shared.lock().stopping = true;
                for session in &self.sessions {
                    unsafe { session.stopRunning() };
                }
                let center = NSNotificationCenter::defaultCenter();
                for token in self.observers.drain(..) {
                    let token: &AnyObject = token.as_ref();
                    unsafe { center.removeObserver(token) };
                }
                let mut st = self.shared.lock();
                if discard {
                    st.primary.cancel();
                    if let Some(overlay) = st.overlay.as_mut() {
                        overlay.cancel();
                    }
                    return Ok(());
                }
                let primary = st.primary.finish();
                if let Some(overlay) = st.overlay.as_mut() {
                    if overlay.finish().is_err() {
                        let _ = std::fs::remove_file(&overlay.path);
                    }
                }
                primary.map_err(CoreError::Other)
            })
        }
    }

    impl Drop for LiveTake {
        fn drop(&mut self) {
            if !self.done {
                // Keep anything recorded; delete a take that never started.
                let discard = self.shared.lock().t0.is_none();
                let _ = self.shutdown(discard);
            }
        }
    }

    fn add_input(session: &AVCaptureSession, input: &AVCaptureInput, what: &str) -> Result<()> {
        if !unsafe { session.canAddInput(input) } {
            return Err(unavailable(format!(
                "the {what} can't be used right now. Another app may be using it exclusively."
            )));
        }
        unsafe { session.addInput(input) };
        Ok(())
    }

    // ---- display preview -------------------------------------------------

    #[link(name = "CoreMedia", kind = "framework")]
    extern "C" {
        fn CMSampleBufferGetImageBuffer(sample: *const std::ffi::c_void) -> *mut std::ffi::c_void;
    }

    #[link(name = "CoreVideo", kind = "framework")]
    extern "C" {
        fn CVPixelBufferLockBaseAddress(buffer: *mut std::ffi::c_void, flags: u64) -> i32;
        fn CVPixelBufferUnlockBaseAddress(buffer: *mut std::ffi::c_void, flags: u64) -> i32;
        fn CVPixelBufferGetBaseAddress(buffer: *mut std::ffi::c_void) -> *mut u8;
        fn CVPixelBufferGetBytesPerRow(buffer: *mut std::ffi::c_void) -> usize;
        fn CVPixelBufferGetWidth(buffer: *mut std::ffi::c_void) -> usize;
        fn CVPixelBufferGetHeight(buffer: *mut std::ffi::c_void) -> usize;
        fn CVPixelBufferGetPixelFormatType(buffer: *mut std::ffi::c_void) -> u32;
    }

    const PIXEL_BGRA: u32 = 1_111_970_369; // kCVPixelFormatType_32BGRA
    const LOCK_READ_ONLY: u64 = 1; // kCVPixelBufferLock_ReadOnly
    /// The preview stops when nobody has asked for a frame for this long
    /// (the recorder closed without saying so).
    const PREVIEW_IDLE: Duration = Duration::from_secs(3);

    /// Holds the newest frame until the recorder picks it up.
    struct PreviewSink {
        latest: Mutex<Option<PreviewFrame>>,
    }

    impl PreviewSink {
        fn on_sample(&self, sample: &CMSampleBuffer) {
            if debug() {
                eprintln!("[preview] frame at {:.3}s", seconds(unsafe { sample.presentation_time_stamp() }));
            }
            if let Some(frame) = unsafe { rgba_frame(sample) } {
                *self.latest.lock().unwrap_or_else(|p| p.into_inner()) = Some(frame);
            }
        }
    }

    /// Copy a BGRA sample into an RGBA frame no wider than
    /// PREVIEW_MAX_WIDTH. Each output pixel averages four samples spread
    /// over its block — cheap, and text stays readable.
    unsafe fn rgba_frame(sample: &CMSampleBuffer) -> Option<PreviewFrame> {
        let buffer = CMSampleBufferGetImageBuffer(sample as *const CMSampleBuffer as *const std::ffi::c_void);
        if buffer.is_null() || CVPixelBufferGetPixelFormatType(buffer) != PIXEL_BGRA {
            return None;
        }
        if CVPixelBufferLockBaseAddress(buffer, LOCK_READ_ONLY) != 0 {
            return None;
        }
        let (w, h) = (CVPixelBufferGetWidth(buffer), CVPixelBufferGetHeight(buffer));
        let stride = CVPixelBufferGetBytesPerRow(buffer);
        let base = CVPixelBufferGetBaseAddress(buffer);
        let frame = if base.is_null() || w == 0 || h == 0 {
            None
        } else {
            let step = w.div_ceil(PREVIEW_MAX_WIDTH as usize).max(1);
            let (ow, oh) = (w / step, h / step);
            let src = std::slice::from_raw_parts(base, stride * h);
            let half = step / 2;
            let at = |x: usize, y: usize, c: usize| src[y * stride + x * 4 + c] as u32;
            let mut rgba = Vec::with_capacity(ow * oh * 4);
            for y in 0..oh {
                let (y0, y1) = (y * step, y * step + half);
                for x in 0..ow {
                    let (x0, x1) = (x * step, x * step + half);
                    let avg = |c| ((at(x0, y0, c) + at(x1, y0, c) + at(x0, y1, c) + at(x1, y1, c)) / 4) as u8;
                    rgba.extend_from_slice(&[avg(2), avg(1), avg(0), 255]);
                }
            }
            Some(PreviewFrame { width: ow as u32, height: oh as u32, rgba })
        };
        CVPixelBufferUnlockBaseAddress(buffer, LOCK_READ_ONLY);
        frame
    }

    struct Preview {
        display: u32,
        session: Retained<AVCaptureSession>,
        sink: Arc<PreviewSink>,
        last_poll: Instant,
        _output: Retained<AnyObject>,
        _delegate: Retained<CaptureDelegate>,
        _queue: DispatchRetained<DispatchQueue>,
    }

    // SAFETY: only start/stop are called on the session, which are
    // thread-safe; the rest is retained.
    unsafe impl Send for Preview {}

    impl Preview {
        fn start(display: u32) -> Result<Preview> {
            let session = unsafe { AVCaptureSession::new() };
            let input = display_input(display)?;
            // Full-size frames, downsampled in rgba_frame: with a scale
            // factor set, AVCaptureScreenInput delivers one frame and stops
            // (measured on macOS 26).
            unsafe { input.setMinFrameDuration(CMTime::new(1, 8)) };
            add_input(&session, &input, "display")?;
            let output = unsafe { AVCaptureVideoDataOutput::new() };
            let queue = DispatchQueue::new("dev.localfirst.roughcut.preview", None);
            let sink = Arc::new(PreviewSink { latest: Mutex::new(None) });
            let delegate = CaptureDelegate::new(Target::Preview(sink.clone()));
            let settings = dict(vec![("PixelFormatType", int(PIXEL_BGRA as i64))]);
            guarded("preview output setup", || unsafe {
                output.setVideoSettings(Some(&*settings));
                output.setAlwaysDiscardsLateVideoFrames(true);
                output.setSampleBufferDelegate_queue(Some(ProtocolObject::from_ref(&*delegate)), Some(&*queue));
            })?;
            if !unsafe { session.canAddOutput(&output) } {
                return Err(CoreError::Other("couldn't attach the preview output".into()));
            }
            unsafe { session.addOutput(&output) };
            guarded("starting the display preview", || unsafe { session.startRunning() })?;
            Ok(Preview {
                display,
                session,
                sink,
                last_poll: Instant::now(),
                _output: obj(output),
                _delegate: delegate,
                _queue: queue,
            })
        }

        fn stop(self) {
            unsafe { self.session.stopRunning() };
        }
    }

    fn preview_slot() -> &'static Mutex<Option<Preview>> {
        static SLOT: std::sync::OnceLock<Mutex<Option<Preview>>> = std::sync::OnceLock::new();
        SLOT.get_or_init(Default::default)
    }

    /// The newest preview frame of `display` since the last call (None =
    /// nothing new yet). Starts the preview on first use and switches it
    /// when the display changes. Off while a take is starting or running.
    pub fn preview_frame(display: u32) -> Result<Option<PreviewFrame>> {
        if LIVE_TAKES.load(std::sync::atomic::Ordering::SeqCst) > 0 {
            preview_stop();
            return Ok(None);
        }
        autoreleasepool(|_| {
            let mut slot = preview_slot().lock().unwrap_or_else(|p| p.into_inner());
            if slot.as_ref().is_some_and(|p| p.display != display) {
                if let Some(old) = slot.take() {
                    old.stop();
                }
            }
            if slot.is_none() {
                if !unsafe { CGPreflightScreenCaptureAccess() } {
                    return Err(unavailable("Screen Recording isn't allowed for RoughCut yet"));
                }
                *slot = Some(Preview::start(display)?);
                spawn_preview_watchdog();
            }
            let Some(preview) = slot.as_mut() else { return Ok(None) };
            preview.last_poll = Instant::now();
            let frame = preview.sink.latest.lock().unwrap_or_else(|p| p.into_inner()).take();
            Ok(frame)
        })
    }

    /// Stop the display preview (recorder closed, source switched).
    pub fn preview_stop() {
        let preview = preview_slot().lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(preview) = preview {
            autoreleasepool(|_| preview.stop());
        }
    }

    /// Stops a preview nobody is polling any more. One watchdog at a time;
    /// it exits with the preview.
    fn spawn_preview_watchdog() {
        static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_secs(1));
            let mut slot = preview_slot().lock().unwrap_or_else(|p| p.into_inner());
            let idle = match slot.as_ref() {
                None => true,
                Some(p) => p.last_poll.elapsed() > PREVIEW_IDLE,
            };
            if idle {
                if let Some(preview) = slot.take() {
                    autoreleasepool(|_| preview.stop());
                }
                RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
                return;
            }
        });
    }

    fn display_input(display: u32) -> Result<Retained<AVCaptureScreenInput>> {
        let input = unsafe { AVCaptureScreenInput::initWithDisplayID(AVCaptureScreenInput::alloc(), display) }
            .ok_or_else(|| unavailable("that display isn't connected any more. Pick another one."))?;
        unsafe {
            input.setMinFrameDuration(CMTime::new(1, 30));
            input.setCapturesCursor(true);
            input.setCapturesMouseClicks(false);
        }
        Ok(input)
    }

    /// Lock the camera and set its best mode. None = another app holds the
    /// lock: record at the camera's current format.
    fn configure_camera(device: &AVCaptureDevice) -> Option<Retained<AVCaptureDevice>> {
        unsafe { device.lockForConfiguration() }.ok()?;
        let formats = unsafe { device.formats() }.to_vec();
        let mut modes = vec![];
        let mut picks = vec![];
        for (fi, format) in formats.iter().enumerate() {
            let dims = unsafe { CMVideoFormatDescriptionGetDimensions(&format.formatDescription()) };
            for range in unsafe { format.videoSupportedFrameRateRanges() }.to_vec() {
                let (lo, hi) = unsafe { (range.minFrameRate(), range.maxFrameRate()) };
                modes.push(Mode { w: dims.width.max(0) as u32, h: dims.height.max(0) as u32, fps_min: lo, fps_max: hi });
                picks.push((fi, range));
            }
        }
        if let Some(best) = pick_mode(&modes) {
            let (fi, range) = &picks[best];
            let fps = target_fps(&modes[best]);
            let duration = unsafe {
                if (fps - range.maxFrameRate()).abs() < 0.01 {
                    range.minFrameDuration()
                } else if (fps - range.minFrameRate()).abs() < 0.01 {
                    range.maxFrameDuration()
                } else {
                    CMTime::new(1000, (fps * 1000.0).round() as i32)
                }
            };
            // A rejected mode leaves the camera at its default: still fine.
            let _ = guarded("camera mode", || unsafe {
                device.setActiveFormat(&formats[*fi]);
                device.setActiveVideoMinFrameDuration(duration);
                device.setActiveVideoMaxFrameDuration(duration);
            });
        }
        Some(device.retain())
    }

    fn missing_stream(st: &State, spec: &TakeSpec) -> String {
        let display_hint = "the display didn't send any frames. Check that RoughCut is allowed under \
                            System Settings ▸ Privacy & Security ▸ Screen & System Audio Recording \
                            (a new grant needs a relaunch).";
        if st.primary.first.is_none() {
            return match spec.video {
                VideoSource::Camera(_) => "the camera didn't send any video. It may be disconnected, or \
                                           held exclusively by another app."
                    .into(),
                VideoSource::Display(_) => display_hint.into(),
            };
        }
        if st.overlay.as_ref().is_some_and(|o| o.first.is_none()) {
            return display_hint.into();
        }
        "the microphone didn't send any audio. Check that it's connected.".into()
    }

    fn runtime_error_text(note: &NSNotification) -> String {
        let Some(info) = note.userInfo() else { return "unknown error".into() };
        let key: &AnyObject = unsafe { AVCaptureSessionErrorKey };
        match info.objectForKey(key).map(|o| o.downcast::<NSError>()) {
            Some(Ok(error)) => error.localizedDescription().to_string(),
            _ => "unknown error".into(),
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;

    pub fn cameras() -> Vec<Device> {
        vec![]
    }

    pub fn microphones() -> Vec<Device> {
        vec![]
    }

    pub fn displays() -> Vec<Display> {
        vec![]
    }

    pub fn preview_frame(_display: u32) -> Result<Option<PreviewFrame>> {
        Err(CoreError::Unavailable("display preview is only available on macOS".into()))
    }

    pub fn preview_stop() {}

    pub struct LiveTake;

    impl LiveTake {
        pub fn start(_spec: TakeSpec) -> Result<LiveTake> {
            Err(CoreError::Unavailable("recording is only available on macOS".into()))
        }

        pub fn failure(&self) -> Option<String> {
            None
        }

        pub fn duration_s(&self) -> f64 {
            0.0
        }

        pub fn finish(self) -> Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(w: u32, h: u32, lo: f64, hi: f64) -> Mode {
        Mode { w, h, fps_min: lo, fps_max: hi }
    }

    #[test]
    fn picks_largest_landscape_at_30fps() {
        // The Y-CAM's real list: fixed-rate ranges at 30/25/60/50 per size.
        let modes = vec![
            m(1920, 1080, 30.00003, 30.00003),
            m(3840, 2160, 30.00003, 30.00003),
            m(3840, 2160, 25.0, 25.0),
            m(3840, 2160, 60.00024, 60.00024),
            m(3840, 2160, 50.0, 50.0),
        ];
        assert_eq!(pick_mode(&modes), Some(1));
    }

    #[test]
    fn prefers_landscape_over_bigger_square() {
        let modes = vec![m(1920, 1920, 1.0, 30.0), m(1920, 1080, 1.0, 30.0), m(1080, 1920, 1.0, 30.0)];
        assert_eq!(pick_mode(&modes), Some(1));
        assert_eq!(target_fps(&modes[1]), 30.0);
    }

    #[test]
    fn ranged_mode_clamps_toward_30() {
        let modes = vec![m(1280, 720, 1.0, 24.0), m(1280, 720, 50.0, 60.0)];
        // 24 is 6 away from 30, 50 is 20 away.
        assert_eq!(pick_mode(&modes), Some(0));
        assert_eq!(target_fps(&modes[0]), 24.0);
        assert_eq!(pick_mode(&[]), None);
    }

    #[test]
    fn encode_size_is_even_and_fits_4k() {
        assert_eq!(encode_size(3024, 1964), (3024, 1964));
        assert_eq!(encode_size(1279, 721), (1278, 720));
        assert_eq!(encode_size(5120, 2880), (3840, 2160));
        assert_eq!(encode_size(6016, 3384), (3840, 2160));
        // 16:10 at 5K: height binds first.
        assert_eq!(encode_size(5120, 3200), (3456, 2160));
        assert_eq!(encode_size(1080, 1920), (1080, 1920));
    }

    #[test]
    fn bitrate_scales_with_size() {
        let cam = |w, h| bitrate(w, h, 0.1, 4_000_000, 16_000_000);
        assert_eq!(cam(1920, 1080), 6_220_800);
        assert_eq!(cam(640, 480), 4_000_000);
        assert_eq!(cam(3840, 2160), 16_000_000);
    }

    #[test]
    fn labels_tell_identical_monitors_apart() {
        let g = |builtin, main, px_w, px_h, x, y, w, h| DisplayGeom { builtin, main, px_w, px_h, x, y, w, h };
        let labels = label_displays(&[
            g(true, true, 3024, 1964, 0.0, 0.0, 1512.0, 982.0),
            g(false, false, 2560, 1440, -2560.0, -400.0, 2560.0, 1440.0),
            g(false, false, 2560, 1440, 1512.0, -400.0, 2560.0, 1440.0),
        ]);
        assert_eq!(
            labels,
            vec![
                "Built-in display · 3024×1964 · main",
                "Display 2 · 2560×1440 · left",
                "Display 3 · 2560×1440 · right",
            ]
        );
        let single = label_displays(&[g(true, true, 3024, 1964, 0.0, 0.0, 1512.0, 982.0)]);
        assert_eq!(single, vec!["Built-in display · 3024×1964"]);
    }
}
