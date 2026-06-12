use roughcut_core::adapters::record;
use roughcut_core::adapters::video::{FfmpegCli, VideoEngine};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let sink: roughcut_core::events::SharedSink = Arc::new(roughcut_core::events::NullSink);
    let d = record::list_devices().await.expect("devices");
    let screen = d.screens.first().expect("a display").index;
    let mic = d.microphones.iter().find(|c| c.name.contains("MacBook")).map(|c| c.index).unwrap_or(0);
    println!("recording display {screen} + mic {mic} for 4s…");
    record::start_capture(&sink, screen, mic, true).await.expect("start");
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    let out = record::stop().await.expect("stop");
    let media = FfmpegCli.probe(&out.path).await.expect("probe");
    println!("probed: {:.2}s {}x{} {}", media.duration, media.width, media.height, media.codec);
    assert!(media.duration > 1.0 && media.width > 800);
    println!("SCREEN-SMOKE-OK");
}
