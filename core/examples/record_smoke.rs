//! Real capture smoke test: record N seconds from camera:mic, stop
//! gracefully, probe the result. Usage: record_smoke [secs] [cam] [mic]
use roughcut_core::adapters::record;
use roughcut_core::events::NullSink;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3);
    let devices = record::list_devices().await.expect("list devices");
    println!("cameras: {:?}", devices.cameras.iter().map(|d| (d.index, &d.name)).collect::<Vec<_>>());
    println!("mics:    {:?}", devices.microphones.iter().map(|d| (d.index, &d.name)).collect::<Vec<_>>());
    let cam: u32 = args.get(2).and_then(|s| s.parse().ok())
        .or_else(|| devices.cameras.iter().find(|d| d.name.contains("MacBook")).map(|d| d.index))
        .expect("no camera");
    let mic: u32 = args.get(3).and_then(|s| s.parse().ok())
        .or_else(|| devices.microphones.iter().find(|d| d.name.contains("MacBook")).map(|d| d.index))
        .expect("no mic");
    println!("recording {secs}s from camera {cam}, mic {mic}…");
    let sink: roughcut_core::events::SharedSink = Arc::new(NullSink);
    let path = record::start_camera(&sink, cam, mic).await.expect("start");
    tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
    let st = record::status();
    println!("status mid-recording: recording={} elapsed={:.1}s", st.recording, st.elapsed_s);
    let finished = record::stop().await.expect("stop").path;
    let _ = path;
    let meta = std::fs::metadata(&finished).expect("file");
    println!("file: {finished} ({} KB)", meta.len() / 1024);
    // probe it back through the same engine the app uses
    use roughcut_core::adapters::video::{FfmpegCli, VideoEngine};
    let media = FfmpegCli.probe(&finished).await.expect("probe");
    println!("probed: {:.2}s {}x{} {} fps={}", media.duration, media.width, media.height, media.codec, media.frame_rate);
    // avfoundation warm-up eats ~2s before the first frame — assert net capture
    assert!(media.duration > (secs as f64 - 2.5).max(0.5), "duration too short");
    println!("SMOKE-OK");
}
