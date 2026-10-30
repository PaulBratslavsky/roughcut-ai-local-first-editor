//! Crash safety on real hardware: a take whose process dies mid-recording
//! (no finalize) must still play up to its last fragment. Capture runs
//! in-process, so the "crash" is a child copy of this example being
//! SIGKILLed while it records.
use roughcut_core::adapters::record;
use roughcut_core::adapters::video::{FfmpegCli, VideoEngine};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    if std::env::var_os("CRASH_SMOKE_CHILD").is_some() {
        let sink: roughcut_core::events::SharedSink = Arc::new(roughcut_core::events::NullSink);
        let d = record::list_devices().await.expect("devices");
        let cam = d.cameras.iter().find(|c| c.name.contains("MacBook")).map(|c| c.index).unwrap_or(0);
        let mic = d.microphones.iter().find(|c| c.name.contains("MacBook")).map(|c| c.index).unwrap_or(0);
        let path = record::start_camera(&sink, cam, mic).await.expect("start");
        println!("{path}");
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        return;
    }
    let mut child = Command::new(std::env::current_exe().unwrap())
        .env("CRASH_SMOKE_CHILD", "1")
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn recorder");
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).expect("take path");
    let path = line.trim().to_string();
    std::thread::sleep(std::time::Duration::from_secs(6));
    // Simulate a crash: SIGKILL, no graceful finalize.
    child.kill().expect("kill");
    let _ = child.wait();
    let media = FfmpegCli.probe(&path).await.expect("orphaned take must still probe");
    println!("orphan: {:.2}s {}x{} {}", media.duration, media.width, media.height, media.codec);
    assert!(media.duration > 1.0, "crash-orphaned take should keep its fragments");
    println!("CRASH-SMOKE-OK");
}
