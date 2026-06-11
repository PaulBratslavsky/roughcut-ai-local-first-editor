//! User confirmation of externally-driven destructive ops (MCP export,
//! delete). The tool handler awaits; the UI answers via a Tauri command.

use super::Editor;
use crate::events::{send, CoreEvent};
use uuid::Uuid;

impl Editor {
    /// Ask the user (via the UI) to approve an externally-driven destructive
    /// op. True when approved; false on deny, 2-minute timeout, or when no
    /// UI is around to answer.
    pub async fn request_confirmation(&self, summary: &str) -> bool {
        if !self.inner.require_confirm.load(std::sync::atomic::Ordering::Relaxed) {
            return true;
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        let id = Uuid::new_v4();
        self.inner.confirms.lock().unwrap().insert(id, tx);
        send(
            &self.inner.sink,
            CoreEvent::ConfirmRequest { id, summary: summary.to_string() },
        );
        match tokio::time::timeout(std::time::Duration::from_secs(120), rx).await {
            Ok(Ok(approved)) => approved,
            _ => {
                self.inner.confirms.lock().unwrap().remove(&id);
                false
            }
        }
    }

    pub fn require_confirmations_for_tests(&self, on: bool) {
        self.inner.require_confirm.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    /// UI answer path (Tauri command).
    pub fn resolve_confirmation(&self, id: Uuid, approved: bool) {
        if let Some(tx) = self.inner.confirms.lock().unwrap().remove(&id) {
            let _ = tx.send(approved);
        }
    }
}
