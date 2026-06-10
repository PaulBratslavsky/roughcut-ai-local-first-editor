//! Live smoke test of the LOCAL agent loop against a running model server
//! (Ollama / llama-server). Run with:
//!
//!     cargo run -p roughcut-core --example agent_live
//!
//! Uses the demo fixture footage, asks Gemma to cut the weekend tangent, and
//! prints every agent step. Requires the model from preferences
//! (INFERENCE_MODEL / default gemma4:26b) to be available locally.

use roughcut_core::adapters::{MockTranscriber, MockVideoEngine};
use roughcut_core::events::EventSink;
use roughcut_core::model::ActionSource;
use roughcut_core::store::SqliteStore;
use roughcut_core::{tools, Editor};
use serde_json::{json, Value};
use std::sync::Arc;

struct StdoutSink;

impl EventSink for StdoutSink {
    fn emit(&self, event: &str, payload: Value) {
        if event == "agent-step" {
            println!("  [{}] {}", payload["kind"].as_str().unwrap_or("?"), payload);
        }
    }
}

#[tokio::main]
async fn main() {
    let editor = Editor::new(
        Box::new(SqliteStore::open_in_memory().unwrap()),
        Box::new(MockVideoEngine),
        Box::new(MockTranscriber),
        Arc::new(StdoutSink),
        true,
    );

    let project = tools::dispatch(
        &editor,
        "create_project",
        &json!({ "name": "agent-live", "file_path": "/demo/footage.mp4" }),
        ActionSource::Ui,
    )
    .await
    .unwrap();
    let pid = project["id"].as_str().unwrap().to_string();
    tools::dispatch(&editor, "transcribe", &json!({ "project_id": pid }), ActionSource::Ui)
        .await
        .unwrap();
    let before = tools::dispatch(&editor, "get_timeline", &json!({ "project_id": pid }), ActionSource::Ui)
        .await
        .unwrap();
    println!("cuts before: {}", before["cut_count"]);

    let instruction = "Cut the tangent where I ramble about going hiking on the weekend. Only that part.";
    println!("instruction: {instruction}\n--- agent steps ---");
    let outcome = tools::dispatch(
        &editor,
        "apply_instruction",
        &json!({ "project_id": pid, "instruction": instruction }),
        ActionSource::Ui,
    )
    .await
    .unwrap();
    println!("--- done ---\nsummary: {}", outcome["summary"]);

    let after = tools::dispatch(&editor, "get_timeline", &json!({ "project_id": pid }), ActionSource::Ui)
        .await
        .unwrap();
    println!("cuts after: {}", after["cut_count"]);

    // Show whether the hiking sentence is now excluded.
    let transcript = tools::dispatch(&editor, "get_transcript", &json!({ "project_id": pid }), ActionSource::Ui)
        .await
        .unwrap();
    let seg = transcript["segments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["text"].as_str().unwrap_or("").contains("hiking"))
        .unwrap();
    let mid = (seg["start"].as_f64().unwrap() + seg["end"].as_f64().unwrap()) / 2.0;
    let included = after["clips"].as_array().unwrap().iter().any(|c| {
        c["included"] == true
            && c["source_in"].as_f64().unwrap() <= mid
            && mid < c["source_out"].as_f64().unwrap()
    });
    println!("hiking tangent still included? {included}");
}
