use roughcut_core::adapters::record;
use roughcut_core::adapters::video::{FfmpegCli, VideoEngine};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let sink: roughcut_core::events::SharedSink = Arc::new(roughcut_core::events::NullSink);
    let d = record::list_devices().await.expect("devices");
    let cam = d.cameras.iter().find(|c| c.name.contains("MacBook")).map(|c| c.index).unwrap_or(0);
    let mic = d.microphones.iter().find(|c| c.name.contains("MacBook")).map(|c| c.index).unwrap_or(0);
    let path = record::start_camera(&sink, cam, mic).await.expect("start");
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    // Simulate a crash: SIGKILL the capture process, no graceful finalize.
    let out = std::process::Command::new("pkill").args(["-9", "-f", "avfoundation"]).output().unwrap();
    let _ = out;
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let st = record::status();
    assert!(!st.recording, "status must notice the dead child");
    let media = FfmpegCli.probe(&path).await.expect("orphaned take must still probe");
    println!("orphan: {:.2}s {}x{} {}", media.duration, media.width, media.height, media.codec);
    assert!(media.duration > 0.5, "crash-orphaned take should keep its frames");
    println!("CRASH-SMOKE-OK");
}
