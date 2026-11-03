//! The Editor: owns all state (projects, transcripts, undo/redo), wires the
//! adapters, and implements every operation in the tool registry. The
//! frontend is a view layer over this; MCP clients and the local agent loop
//! call the same methods.
//!
//! One submodule per concern; everything shares the [`Inner`] state defined
//! here:
//! - [`projects`] — project lifecycle + read access
//! - [`media`] — import, transcription, transcript text updates
//! - [`edits`] — THE mutation seam: apply_edit, journal, undo/redo
//! - [`clips`] — saved clips + candidate storage, one scan per project, face crop
//! - [`semantic`] — embedding index + cosine search
//! - [`confirm`] — user approval of externally-driven destructive ops
//! - [`route`] — the LOCAL / HOSTED switch: which inference client a run gets

mod clips;
mod confirm;
mod edits;
mod media;
mod projects;
pub mod route;
mod semantic;

pub use clips::{ClipChanges, ClipScan, ClipSource, FaceCrop};

use crate::adapters::{
    AutoTranscriber, AutoVideoEngine, Embedder, MockTranscriber,
    MockVideoEngine, OllamaEmbedder, Transcriber, VideoEngine,
};
use crate::error::{CoreError, Result};
use crate::events::{NullSink, SharedSink};
use crate::model::*;
use crate::store::{SqliteStore, Store};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// Journals persist per project, so undo survives restarts. The exact
/// inverse/redo clip snapshots live HERE, not on the public EditAction.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct JournalEntry {
    pub action: EditAction,
    /// Applying this restores the pre-op state exactly.
    pub inverse: EditOp,
    /// Applying this restores the post-op state exactly.
    pub redo: EditOp,
}

#[derive(Default)]
pub struct ProjectState {
    pub project: Option<Project>,
    pub transcript: Option<Transcript>,
    pub undo: Vec<JournalEntry>,
    pub redo: Vec<JournalEntry>,
}

pub struct EditOutcome {
    pub action: EditAction,
    pub timeline: Timeline,
    /// For [`EditOp::SplitClip`]: the two resulting clips.
    pub split_clips: Vec<Clip>,
}

pub struct Inner {
    pub store: Box<dyn Store>,
    pub video: Box<dyn VideoEngine>,
    pub transcriber: Box<dyn Transcriber>,
    pub sink: SharedSink,
    pub demo: bool,
    /// Test override; production builds the Ollama embedder from preferences.
    embedder_override: Mutex<Option<Arc<dyn Embedder>>>,
    /// Pending user confirmations for externally-driven destructive ops.
    confirms: Mutex<HashMap<Uuid, tokio::sync::oneshot::Sender<bool>>>,
    /// Off in headless/test contexts (no UI to answer the prompt).
    require_confirm: std::sync::atomic::AtomicBool,
    /// Transient: raw peaks staged by generate_rough_cut for the very next
    /// apply (RoughCut refines silences against real energy). Consumed once.
    pub(crate) rough_cut_peaks: Mutex<Option<Vec<u8>>>,
    /// Projects with a `find_clips` run in flight (one at a time each).
    scans: Mutex<HashSet<Uuid>>,
    /// The LOCAL / HOSTED switch's key and status (see [`route`]).
    pub(crate) route: route::RouteState,
    /// Serialises every preferences load-modify-save, so a slow tool call
    /// can't write back a model switch the user changed meanwhile.
    pub(crate) prefs_write: Mutex<()>,
    state: Mutex<HashMap<Uuid, ProjectState>>,
}

#[derive(Clone)]
pub struct Editor {
    inner: Arc<Inner>,
}

impl Editor {
    pub fn new(
        store: Box<dyn Store>,
        video: Box<dyn VideoEngine>,
        transcriber: Box<dyn Transcriber>,
        sink: SharedSink,
        demo: bool,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                store,
                video,
                transcriber,
                sink,
                demo,
                embedder_override: Mutex::new(None),
                confirms: Mutex::new(HashMap::new()),
                require_confirm: std::sync::atomic::AtomicBool::new(false),
                rough_cut_peaks: Mutex::new(None),
                scans: Mutex::new(HashSet::new()),
                route: route::RouteState::new(Arc::new(crate::secrets::KeyringStore::hosted())),
                prefs_write: Mutex::new(()),
                state: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Production wiring: SQLite at the default path, real adapters when the
    /// toolchain exists, fixture adapters otherwise (demo mode).
    pub fn bootstrap(sink: SharedSink) -> Result<Self> {
        let store = SqliteStore::open(&SqliteStore::default_path())?;
        Self::bootstrap_with_store(Box::new(store), sink)
    }

    pub fn bootstrap_with_store(store: Box<dyn Store>, sink: SharedSink) -> Result<Self> {
        // Auto adapters resolve real-vs-fixture PER CALL via capabilities::probe(),
        // so installing ffmpeg or downloading a model mid-session just works.
        let editor = Self::new(
            store,
            Box::new(AutoVideoEngine),
            Box::new(AutoTranscriber),
            sink,
            false,
        );
        // The app shell has a UI that can answer confirmation prompts.
        // ROUGHCUT_NO_CONFIRM=1 bypasses them (scripted/CI runs).
        let confirm = std::env::var("ROUGHCUT_NO_CONFIRM").map(|v| v != "1").unwrap_or(true);
        editor.inner.require_confirm.store(confirm, std::sync::atomic::Ordering::Relaxed);
        // Trash older than 30 days is gone for good.
        let _ = editor.purge_trash(30);
        // Keys the removed "frontier" path left in plain text.
        crate::secrets::remove_legacy_plaintext();
        Ok(editor)
    }

    pub fn test_instance() -> Self {
        let store = SqliteStore::open_in_memory().expect("in-memory store");
        // Deterministic: no live model server in tests, so the agent loop
        // always exercises its offline path.
        let mut prefs = Preferences::default();
        prefs.inference_endpoint = "http://127.0.0.1:9/v1".into();
        store.save_preferences(&prefs).expect("prefs");
        let editor = Self::new(
            Box::new(store),
            Box::new(MockVideoEngine),
            Box::new(MockTranscriber),
            Arc::new(NullSink),
            true,
        );
        // Tests never read or write the developer's real key store.
        editor.set_secret_store_for_tests(Arc::new(crate::secrets::MemoryStore::default()));
        editor
    }

    /// Dynamic: fixtures forced (tests / FABLE_DEMO) or the media toolchain is
    /// missing right now.
    pub fn demo_mode(&self) -> bool {
        self.inner.demo || crate::capabilities::probe().demo()
    }

    pub fn sink(&self) -> SharedSink {
        self.inner.sink.clone()
    }

    pub fn store(&self) -> &dyn Store {
        self.inner.store.as_ref()
    }

    pub fn video(&self) -> &dyn VideoEngine {
        self.inner.video.as_ref()
    }

    /// Local embedder from preferences (same endpoint as the chat model).
    pub fn embedder(&self) -> Result<Arc<dyn Embedder>> {
        if let Some(e) = self.inner.embedder_override.lock().unwrap().clone() {
            return Ok(e);
        }
        let prefs = self.inner.store.load_preferences()?;
        Ok(Arc::new(OllamaEmbedder::new(&prefs.inference_endpoint, &prefs.embedding_model)))
    }

    pub fn set_embedder_for_tests(&self, embedder: Arc<dyn Embedder>) {
        *self.inner.embedder_override.lock().unwrap() = Some(embedder);
    }

    /// Load a project (and its transcript + persisted journal) into memory if
    /// it isn't already. Every submodule's entry point calls this first.
    fn ensure_loaded(&self, project_id: Uuid) -> Result<()> {
        let mut state = self.inner.state.lock().unwrap();
        let entry = state.entry(project_id).or_default();
        if entry.project.is_none() {
            entry.project = Some(self.inner.store.load_project(project_id)?);
            entry.transcript = self.inner.store.load_transcript(project_id)?;
            // Undo/redo journal survives restarts.
            if let Some(doc) = self.inner.store.get_kv(&format!("journal:{project_id}"))? {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&doc) {
                    entry.undo = journal_entries(&v["undo"]);
                    entry.redo = journal_entries(&v["redo"]);
                }
            }
        }
        Ok(())
    }

    /// Read access to a loaded project (loads it on demand).
    pub fn with_project<T>(
        &self,
        project_id: Uuid,
        f: impl FnOnce(&Project, Option<&Transcript>) -> Result<T>,
    ) -> Result<T> {
        self.ensure_loaded(project_id)?;
        let state = self.inner.state.lock().unwrap();
        let entry = state.get(&project_id).ok_or_else(|| CoreError::NotFound("project".into()))?;
        let project =
            entry.project.as_ref().ok_or_else(|| CoreError::NotFound("project".into()))?;
        f(project, entry.transcript.as_ref())
    }
}

/// Read a persisted undo or redo stack one entry at a time. An entry this
/// build can't read is dropped instead of losing the whole stack. That covers
/// the removed `reorder_clip` op: it never changed the timeline, and every
/// entry's inverse and redo are full snapshots, so the entries around a
/// dropped one still undo and redo exactly.
fn journal_entries(stack: &serde_json::Value) -> Vec<JournalEntry> {
    stack
        .as_array()
        .map(|items| items.iter().filter_map(|e| serde_json::from_value(e.clone()).ok()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_journal_entry_with_a_removed_op_is_dropped_not_the_whole_history() {
        let snapshot = |t: &Timeline| EditOp::SetClips {
            clips: t.clips.clone(),
            global_padding: t.global_padding,
        };
        let before = Timeline::new(60.0);
        let mut after = before.clone();
        after.set_range_included(10.0, 20.0, false, ClipOrigin::Manual);
        let cut = JournalEntry {
            action: EditAction::new(ActionSource::Ui, EditOp::CutRange { start: 10.0, end: 20.0 }),
            inverse: snapshot(&before),
            redo: snapshot(&after),
        };
        let cut = serde_json::to_value(&cut).unwrap();
        // What a journal saved before reorder_clip was removed looks like.
        let mut reorder = cut.clone();
        reorder["action"]["kind"] = "reorder".into();
        reorder["action"]["op"] =
            serde_json::json!({ "type": "reorder_clip", "clip_id": Uuid::new_v4(), "new_order": 0 });
        let saved = serde_json::json!({ "undo": [cut.clone(), reorder, cut], "redo": [] });

        let undo = journal_entries(&saved["undo"]);
        assert_eq!(undo.len(), 2, "only the unreadable entry is dropped");
        assert!(undo.iter().all(|e| matches!(e.action.op, EditOp::CutRange { .. })));
        assert!(journal_entries(&saved["redo"]).is_empty());
        assert!(journal_entries(&serde_json::Value::Null).is_empty());
    }
}
