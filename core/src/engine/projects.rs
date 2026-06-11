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
    /// background; `media-assets-changed` fires when it lands.
    pub async fn media_assets(
        &self,
        project_id: Uuid,
    ) -> Result<crate::adapters::video::MediaAssets> {
        let media = self
            .with_project(project_id, |p, _| Ok(p.media.clone()))?
            .ok_or_else(|| CoreError::InvalidArg("project has no media".into()))?;
        let assets = self.inner.video.media_assets(&media).await?;
        if assets.playback_path.is_none() && !self.demo_mode() {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let editor = self.clone();
                handle.spawn(async move {
                    if let Ok(Some(_)) =
                        crate::adapters::video::ensure_playable_copy(&media).await
                    {
                        send(&editor.inner.sink, CoreEvent::MediaAssetsChanged { project_id });
                    }
                });
            }
        }
        Ok(assets)
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
