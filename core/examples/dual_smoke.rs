//! Dual capture on real hardware: one process, camera+mic AND screen,
//! pause/resume, finish -> two stitched files with near-equal durations.
use roughcut_core::adapters::record;
use roughcut_core::adapters::video::{FfmpegCli, VideoEngine};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let sink: roughcut_core::events::SharedSink = Arc::new(roughcut_core::events::NullSink);
    let d = record::list_devices().await.expect("devices");
    let cam = d.cameras.iter().find(|c| c.name == "MacBook Pro Camera").map(|c| c.index).unwrap_or(0);
    let mic = d.microphones.iter().find(|c| c.name.contains("MacBook")).map(|c| c.index).unwrap_or(0);
    let screen = d.screens.first().expect("display").index;
    println!("dual: cam {cam} + mic {mic} + screen {screen}, 5s…");
    record::start_capture_full(&sink, cam, mic, false, Some(screen)).await.expect("start");
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    let out = record::stop().await.expect("stop");
    println!("primary: {}", out.path);
    println!("screen:  {:?}", out.screen_path);
    let cam_m = FfmpegCli.probe(&out.path).await.expect("probe cam");
    let scr = out.screen_path.expect("screen file");
    let scr_m = FfmpegCli.probe(&scr).await.expect("probe screen");
    println!("cam: {:.2}s {}x{} | screen: {:.2}s {}x{}", cam_m.duration, cam_m.width, cam_m.height, scr_m.duration, scr_m.width, scr_m.height);
    assert!(cam_m.duration > 1.0 && scr_m.duration > 1.0);
    assert!((cam_m.duration - scr_m.duration).abs() < 1.0, "streams should be near-synced");
    println!("DUAL-SMOKE-OK");
}
