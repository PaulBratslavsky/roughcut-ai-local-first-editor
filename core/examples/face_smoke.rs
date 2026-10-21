//! Real face-finding smoke test: probe a video, grab frames across a span,
//! run Apple Vision on each, print the faces and the 9:16 crop it would pick.
//! Then the same through the tool registry: a 9:16 clip over the span, and
//! center_clip_on_face on it (real ffmpeg + Vision, in-memory store).
//! Usage: face_smoke <video> [start_s] [end_s]
use roughcut_core::adapters::face;
use roughcut_core::adapters::{AutoVideoEngine, FfmpegCli, MockTranscriber, VideoEngine};
use roughcut_core::events::NullSink;
use roughcut_core::store::SqliteStore;
use roughcut_core::tools::{self, Caller};
use roughcut_core::Editor;
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("usage: face_smoke <video> [start_s] [end_s]");
    let media = FfmpegCli.probe(path).await.expect("probe");
    let start: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let end: f64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(media.duration.min(start + 30.0));
    println!("{}x{} {:.0}s — sampling {start:.1}s–{end:.1}s", media.width, media.height, media.duration);

    let dir = tempfile::tempdir().expect("tempdir");
    let times = face::sample_times(&[(start, end)], 12);
    let mut frames = vec![];
    let mut size = None;
    for (i, t) in times.iter().enumerate() {
        let p = dir.path().join(format!("f{i:02}.png"));
        let t0 = std::time::Instant::now();
        FfmpegCli.extract_frame(&media, *t, 640, &p.to_string_lossy()).await.expect("grab");
        let grab = t0.elapsed();
        size = size.or_else(|| face::png_size(&p).ok());
        let t1 = std::time::Instant::now();
        let faces = face::detect_faces(&p).expect("detect");
        println!(
            "  t={t:7.2}s grab {:>4}ms detect {:>3}ms  faces: {}",
            grab.as_millis(),
            t1.elapsed().as_millis(),
            faces
                .iter()
                .map(|f| format!("cx={:.3} cy={:.3} h={:.3} conf={:.2}", f.cx, f.cy, f.h, f.confidence))
                .collect::<Vec<_>>()
                .join(" | ")
        );
        frames.push(faces);
    }
    let (w, h) = size.expect("frame size");
    match face::crop_x_for_faces(&frames, w, h) {
        Some(x) => println!("frame {w}x{h} → crop_x = {x:.3}"),
        None => println!("frame {w}x{h} → no confident face; crop stays put"),
    }

    let editor = Editor::new(
        Box::new(SqliteStore::open_in_memory().expect("store")),
        Box::new(AutoVideoEngine),
        Box::new(MockTranscriber),
        Arc::new(NullSink),
        false,
    );
    let call = |name: &'static str, args: serde_json::Value| {
        let editor = editor.clone();
        async move { tools::dispatch(&editor, name, &args, Caller::Ui).await.expect(name) }
    };
    let p = call("create_project", json!({ "name": "face smoke", "file_path": path })).await;
    let pid = p["id"].as_str().unwrap().to_string();
    let clip = call("create_clip", json!({ "project_id": pid, "start": start, "end": end, "format": "vertical" })).await;
    let cid = clip["clip"]["id"].as_str().unwrap().to_string();
    // Creating it as 9:16 already started a background face pass.
    println!("created 9:16 clip, crop_x = {}", clip["clip"]["crop_x"]);
    tokio::time::sleep(std::time::Duration::from_secs(8)).await;
    let listed = call("list_clips", json!({ "project_id": pid })).await;
    println!("after the background pass, crop_x = {}", listed["clips"][0]["crop_x"]);
    let t0 = std::time::Instant::now();
    let found = call("center_clip_on_face", json!({ "project_id": pid, "clip_id": cid })).await;
    println!(
        "center_clip_on_face → crop_x {} · face in {}/{} frames · {:.1}s",
        found["crop_x"], found["frames_with_face"], found["frames_sampled"], t0.elapsed().as_secs_f64()
    );
    let listed = call("list_clips", json!({ "project_id": pid })).await;
    println!("saved clip crop_x = {}", listed["clips"][0]["crop_x"]);
}
