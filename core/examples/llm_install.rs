use roughcut_core::events::NullSink;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let sink: roughcut_core::events::SharedSink = Arc::new(NullSink);
    match roughcut_core::llm_runtime::install_llama_server(&sink).await {
        Ok(p) => println!("installed: {p}"),
        Err(e) => {
            eprintln!("ERR {e}");
            std::process::exit(1);
        }
    }
}
