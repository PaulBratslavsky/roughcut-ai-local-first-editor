use roughcut_core::{tools, Editor};
use roughcut_core::model::ActionSource;
use roughcut_core::events::NullSink;
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let editor = Editor::bootstrap_with_store(
        Box::new(roughcut_core::store::SqliteStore::open_in_memory().unwrap()),
        Arc::new(NullSink),
    ).unwrap();
    editor.require_confirmations_for_tests(false);
    let call = |n: &'static str, a: serde_json::Value| {
        let e = editor.clone();
        async move { tools::dispatch(&e, n, &a, ActionSource::Ui).await.unwrap() }
    };
    let p = call("create_project", json!({"name":"declick","file_path":"/tmp/nle-test-source.mp4"})).await;
    let pid = p["id"].as_str().unwrap().to_string();
    call("transcribe", json!({"project_id": pid})).await;
    call("generate_rough_cut", json!({"project_id": pid})).await;
    let r = call("export", json!({"project_id": pid, "target":"mp4", "out_path":"/tmp/declick-test.mp4"})).await;
    println!("rendered: {}", r["path"]);
}
