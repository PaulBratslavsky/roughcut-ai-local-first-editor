//! Runs a Shorts scan on a real transcript with the local model, without
//! touching the app's library: a throwaway library file in the temp dir
//! (removed at the end), the video only probed.
//!
//! Export the transcript first (the `get_transcript` tool's JSON), then:
//!
//! `cargo run -p roughcut-core --example shorts_eval -- <video> <transcript.json> <out.json> [model]`
//!
//! Writes the candidates (the `find_clips` result) to `out.json`. The model
//! defaults to the app's default local model; the endpoint is
//! `INFERENCE_ENDPOINT` or the default (Ollama on this machine).

use roughcut_core::adapters::{FfmpegCli, MockTranscriber};
use roughcut_core::events::NullSink;
use roughcut_core::model::Transcript;
use roughcut_core::store::SqliteStore;
use roughcut_core::tools::{self, Caller};
use roughcut_core::Editor;
use serde_json::json;
use std::sync::Arc;
use std::time::Instant;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, video, transcript, out, rest @ ..] = args.as_slice() else {
        eprintln!("usage: shorts_eval <video> <transcript.json> <out.json> [model]");
        std::process::exit(2);
    };
    // A throwaway library file: the editor caches a project once loaded, so
    // the transcript goes in between two editors on the same file.
    let db = std::env::temp_dir().join(format!("roughcut-shorts-eval-{}.db", std::process::id()));
    let open = || {
        let editor = Editor::new(
            Box::new(SqliteStore::open(&db).expect("eval store")),
            Box::new(FfmpegCli),
            Box::new(MockTranscriber),
            Arc::new(NullSink),
            false,
        );
        // Local mode only; never the real key store.
        editor.set_secret_store_for_tests(Arc::new(roughcut_core::secrets::MemoryStore::default()));
        editor
    };
    let setup = open();
    if let Some(model) = rest.first() {
        call(&setup, "set_preferences", json!({ "preferences": { "inference_model": model } })).await;
    }
    let p = call(&setup, "create_project", json!({ "name": "eval", "file_path": video })).await;
    let pid = p["id"].as_str().unwrap().to_string();
    let mut t: Transcript = serde_json::from_str(&std::fs::read_to_string(transcript).expect("read transcript"))
        .expect("a get_transcript JSON");
    t.media_id = p["media"]["id"].as_str().unwrap().parse().unwrap();
    setup.store().save_transcript(pid.parse().unwrap(), &t).expect("save transcript");
    drop(setup);
    let editor = open();

    let started = Instant::now();
    let found = call(&editor, "find_clips", json!({ "project_id": pid, "scan": true, "count": 60 })).await;
    let n = found["candidates"].as_array().map(Vec::len).unwrap_or(0);
    eprintln!("{n} candidates by {} in {:.0}s", found["method"], started.elapsed().as_secs_f64());
    std::fs::write(out, serde_json::to_string_pretty(&found).unwrap()).expect("write out");
    drop(editor);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
    }
}

async fn call(editor: &Editor, tool: &str, args: serde_json::Value) -> serde_json::Value {
    tools::dispatch(editor, tool, &args, Caller::Ui).await.unwrap_or_else(|e| panic!("{tool}: {e}"))
}
