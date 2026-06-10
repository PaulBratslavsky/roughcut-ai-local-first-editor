//! Platform-specific work lives behind these traits so the core stays portable
//! (see docs/04-tech-decisions.md). Each trait has a production implementation
//! that shells out to the bundled binary, and a fixture-backed mock used in
//! demo mode and tests.

pub mod inference;
pub mod transcribe;
pub mod video;

pub use inference::{ChatMessage, ChatRequest, ChatResponse, InferenceClient, OpenAiCompatClient, ToolCall};
pub use transcribe::{AutoTranscriber, MockTranscriber, Transcriber, WhisperCli};
pub use video::{AutoVideoEngine, FfmpegCli, MockVideoEngine, VideoEngine};

/// True when the app should run on fixture adapters: either explicitly
/// requested (`FABLE_DEMO=1`) or the media toolchain is missing. Resolved at
/// call time — see `crate::capabilities`.
pub fn demo_mode() -> bool {
    crate::capabilities::probe().demo()
}
