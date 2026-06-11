//! Print the provisioning status (live check against local services) — the
//! same shape the setup screen fetches, runtime details included.
use roughcut_core::Editor;
use roughcut_core::events::NullSink;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let editor = Editor::bootstrap_with_store(
        Box::new(roughcut_core::store::SqliteStore::open(&roughcut_core::store::SqliteStore::default_path()).unwrap()),
        Arc::new(NullSink),
    )
    .unwrap();
    let s = roughcut_core::setup::status(&editor).await;
    println!("{}", serde_json::to_string_pretty(&s).unwrap());
}
