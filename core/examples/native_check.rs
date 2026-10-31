//! Native capture end-to-end on real hardware: camera (shared with any app
//! holding it), pause/resume, dual camera+display, display only. Prints one
//! line per check; exits non-zero on the first failure.
//!
//! Usage: native_check [camera name substring] [mic name substring]
//! Run it inside an app bundle signed as RoughCut so macOS applies RoughCut's
//! camera/mic/Screen Recording grants (see core/src/adapters/README.md).
use roughcut_core::adapters::record;
use roughcut_core::adapters::video::{FfmpegCli, VideoEngine};
use std::sync::Arc;
use std::time::Duration;

fn pick(list: &[record::CaptureDevice], want: Option<&String>) -> u32 {
    want.and_then(|w| list.iter().find(|d| d.name.contains(w.as_str())))
        .or_else(|| list.first())
        .map(|d| d.index)
        .expect("no device")
}

async fn probe(label: &str, path: &str) -> f64 {
    let m = FfmpegCli.probe(path).await.expect("probe");
    println!("{label}: {:.2}s {}x{} {} fps={:.2} audio_hz={}", m.duration, m.width, m.height, m.codec, m.frame_rate, m.audio_sample_rate);
    m.duration
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let sink: roughcut_core::events::SharedSink = Arc::new(roughcut_core::events::NullSink);
    let d = record::list_devices().await.expect("devices");
    for c in &d.cameras {
        println!("camera {} {} [{}]", c.index, c.name, c.id);
    }
    for m in &d.microphones {
        println!("mic    {} {} [{}]", m.index, m.name, m.id);
    }
    for s in &d.screens {
        println!("screen {} {} [{}]", s.index, s.name, s.id);
    }
    let cam = pick(&d.cameras, args.get(1));
    let mic = pick(&d.microphones, args.get(2));
    let screen = d.screens.first().expect("a display").index;

    println!("== A: camera {cam} + mic {mic}, 4s, pause, resume 3s");
    let t = std::time::Instant::now();
    record::start_camera(&sink, cam, mic).await.expect("start A");
    println!("started in {:.2}s", t.elapsed().as_secs_f64());
    tokio::time::sleep(Duration::from_secs(4)).await;
    let st = record::pause().await.expect("pause");
    println!("paused: take={} total={:.2}s error={:?}", st.take, st.total_s, st.error);
    record::resume(&sink).await.expect("resume");
    tokio::time::sleep(Duration::from_secs(3)).await;
    let st = record::status();
    assert!(st.recording && st.error.is_none(), "take 2 should be live: {st:?}");
    let out = record::stop().await.expect("stop A");
    let dur = probe("A final", &out.path).await;
    assert!(dur > 5.5 && dur < 8.0, "A: two takes ≈ 7 s, got {dur}");

    println!("== B: dual camera + display {screen}, 5s");
    record::start_capture_full(&sink, cam, mic, false, Some(screen)).await.expect("start B");
    tokio::time::sleep(Duration::from_secs(5)).await;
    let out = record::stop().await.expect("stop B");
    let cam_s = probe("B camera", &out.path).await;
    let scr_s = probe("B screen", out.screen_path.as_deref().expect("screen file")).await;
    assert!((cam_s - scr_s).abs() < 0.5, "B streams should match: {cam_s} vs {scr_s}");

    println!("== C: display {screen} + mic, 4s");
    record::start_capture(&sink, screen, mic, true).await.expect("start C");
    tokio::time::sleep(Duration::from_secs(4)).await;
    let out = record::stop().await.expect("stop C");
    let dur = probe("C final", &out.path).await;
    assert!(dur > 3.0, "C: ≈ 4 s, got {dur}");
    println!("NATIVE-CHECK-OK");
}
