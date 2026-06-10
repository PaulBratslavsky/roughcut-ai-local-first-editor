//! The Editor: owns all state (projects, transcripts, undo/redo), wires the
//! adapters, and implements every operation in the tool registry. The
//! frontend is a view layer over this; MCP clients and the local agent loop
//! call the same methods.

use crate::adapters::{
    AutoTranscriber, AutoVideoEngine, Embedder, InferenceClient, MockTranscriber,
    MockVideoEngine, OllamaEmbedder, OpenAiCompatClient, Transcriber, VideoEngine,
};
use crate::detect;
use crate::error::{CoreError, Result};
use crate::events::{send, CoreEvent, NullSink, SharedSink};
use crate::model::*;
use crate::store::{SqliteStore, Store};
use crate::time::snap_to_frame;
use chrono::Utc;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// Journals persist per project, so undo survives restarts. The exact
/// inverse/redo clip snapshots live HERE, not on the public EditAction.
const UNDO_CAP: usize = 100;

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
        Ok(editor)
    }

    pub fn test_instance() -> Self {
        let store = SqliteStore::open_in_memory().expect("in-memory store");
        // Deterministic: no live model server in tests, so the agent loop
        // always exercises its offline path.
        let mut prefs = Preferences::default();
        prefs.inference_endpoint = "http://127.0.0.1:9/v1".into();
        store.save_preferences(&prefs).expect("prefs");
        Self::new(
            Box::new(store),
            Box::new(MockVideoEngine),
            Box::new(MockTranscriber),
            Arc::new(NullSink),
            true,
        )
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

    /// Inference client built from current preferences (the endpoint makes the
    /// local model and a remote frontier model interchangeable).
    pub fn inference(&self) -> Result<Box<dyn InferenceClient>> {
        let prefs = self.inner.store.load_preferences()?;
        Ok(Box::new(OpenAiCompatClient::new(&prefs.inference_endpoint, None)))
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

    // -------------------------------------------------- user confirmations

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

    // ----------------------------------------------------- semantic index

    /// Embed every speech segment and persist the vectors. Best-effort: no
    /// embedding server → Ok(0), keyword search keeps working.
    pub async fn index_transcript(&self, project_id: Uuid) -> Result<u32> {
        let transcript = match self.get_transcript(project_id)? {
            Some(t) => t,
            None => return Ok(0),
        };
        let embedder = self.embedder()?;
        let items: Vec<(Uuid, String)> = transcript
            .segments
            .iter()
            .filter(|seg| !seg.is_silence && !seg.is_filler && !seg.text.is_empty())
            .map(|seg| (seg.id, seg.text.clone()))
            .collect();
        if items.is_empty() {
            return Ok(0);
        }
        let mut vectors: Vec<(Uuid, Vec<f32>)> = Vec::with_capacity(items.len());
        for chunk in items.chunks(64) {
            let texts: Vec<String> = chunk.iter().map(|(_, t)| t.clone()).collect();
            let embedded = match embedder.embed(&texts).await {
                Ok(v) => v,
                // No embedding server / model missing: silently skip.
                Err(_) => return Ok(0),
            };
            for ((id, _), v) in chunk.iter().zip(embedded) {
                vectors.push((*id, v));
            }
        }
        let n = vectors.len() as u32;
        self.inner.store.save_embeddings(project_id, &embedder.model_id(), &vectors)?;
        Ok(n)
    }

    /// Cosine top-k over the project's index. None = no usable index (caller
    /// falls back to keyword search).
    pub async fn semantic_find(
        &self,
        project_id: Uuid,
        query: &str,
        k: usize,
    ) -> Result<Option<Vec<(Uuid, f32)>>> {
        let Some((indexed_model, vectors)) = self.inner.store.load_embeddings(project_id)? else {
            return Ok(None);
        };
        let embedder = self.embedder()?;
        if embedder.model_id() != indexed_model {
            return Ok(None); // stale index from a different model
        }
        let q = match embedder.embed(&[query.to_string()]).await {
            Ok(mut v) if !v.is_empty() => v.remove(0),
            _ => return Ok(None),
        };
        let mut scored: Vec<(Uuid, f32)> = vectors
            .iter()
            .map(|(id, v)| (*id, crate::adapters::embed::cosine(&q, v)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(k);
        Ok(Some(scored))
    }

    // ------------------------------------------------------------ projects

    pub async fn create_project(&self, name: &str, file_path: Option<&str>) -> Result<Project> {
        let mut project = Project::new(name);
        if let Some(path) = file_path {
            let media = self.inner.video.probe(path).await?;
            project.timeline = Timeline::new(media.duration);
            project.media = Some(media);
        }
        self.inner.store.save_project(&project)?;
        {
            let mut state = self.inner.state.lock().unwrap();
            state.insert(
                project.id,
                ProjectState { project: Some(project.clone()), ..Default::default() },
            );
        }
        send(&self.inner.sink, CoreEvent::ProjectsChanged {});
        Ok(project)
    }

    /// Save-as: clone media + current cut state under a new name with a fresh
    /// undo history. The original project is untouched.
    pub fn duplicate_project(&self, project_id: Uuid, name: &str) -> Result<Project> {
        self.ensure_loaded(project_id)?;
        let (source, transcript) = {
            let state = self.inner.state.lock().unwrap();
            let entry =
                state.get(&project_id).ok_or_else(|| CoreError::NotFound("project".into()))?;
            (
                entry.project.clone().ok_or_else(|| CoreError::NotFound("project".into()))?,
                entry.transcript.clone(),
            )
        };
        let mut copy = source;
        copy.id = Uuid::new_v4();
        copy.name = name.to_string();
        copy.created_at = Utc::now();
        copy.updated_at = Utc::now();
        self.inner.store.save_project(&copy)?;
        if let Some(t) = &transcript {
            self.inner.store.save_transcript(copy.id, t)?;
        }
        {
            let mut state = self.inner.state.lock().unwrap();
            state.insert(
                copy.id,
                ProjectState {
                    project: Some(copy.clone()),
                    transcript,
                    ..Default::default()
                },
            );
        }
        send(&self.inner.sink, CoreEvent::ProjectsChanged {});
        Ok(copy)
    }

    fn ensure_loaded(&self, project_id: Uuid) -> Result<()> {
        let mut state = self.inner.state.lock().unwrap();
        let entry = state.entry(project_id).or_default();
        if entry.project.is_none() {
            entry.project = Some(self.inner.store.load_project(project_id)?);
            entry.transcript = self.inner.store.load_transcript(project_id)?;
            // Undo/redo journal survives restarts.
            if let Some(doc) = self.inner.store.get_kv(&format!("journal:{project_id}"))? {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&doc) {
                    entry.undo = serde_json::from_value(v["undo"].clone()).unwrap_or_default();
                    entry.redo = serde_json::from_value(v["redo"].clone()).unwrap_or_default();
                }
            }
        }
        Ok(())
    }

    pub fn open_project(&self, project_id: Uuid) -> Result<Project> {
        self.ensure_loaded(project_id)?;
        self.with_project(project_id, |p, _| Ok(p.clone()))
    }

    pub fn save_project(&self, project_id: Uuid) -> Result<Project> {
        self.ensure_loaded(project_id)?;
        let state = self.inner.state.lock().unwrap();
        let entry = state.get(&project_id).ok_or_else(|| CoreError::NotFound("project".into()))?;
        let project = entry.project.clone().ok_or_else(|| CoreError::NotFound("project".into()))?;
        self.inner.store.save_project(&project)?;
        if let Some(t) = &entry.transcript {
            self.inner.store.save_transcript(project_id, t)?;
        }
        Ok(project)
    }

    pub fn list_projects(&self) -> Result<Vec<ProjectSummary>> {
        self.inner.store.list_projects()
    }

    /// Delete a project's edit state (store + memory). Non-destructive to
    /// media: the source video file is never touched.
    pub fn delete_project(&self, project_id: Uuid) -> Result<()> {
        self.inner.store.delete_project(project_id)?;
        self.inner.state.lock().unwrap().remove(&project_id);
        send(&self.inner.sink, CoreEvent::ProjectsChanged {});
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

    pub fn get_timeline(&self, project_id: Uuid) -> Result<Timeline> {
        self.with_project(project_id, |p, _| Ok(p.timeline.clone()))
    }

    /// Waveform peaks + thumbnails for the project's media (generated on
    /// first call, cached after). Returns file PATHS for the asset protocol.
    pub async fn media_assets(
        &self,
        project_id: Uuid,
    ) -> Result<crate::adapters::video::MediaAssets> {
        let media = self
            .with_project(project_id, |p, _| Ok(p.media.clone()))?
            .ok_or_else(|| CoreError::InvalidArg("project has no media".into()))?;
        self.inner.video.media_assets(&media).await
    }

    pub fn get_transcript(&self, project_id: Uuid) -> Result<Option<Transcript>> {
        self.ensure_loaded(project_id)?;
        let state = self.inner.state.lock().unwrap();
        Ok(state.get(&project_id).and_then(|e| e.transcript.clone()))
    }

    // --------------------------------------------------------------- media

    pub async fn import_media(
        &self,
        project_id: Option<Uuid>,
        file_path: &str,
    ) -> Result<Media> {
        let media = self.inner.video.probe(file_path).await?;
        if let Some(pid) = project_id {
            self.ensure_loaded(pid)?;
            {
                let mut state = self.inner.state.lock().unwrap();
                let entry =
                    state.get_mut(&pid).ok_or_else(|| CoreError::NotFound("project".into()))?;
                let project =
                    entry.project.as_mut().ok_or_else(|| CoreError::NotFound("project".into()))?;
                project.timeline = Timeline::new(media.duration);
                project.media = Some(media.clone());
                project.updated_at = Utc::now();
                self.inner.store.save_project(project)?;
            }
            send(&self.inner.sink, CoreEvent::TimelineChanged { project_id: pid });
        }
        Ok(media)
    }

    pub async fn transcribe(&self, project_id: Uuid, language: Option<&str>) -> Result<Transcript> {
        self.ensure_loaded(project_id)?;
        let media = self
            .with_project(project_id, |p, _| Ok(p.media.clone()))?
            .ok_or_else(|| CoreError::InvalidArg("project has no media; import first".into()))?;
        let prefs = self.inner.store.load_preferences()?;
        let lang = language.map(|s| s.to_string()).unwrap_or(prefs.language);
        let demo = self.demo_mode();
        send(&self.inner.sink, CoreEvent::progress("transcribe", Some(project_id), 0.0, "starting"));

        let transcript = if demo {
            self.inner.transcriber.transcribe(&media, "", &lang, &self.inner.sink).await?
        } else {
            let wav = std::env::temp_dir().join(format!("roughcut-{}.wav", media.id));
            let wav_str = wav.to_string_lossy().to_string();
            send(
                &self.inner.sink,
                CoreEvent::progress("transcribe", Some(project_id), 0.1, "extracting audio"),
            );
            self.inner.video.extract_audio_wav(&media, &wav_str).await?;
            send(
                &self.inner.sink,
                CoreEvent::progress("transcribe", Some(project_id), 0.25, "transcribing on-device"),
            );
            let t = self.inner.transcriber.transcribe(&media, &wav_str, &lang, &self.inner.sink).await;
            let _ = std::fs::remove_file(&wav);
            t?
        };

        {
            let mut state = self.inner.state.lock().unwrap();
            let entry =
                state.get_mut(&project_id).ok_or_else(|| CoreError::NotFound("project".into()))?;
            entry.transcript = Some(transcript.clone());
        }
        self.inner.store.save_transcript(project_id, &transcript)?;
        send(&self.inner.sink, CoreEvent::progress("transcribe", Some(project_id), 1.0, "done"));
        send(&self.inner.sink, CoreEvent::TranscriptChanged { project_id });

        // Gemma post-pass (real transcriptions only): clean up casing /
        // punctuation / mis-hearings in the background; the UI refreshes via
        // transcript-changed when it lands. Timestamps are never touched.
        {
            let editor = self.clone();
            tokio::spawn(async move {
                if !demo {
                    if let Ok(n) = crate::agent::clean_transcript(&editor, project_id).await {
                        if n > 0 {
                            send(
                                &editor.inner.sink,
                                CoreEvent::progress(
                                    "transcribe",
                                    Some(project_id),
                                    1.0,
                                    format!("polished {n} segment(s) with the local LLM"),
                                ),
                            );
                        }
                    }
                }
                // Semantic index AFTER cleanup so vectors match final text.
                let _ = editor.index_transcript(project_id).await;
            });
        }
        Ok(transcript)
    }

    /// Replace segment texts (and word texts when counts align) keeping ALL
    /// timestamps intact — used by the LLM transcript cleanup pass.
    pub fn update_segment_texts(&self, project_id: Uuid, changes: &[(Uuid, String)]) -> Result<u32> {
        self.ensure_loaded(project_id)?;
        let mut applied = 0;
        {
            let mut state = self.inner.state.lock().unwrap();
            let entry =
                state.get_mut(&project_id).ok_or_else(|| CoreError::NotFound("project".into()))?;
            let t = entry
                .transcript
                .as_mut()
                .ok_or_else(|| CoreError::InvalidArg("no transcript".into()))?;
            for (id, text) in changes {
                if let Some(seg) = t.segments.iter_mut().find(|s| s.id == *id) {
                    let new_words: Vec<&str> = text.split_whitespace().collect();
                    if new_words.len() == seg.words.len() {
                        for (w, nw) in seg.words.iter_mut().zip(&new_words) {
                            w.text = (*nw).to_string();
                        }
                    }
                    seg.text = text.clone();
                    applied += 1;
                }
            }
            if applied > 0 {
                t.model_used =
                    format!("{}+llm-cleanup", t.model_used.trim_end_matches("+llm-cleanup"));
                self.inner.store.save_transcript(project_id, t)?;
            }
        }
        if applied > 0 {
            send(&self.inner.sink, CoreEvent::TranscriptChanged { project_id });
        }
        Ok(applied)
    }

    // ----------------------------------------------------------- mutations

    /// THE mutation seam: every edit — UI, local agent, or MCP client — is an
    /// [`EditOp`] applied here. Frame-snap → apply → record an EditAction
    /// carrying the op, its exact inverse, and its exact redo → persist the
    /// project and the undo journal → notify.
    pub fn apply_edit(
        &self,
        project_id: Uuid,
        op: EditOp,
        source: ActionSource,
    ) -> Result<EditOutcome> {
        self.ensure_loaded(project_id)?;
        let prefs = self.inner.store.load_preferences()?;
        let outcome = {
            let mut state = self.inner.state.lock().unwrap();
            let entry =
                state.get_mut(&project_id).ok_or_else(|| CoreError::NotFound("project".into()))?;
            let ProjectState { project, transcript, undo, redo } = entry;
            let project =
                project.as_mut().ok_or_else(|| CoreError::NotFound("project".into()))?;
            let op = normalize_op(op, project.fps());
            let before_clips = project.timeline.clips.clone();
            let before_padding = project.timeline.global_padding;
            let mut split_ids = vec![];
            apply_op(project, transcript.as_mut(), &op, &prefs, &mut split_ids)?;
            project.timeline.normalize();
            project.updated_at = Utc::now();
            let action = EditAction::new(source, op);
            undo.push(JournalEntry {
                action: action.clone(),
                inverse: EditOp::SetClips { clips: before_clips, global_padding: before_padding },
                redo: EditOp::SetClips {
                    clips: project.timeline.clips.clone(),
                    global_padding: project.timeline.global_padding,
                },
            });
            if undo.len() > UNDO_CAP {
                undo.remove(0);
            }
            redo.clear();
            self.inner.store.save_project(project)?;
            if let Some(t) = transcript.as_ref() {
                self.inner.store.save_transcript(project_id, t)?;
            }
            self.persist_journal(project_id, undo, redo)?;
            let split_clips = project
                .timeline
                .clips
                .iter()
                .filter(|c| split_ids.contains(&c.id))
                .cloned()
                .collect();
            EditOutcome { action, timeline: project.timeline.clone(), split_clips }
        };
        send(&self.inner.sink, CoreEvent::TimelineChanged { project_id });
        Ok(outcome)
    }

    fn persist_journal(&self, project_id: Uuid, undo: &[JournalEntry], redo: &[JournalEntry]) -> Result<()> {
        let doc = serde_json::json!({ "undo": undo, "redo": redo }).to_string();
        self.inner.store.set_kv(&format!("journal:{project_id}"), &doc)
    }

    /// Detect fillers + repeated takes and PERSIST the flags — the same write
    /// path the rough cut uses, so analysis behaves identically from every
    /// caller (UI, agent, MCP).
    pub fn annotate_transcript(
        &self,
        project_id: Uuid,
    ) -> Result<(detect::FillerFindings, Vec<detect::TakeGroup>)> {
        self.ensure_loaded(project_id)?;
        let prefs = self.inner.store.load_preferences()?;
        let out = {
            let mut state = self.inner.state.lock().unwrap();
            let entry =
                state.get_mut(&project_id).ok_or_else(|| CoreError::NotFound("project".into()))?;
            let t = entry
                .transcript
                .as_mut()
                .ok_or_else(|| CoreError::InvalidArg("no transcript".into()))?;
            let result = detect::annotate(t, &prefs.custom_filler_words);
            self.inner.store.save_transcript(project_id, t)?;
            result
        };
        send(&self.inner.sink, CoreEvent::TranscriptChanged { project_id });
        Ok(out)
    }

    pub async fn generate_rough_cut(
        &self,
        project_id: Uuid,
        aggressiveness: Option<Aggressiveness>,
        source: ActionSource,
    ) -> Result<(Timeline, u32)> {
        self.ensure_loaded(project_id)?;
        if self.get_transcript(project_id)?.is_none() {
            self.transcribe(project_id, None).await?;
        }
        let prefs = self.inner.store.load_preferences()?;
        let aggr = aggressiveness.unwrap_or(prefs.cut_aggressiveness);
        let outcome = self.apply_edit(project_id, EditOp::RoughCut { aggressiveness: aggr }, source)?;
        let cut_count = outcome.timeline.cut_count;
        Ok((outcome.timeline, cut_count))
    }

    // ----------------------------------------------------------- undo/redo

    pub fn undo(&self, project_id: Uuid) -> Result<(Option<EditAction>, Timeline)> {
        self.step_history(project_id, true)
    }

    pub fn redo(&self, project_id: Uuid) -> Result<(Option<EditAction>, Timeline)> {
        self.step_history(project_id, false)
    }

    fn step_history(
        &self,
        project_id: Uuid,
        is_undo: bool,
    ) -> Result<(Option<EditAction>, Timeline)> {
        self.ensure_loaded(project_id)?;
        let prefs = self.inner.store.load_preferences()?;
        let result = {
            let mut state = self.inner.state.lock().unwrap();
            let entry =
                state.get_mut(&project_id).ok_or_else(|| CoreError::NotFound("project".into()))?;
            let ProjectState { project, transcript, undo, redo } = entry;
            let project =
                project.as_mut().ok_or_else(|| CoreError::NotFound("project".into()))?;
            let popped = if is_undo { undo.pop() } else { redo.pop() };
            match popped {
                Some(entry) => {
                    let replay = if is_undo { &entry.inverse } else { &entry.redo };
                    apply_op(project, transcript.as_mut(), replay, &prefs, &mut vec![])?;
                    project.timeline.normalize();
                    project.updated_at = Utc::now();
                    self.inner.store.save_project(project)?;
                    let action = entry.action.clone();
                    if is_undo {
                        redo.push(entry);
                    } else {
                        undo.push(entry);
                    }
                    self.persist_journal(project_id, undo, redo)?;
                    (Some(action), project.timeline.clone())
                }
                None => (None, project.timeline.clone()),
            }
        };
        send(&self.inner.sink, CoreEvent::TimelineChanged { project_id });
        Ok(result)
    }

    // --------------------------------------------------------- preferences

    pub fn get_preferences(&self) -> Result<Preferences> {
        self.inner.store.load_preferences()
    }

    pub fn set_preferences(&self, prefs: Preferences) -> Result<Preferences> {
        self.inner.store.save_preferences(&prefs)?;
        Ok(prefs)
    }
}

// ------------------------------------------------------------- op helpers

/// Frame-snap an op's time fields (frame-exact internally, seconds at the
/// API edge) and fold linked padding. One place, every caller.
fn normalize_op(op: EditOp, fps: f64) -> EditOp {
    match op {
        EditOp::CutRange { start, end } => {
            EditOp::CutRange { start: snap_to_frame(start, fps), end: snap_to_frame(end, fps) }
        }
        EditOp::RestoreRange { start, end } => {
            EditOp::RestoreRange { start: snap_to_frame(start, fps), end: snap_to_frame(end, fps) }
        }
        EditOp::TrimClip { clip_id, new_source_in, new_source_out } => EditOp::TrimClip {
            clip_id,
            new_source_in: snap_to_frame(new_source_in, fps),
            new_source_out: snap_to_frame(new_source_out, fps),
        },
        EditOp::SplitClip { clip_id, at_time } => {
            EditOp::SplitClip { clip_id, at_time: snap_to_frame(at_time, fps) }
        }
        EditOp::SetGlobalPadding { start_s, end_s, linked } => {
            let (start_s, end_s) = if linked { (start_s, start_s) } else { (start_s, end_s) };
            EditOp::SetGlobalPadding { start_s, end_s, linked }
        }
        other => other,
    }
}

fn resolve_segments(t: &Transcript, ids: &[Uuid]) -> Result<Vec<(Uuid, f64, f64)>> {
    ids.iter()
        .map(|id| {
            t.segment(*id)
                .map(|s| (s.id, s.start, s.end))
                .ok_or_else(|| CoreError::NotFound(format!("segment {id}")))
        })
        .collect()
}

/// Apply one op to the project — shared verbatim by apply_edit, undo, and
/// redo, so history replay can never drift from the original edit.
fn apply_op(
    project: &mut Project,
    transcript: Option<&mut Transcript>,
    op: &EditOp,
    prefs: &Preferences,
    split_ids: &mut Vec<Uuid>,
) -> Result<()> {
    match op {
        EditOp::CutRange { start, end } => {
            project.timeline.set_range_included(*start, *end, false, ClipOrigin::Manual);
        }
        EditOp::RestoreRange { start, end } => {
            project.timeline.set_range_included(*start, *end, true, ClipOrigin::Manual);
        }
        EditOp::CutSegments { segment_ids } => {
            let t = transcript
                .as_deref()
                .ok_or_else(|| CoreError::InvalidArg("no transcript yet".into()))?;
            for (id, s, e) in resolve_segments(t, segment_ids)? {
                project.timeline.cut_linked(s, e, &[id], ClipOrigin::Manual);
            }
        }
        EditOp::RestoreSegments { segment_ids } => {
            let t = transcript
                .as_deref()
                .ok_or_else(|| CoreError::InvalidArg("no transcript yet".into()))?;
            for (_, s, e) in resolve_segments(t, segment_ids)? {
                project.timeline.set_range_included(s, e, true, ClipOrigin::Manual);
            }
        }
        EditOp::TrimClip { clip_id, new_source_in, new_source_out } => {
            project
                .timeline
                .trim_clip(*clip_id, *new_source_in, *new_source_out)
                .map_err(CoreError::InvalidArg)?;
        }
        EditOp::SplitClip { clip_id, at_time } => {
            let (l, r) =
                project.timeline.split_clip(*clip_id, *at_time).map_err(CoreError::InvalidArg)?;
            split_ids.extend([l, r]);
        }
        EditOp::ReorderClip { clip_id, new_order } => {
            let clips = &mut project.timeline.clips;
            let idx = clips
                .iter()
                .position(|c| c.id == *clip_id)
                .ok_or_else(|| CoreError::NotFound(format!("clip {clip_id}")))?;
            let clip = clips.remove(idx);
            let new_idx = (*new_order as usize).min(clips.len());
            clips.insert(new_idx, clip);
            for (i, c) in clips.iter_mut().enumerate() {
                c.order = i as u32;
            }
        }
        EditOp::SetGlobalPadding { start_s, end_s, linked } => {
            project.timeline.apply_padding(*start_s, *end_s, *linked);
        }
        EditOp::RoughCut { aggressiveness } => {
            let t = transcript.ok_or_else(|| CoreError::InvalidArg("no transcript".into()))?;
            let duration = project.timeline.duration;
            let outcome = detect::generate_rough_cut(
                t,
                duration,
                *aggressiveness,
                &prefs.custom_filler_words,
                prefs.silence_min_duration_s,
            );
            // COMPOSE with the existing cut: AI exclusions are added on top of
            // whatever is already cut (manual or MCP edits survive — and the
            // whole pass is still one undo step).
            for c in outcome.timeline.clips.iter().filter(|c| !c.included) {
                project.timeline.cut_linked(
                    c.source_in,
                    c.source_out,
                    &c.linked_segment_ids,
                    ClipOrigin::AiCut,
                );
            }
            let padding = prefs.default_padding;
            if padding.start_s > 0.0 || padding.end_s > 0.0 {
                project.timeline.apply_padding(padding.start_s, padding.end_s, padding.linked);
            }
        }
        EditOp::SetClips { clips, global_padding } => {
            project.timeline.clips = clips.clone();
            project.timeline.global_padding = *global_padding;
        }
    }
    Ok(())
}
