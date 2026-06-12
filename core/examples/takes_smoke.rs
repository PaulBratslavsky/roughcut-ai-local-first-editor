//! Takes flow on real hardware: start -> pause -> resume -> stop -> ONE
//! concatenated file that probes clean.
use roughcut_core::adapters::record;
use roughcut_core::adapters::video::{FfmpegCli, VideoEngine};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let sink: roughcut_core::events::SharedSink = Arc::new(roughcut_core::events::NullSink);
    let d = record::list_devices().await.expect("devices");
    let cam = d.cameras.iter().find(|c| c.name.contains("MacBook")).map(|c| c.index).unwrap_or(0);
    let mic = d.microphones.iter().find(|c| c.name.contains("MacBook")).map(|c| c.index).unwrap_or(0);
    println!("take 1 (4s)…");
    record::start_camera(&sink, cam, mic).await.expect("start");
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    let st = record::pause().await.expect("pause");
    println!("paused: take={} total={:.1}s", st.take, st.total_s);
    assert!(st.paused && st.take == 1);
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    println!("take 2 (4s)…");
    let st = record::resume(&sink).await.expect("resume");
    assert!(st.recording && st.take == 2, "resume status: {st:?}");
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    let out = record::stop().await.expect("stop");
    let path = out.path;
    println!("combined: {path}");
    assert!(out.screen_path.is_none(), "camera-only session must not produce a screen file");
    assert!(!path.contains("-take"), "expected the concat file, got a take: {path}");
    let media = FfmpegCli.probe(&path).await.expect("probe");
    println!("probed: {:.2}s {}x{} {}", media.duration, media.width, media.height, media.codec);
    assert!(media.duration > 2.0, "stitched duration too short: {}", media.duration);
    let lib = record::list_recordings().await.expect("library");
    println!("library: {} files, newest: {} ({:.1}s)", lib.len(), lib[0].name, lib[0].duration_s);
    assert!(lib.iter().any(|f| path.ends_with(&format!("{}.mp4", f.name))));
    println!("TAKES-SMOKE-OK");
}
