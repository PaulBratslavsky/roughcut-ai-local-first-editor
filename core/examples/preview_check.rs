//! Display preview on real hardware: frames arrive downscaled, switching
//! displays works, and the preview steps aside while a take records.
//! Run inside a bundle signed as RoughCut (see core/src/adapters/README.md).
use roughcut_core::adapters::record;
use std::sync::Arc;
use std::time::{Duration, Instant};

async fn frames_for(display: u32, secs: f64) -> (usize, Option<(u32, u32)>) {
    let start = Instant::now();
    let mut count = 0;
    let mut size = None;
    while start.elapsed().as_secs_f64() < secs {
        let buf = record::screen_preview(display).await.expect("preview");
        if buf.len() > 8 {
            let w = u32::from_le_bytes(buf[0..4].try_into().unwrap());
            let h = u32::from_le_bytes(buf[4..8].try_into().unwrap());
            assert_eq!(buf.len(), 8 + (w * h * 4) as usize, "payload matches header");
            count += 1;
            size = Some((w, h));
        }
        tokio::time::sleep(Duration::from_millis(125)).await;
    }
    (count, size)
}

#[tokio::main]
async fn main() {
    let d = record::list_devices().await.expect("devices");
    for s in &d.screens {
        println!("screen {} {}", s.index, s.name);
    }
    let (n, size) = frames_for(0, 2.0).await;
    println!("display 0: {n} frames in 2 s, size {size:?}");
    assert!(n >= 5, "expected a steady preview");
    assert!(size.is_some_and(|(w, _)| w <= 640));
    if d.screens.len() > 1 {
        let (n, size) = frames_for(1, 1.5).await;
        println!("display 1: {n} frames in 1.5 s, size {size:?}");
        assert!(n >= 3, "switching displays should keep previewing");
    }
    // A take must switch the preview off.
    let sink: roughcut_core::events::SharedSink = Arc::new(roughcut_core::events::NullSink);
    let mic = d.microphones.iter().find(|m| m.name.contains("MacBook")).map(|m| m.index).unwrap_or(0);
    record::start_capture(&sink, 0, mic, true).await.expect("start take");
    let (n, _) = frames_for(0, 1.0).await;
    println!("during a take: {n} preview frames");
    assert_eq!(n, 0, "no preview while recording");
    record::stop().await.expect("stop take");
    let (n, _) = frames_for(0, 1.5).await;
    println!("after the take: {n} frames");
    assert!(n >= 3, "preview resumes after recording");
    // Optional: dump one frame as PPM to eyeball colors.
    if let Some(path) = std::env::var_os("PREVIEW_DUMP") {
        let mut buf = vec![];
        for _ in 0..20 {
            buf = record::screen_preview(0).await.expect("preview");
            if buf.len() > 8 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(125)).await;
        }
        let w = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        let h = u32::from_le_bytes(buf[4..8].try_into().unwrap());
        let mut ppm = format!("P6 {w} {h} 255\n").into_bytes();
        for px in buf[8..].chunks(4) {
            ppm.extend_from_slice(&px[..3]);
        }
        std::fs::write(path, ppm).expect("write dump");
    }
    record::stop_screen_preview().await;
    println!("PREVIEW-CHECK-OK");
}
