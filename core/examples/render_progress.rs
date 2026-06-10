//! Verify ffmpeg render progress events stream during an MP4 export.
use roughcut_core::{tools, Editor};
use roughcut_core::model::ActionSource;
use serde_json::{json, Value};
use std::sync::Arc;

struct CountSink(std::sync::Mutex<Vec<String>>);
impl roughcut_core::events::EventSink for CountSink {
    fn emit(&self, event: &str, payload: Value) {
        if event == "progress" && payload["task"] == "export" {
            self.0.lock().unwrap().push(payload["message"].as_str().unwrap_or("").to_string());
        }
    }
}

#[tokio::main]
async fn main() {
    let sink = Arc::new(CountSink(std::sync::Mutex::new(vec![])));
    let editor = Editor::bootstrap_with_store(
        Box::new(roughcut_core::store::SqliteStore::open_in_memory().unwrap()),
        sink.clone(),
    )
    .unwrap();
    editor.require_confirmations_for_tests(false);
    let call = |n: &'static str, a: Value| {
        let e = editor.clone();
        async move { tools::dispatch(&e, n, &a, ActionSource::Ui).await.unwrap() }
    };
    let p = call("create_project", json!({"name":"prog","file_path":"/tmp/nle-test-source.mp4"})).await;
    let pid = p["id"].as_str().unwrap().to_string();
    call("transcribe", json!({"project_id": pid})).await;
    call("generate_rough_cut", json!({"project_id": pid})).await;
    call("export", json!({"project_id": pid, "target":"mp4", "out_path":"/tmp/progress-test.mp4"})).await;
    let msgs = sink.0.lock().unwrap();
    println!("export progress events: {}", msgs.len());
    println!("first: {:?} | last: {:?}", msgs.first(), msgs.last());
    assert!(msgs.len() >= 3, "expected streamed percentages");
    assert_eq!(msgs.last().map(String::as_str), Some("render complete"));
    println!("PASS");
}
