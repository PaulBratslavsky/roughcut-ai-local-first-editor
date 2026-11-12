//! Project lifecycle (create / duplicate / open / save / list / delete) and
//! the read accessors built on [`Editor::with_project`].

use super::{Editor, ProjectState};
use crate::error::{CoreError, Result};
use crate::events::{send, CoreEvent};
use crate::model::*;
use chrono::Utc;
use uuid::Uuid;

impl Editor {
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

    pub fn open_project(&self, project_id: Uuid) -> Result<Project> {
        self.ensure_loaded(project_id)?;
        // Old projects (transcribed before indexing existed) gain a semantic
        // index in the background the first time they're opened.
        self.backfill_index_if_missing(project_id);
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

    /// Move a project to the TRASH (soft delete): edit state survives and
    /// [`Editor::restore_project`] brings it back; the startup sweep purges
    /// trash older than 30 days. The source video file is never touched.
    pub fn delete_project(&self, project_id: Uuid) -> Result<()> {
        self.ensure_loaded(project_id)?;
        {
            let mut state = self.inner.state.lock().unwrap();
            let entry =
                state.get_mut(&project_id).ok_or_else(|| CoreError::NotFound("project".into()))?;
            let project =
                entry.project.as_mut().ok_or_else(|| CoreError::NotFound("project".into()))?;
            project.deleted_at = Some(Utc::now());
            project.updated_at = Utc::now();
            self.inner.store.save_project(project)?;
        }
        self.inner.state.lock().unwrap().remove(&project_id);
        send(&self.inner.sink, CoreEvent::ProjectsChanged {});
        Ok(())
    }

    /// Bring a trashed project back, edit state and all.
    pub fn restore_project(&self, project_id: Uuid) -> Result<Project> {
        self.ensure_loaded(project_id)?;
        let project = {
            let mut state = self.inner.state.lock().unwrap();
            let entry =
                state.get_mut(&project_id).ok_or_else(|| CoreError::NotFound("project".into()))?;
            let project =
                entry.project.as_mut().ok_or_else(|| CoreError::NotFound("project".into()))?;
            project.deleted_at = None;
            project.updated_at = Utc::now();
            self.inner.store.save_project(project)?;
            project.clone()
        };
        send(&self.inner.sink, CoreEvent::ProjectsChanged {});
        Ok(project)
    }

    /// Hard-delete trash older than `days` (called once at startup).
    pub fn purge_trash(&self, days: i64) -> Result<u32> {
        let cutoff = Utc::now() - chrono::Duration::days(days);
        let mut purged = 0;
        for p in self.inner.store.list_projects()? {
            if matches!(p.deleted_at, Some(t) if t < cutoff) {
                self.inner.store.delete_project(p.id)?;
                self.inner.state.lock().unwrap().remove(&p.id);
                purged += 1;
            }
        }
        Ok(purged)
    }

    pub fn get_timeline(&self, project_id: Uuid) -> Result<Timeline> {
        self.with_project(project_id, |p, _| Ok(p.timeline.clone()))
    }

    /// Waveform peaks + thumbnails for the project's media (generated on
    /// first call, cached after). Returns file PATHS for the asset protocol.
    /// A missing playable copy (sources WebKit can't stream) builds in the
    /// background, started BEFORE the peaks and thumbnails so the preview
    /// doesn't wait on them; `media-assets-changed` fires when it lands or
    /// fails (then `playback_error` says why, and it isn't retried this run).
    pub async fn media_assets(
        &self,
        project_id: Uuid,
    ) -> Result<crate::adapters::video::MediaAssets> {
        use crate::adapters::video::{ensure_playable_copy, playable_copy_failure, playable_copy_path};
        let media = self
            .with_project(project_id, |p, _| Ok(p.media.clone()))?
            .ok_or_else(|| CoreError::InvalidArg("project has no media".into()))?;
        // Only when there's neither a copy nor a recorded failure: otherwise
        // the event below would trigger a refetch that lands here again.
        if !self.demo_mode()
            && playable_copy_path(&media).is_none()
            && playable_copy_failure(&media).is_none()
        {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let editor = self.clone();
                let media = media.clone();
                handle.spawn(async move {
                    match ensure_playable_copy(&media).await {
                        Ok(Some(_)) | Err(_) => {
                            send(&editor.inner.sink, CoreEvent::MediaAssetsChanged { project_id })
                        }
                        Ok(None) => {} // plays as-is, or another call is building it
                    }
                });
            }
        }
        self.inner.video.media_assets(&media).await
    }

    pub fn get_transcript(&self, project_id: Uuid) -> Result<Option<Transcript>> {
        self.ensure_loaded(project_id)?;
        let state = self.inner.state.lock().unwrap();
        Ok(state.get(&project_id).and_then(|e| e.transcript.clone()))
    }

    pub fn get_preferences(&self) -> Result<Preferences> {
        self.inner.store.load_preferences()
    }

    pub fn set_preferences(&self, prefs: Preferences) -> Result<Preferences> {
        self.inner.store.save_preferences(&prefs)?;
        Ok(prefs)
    }

    /// Apply a PARTIAL preferences update: only the keys given change, nested
    /// objects (`default_padding`) merge, unknown keys are rejected rather than
    /// silently dropped. Zero egress: only the user may point inference at a
    /// host off this machine — post-transcribe cleanup and the embedding index
    /// send transcript text wherever `inference_endpoint` names.
    pub fn update_preferences(
        &self,
        patch: &serde_json::Value,
        source: ActionSource,
    ) -> Result<Preferences> {
        let patch = patch
            .as_object()
            .ok_or_else(|| CoreError::InvalidArg("preferences must be an object".into()))?;
        let _write = self.inner.prefs_write.lock().unwrap();
        let current = self.get_preferences()?;
        let serde_json::Value::Object(mut merged) = serde_json::to_value(&current)? else {
            return Err(CoreError::Other("preferences did not serialize to an object".into()));
        };
        let unknown: Vec<&str> =
            patch.keys().filter(|k| !merged.contains_key(*k)).map(String::as_str).collect();
        if !unknown.is_empty() {
            let mut known: Vec<&str> = merged.keys().map(String::as_str).collect();
            known.sort_unstable();
            return Err(CoreError::InvalidArg(format!(
                "unknown preference(s): {}; known: {}",
                unknown.join(", "),
                known.join(", ")
            )));
        }
        merge_into(&mut merged, patch);
        let next: Preferences = serde_json::from_value(serde_json::Value::Object(merged))?;
        // The model switch, its model and the consent are the user's, set in
        // the app (shell commands), never through this tool by any caller.
        if next.model_mode != current.model_mode
            || next.hosted_model != current.hosted_model
            || next.hosted_consent_at != current.hosted_consent_at
        {
            return Err(CoreError::InvalidArg(
                "model_mode, hosted_model and hosted_consent_at can only be changed in the app \
                 (the LOCAL / HOSTED switch and Setup)"
                    .into(),
            ));
        }
        // Widening where MCP clients may write without asking is the user's
        // call, in Setup — no caller can do it through this tool.
        if next.trusted_export_folder != current.trusted_export_folder {
            return Err(CoreError::InvalidArg(
                "trusted_export_folder can only be changed in the app (Setup ▸ Exports from AI tools)".into(),
            ));
        }
        if source != ActionSource::Ui
            && next.inference_endpoint != current.inference_endpoint
            && !is_loopback_url(&next.inference_endpoint)
        {
            return Err(CoreError::InvalidArg(format!(
                "inference_endpoint {} is not on this machine; only the user can send \
                 transcripts off-device (change it in the app's Setup screen)",
                next.inference_endpoint
            )));
        }
        self.set_preferences(next)
    }

    /// Set (or clear, with None) the folder MCP exports may write into
    /// without asking. The user's action in the app only: a Tauri shell
    /// command, never a registry tool, so no MCP client or agent can widen
    /// its own write access (same rule as the model switch, ADR-0007).
    pub fn set_trusted_export_folder(&self, folder: Option<&str>) -> Result<Preferences> {
        let folder = match folder.map(str::trim).filter(|f| !f.is_empty()) {
            None => None,
            Some(f) => Some(crate::export::canonical_folder(f)?),
        };
        let _write = self.inner.prefs_write.lock().unwrap();
        let mut prefs = self.get_preferences()?;
        prefs.trusted_export_folder = folder;
        self.set_preferences(prefs)
    }

    /// Presentation layout — project metadata, persisted, not journaled.
    pub fn set_layout(&self, project_id: uuid::Uuid, layout: crate::model::Layout) -> crate::error::Result<()> {
        self.ensure_loaded(project_id)?;
        let mut state = self.inner.state.lock().unwrap();
        let entry = state
            .get_mut(&project_id)
            .and_then(|e| e.project.as_mut())
            .ok_or_else(|| crate::error::CoreError::NotFound(format!("project {project_id}")))?;
        entry.layout = layout;
        self.inner.store.save_project(entry)?;
        crate::events::send(
            &self.sink(),
            crate::events::CoreEvent::TimelineChanged { project_id },
        );
        Ok(())
    }

    /// A/V sync nudge — project metadata, persisted, not journaled.
    pub fn set_audio_offset(&self, project_id: uuid::Uuid, offset_s: f64) -> crate::error::Result<()> {
        self.ensure_loaded(project_id)?;
        let mut state = self.inner.state.lock().unwrap();
        let entry = state
            .get_mut(&project_id)
            .and_then(|e| e.project.as_mut())
            .ok_or_else(|| crate::error::CoreError::NotFound(format!("project {project_id}")))?;
        entry.audio_offset_s = offset_s;
        self.inner.store.save_project(entry)?;
        Ok(())
    }
}

/// JSON merge: nested objects merge key by key; anything else replaces.
fn merge_into(
    base: &mut serde_json::Map<String, serde_json::Value>,
    patch: &serde_json::Map<String, serde_json::Value>,
) {
    for (k, v) in patch {
        match (base.get_mut(k), v) {
            (Some(serde_json::Value::Object(b)), serde_json::Value::Object(p)) => merge_into(b, p),
            _ => {
                base.insert(k.clone(), v.clone());
            }
        }
    }
}

/// Is `url` served from this machine (localhost, 127.0.0.0/8, ::1)?
fn is_loopback_url(url: &str) -> bool {
    let Some(host) = reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_owned))
    else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host == "localhost"
        || host.ends_with(".localhost")
        || host.parse::<std::net::IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_urls() {
        for local in [
            "http://localhost:11434/v1",
            "http://127.0.0.1:47861/v1",
            "http://127.1.2.3/v1",
            "http://[::1]:8080/v1",
            "http://LOCALHOST/v1",
            "http://llm.localhost/v1",
        ] {
            assert!(is_loopback_url(local), "{local} should be local");
        }
        for remote in [
            "https://api.example.com/v1",
            "http://192.168.1.10:11434/v1",
            "http://0.0.0.0:11434/v1",
            "http://localhost.example.com/v1",
            "not a url",
            "",
        ] {
            assert!(!is_loopback_url(remote), "{remote} should not be local");
        }
    }

    #[test]
    fn merge_is_deep_for_objects_and_replaces_otherwise() {
        let mut base = serde_json::json!({
            "a": 1,
            "pad": { "start_s": 0.1, "end_s": 0.2, "linked": true },
            "words": ["um"]
        });
        let patch = serde_json::json!({ "pad": { "end_s": 0.5 }, "words": ["uh"] });
        merge_into(base.as_object_mut().unwrap(), patch.as_object().unwrap());
        assert_eq!(
            base,
            serde_json::json!({
                "a": 1,
                "pad": { "start_s": 0.1, "end_s": 0.5, "linked": true },
                "words": ["uh"]
            })
        );
    }
}
