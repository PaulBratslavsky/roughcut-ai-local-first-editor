//! Media import and transcription, including the background LLM cleanup +
//! semantic-index pass that follows a real transcription.

use super::Editor;
use crate::error::{CoreError, Result};
use crate::events::{send, CoreEvent, ProgressTask};
use crate::model::*;
use chrono::Utc;
use uuid::Uuid;

impl Editor {
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
        send(&self.inner.sink, CoreEvent::progress(ProgressTask::Transcribe, Some(project_id), 0.0, "starting"));

        let transcript = if demo {
            self.inner.transcriber.transcribe(&media, "", &lang, Some(project_id), &self.inner.sink).await?
        } else {
            let wav = std::env::temp_dir().join(format!("roughcut-{}.wav", media.id));
            let wav_str = wav.to_string_lossy().to_string();
            send(
                &self.inner.sink,
                CoreEvent::progress(ProgressTask::Transcribe, Some(project_id), 0.1, "extracting audio"),
            );
            self.inner.video.extract_audio_wav(&media, &wav_str).await?;
            send(
                &self.inner.sink,
                CoreEvent::progress(ProgressTask::Transcribe, Some(project_id), 0.25, "transcribing on-device"),
            );
            let t = self.inner.transcriber.transcribe(&media, &wav_str, &lang, Some(project_id), &self.inner.sink).await;
            let _ = std::fs::remove_file(&wav);
            t?
        };

        {
            let mut state = self.inner.state.lock().unwrap();
            let entry =
                state.get_mut(&project_id).ok_or_else(|| CoreError::NotFound("project".into()))?;
            entry.transcript = Some(transcript.clone());
            // Rough-cut suggestions reference the OLD segment ids — a fresh
            // transcribe replaces every id, so the old flags are stale.
            if let Some(p) = entry.project.as_mut() {
                if !p.suggestions.is_empty() {
                    p.suggestions.clear();
                    let _ = self.inner.store.save_project(p);
                }
            }
        }
        self.inner.store.save_transcript(project_id, &transcript)?;
        send(&self.inner.sink, CoreEvent::progress(ProgressTask::Transcribe, Some(project_id), 1.0, "done"));
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
                                CoreEvent::progress(ProgressTask::Transcribe,
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
}

impl super::Editor {
    /// Append another video to the END of this project's source: lossless
    /// concat into a new file in ~/Movies/RoughCut (originals untouched),
    /// media swapped, timeline extended with an included clip over the new
    /// span. Existing cuts keep their timestamps. Caller re-transcribes.
    /// Inputs must match codec/resolution (stream copy) — mismatches refuse.
    pub async fn append_media(&self, project_id: uuid::Uuid, file_path: &str) -> crate::error::Result<crate::model::Media> {
        use crate::error::CoreError;
        let current = self.with_project(project_id, |p, _| {
            p.media.clone().ok_or_else(|| CoreError::InvalidArg("project has no media".into()))
        })?;
        if !std::path::Path::new(file_path).is_file() {
            return Err(CoreError::NotFound(format!("file {file_path}")));
        }
        // combine_recordings carries the codec/resolution guard.
        let combined = crate::adapters::record::combine_recordings(&[
            current.file_path.clone(),
            file_path.to_string(),
        ])
        .await?;
        let new_media = self.video().probe(&combined).await?;
        let old_duration = current.duration;
        {
            let mut state = self.inner.state.lock().unwrap();
            let entry = state
                .get_mut(&project_id)
                .and_then(|e| e.project.as_mut())
                .ok_or_else(|| CoreError::NotFound(format!("project {project_id}")))?;
            entry.timeline.duration = new_media.duration;
            entry.timeline.clips.push(crate::model::Clip {
                id: uuid::Uuid::new_v4(),
                source_in: old_duration,
                source_out: new_media.duration,
                included: true,
                origin: crate::model::ClipOrigin::Manual,
                order: entry.timeline.clips.len() as u32,
                linked_segment_ids: vec![],
            });
            entry.timeline.normalize();
            entry.media = Some(new_media.clone());
            self.inner.store.save_project(entry)?;
        }
        crate::events::send(
            &self.sink(),
            crate::events::CoreEvent::TimelineChanged { project_id },
        );
        crate::events::send(
            &self.sink(),
            crate::events::CoreEvent::MediaAssetsChanged { project_id },
        );
        Ok(new_media)
    }
}

impl super::Editor {
    /// Attach the dual-capture screen file (probed) to a project.
    pub async fn attach_screen_media(&self, project_id: uuid::Uuid, file_path: &str) -> crate::error::Result<()> {
        let media = self.video().probe(file_path).await?;
        self.ensure_loaded(project_id)?;
        let mut state = self.inner.state.lock().unwrap();
        let entry = state
            .get_mut(&project_id)
            .and_then(|e| e.project.as_mut())
            .ok_or_else(|| crate::error::CoreError::NotFound(format!("project {project_id}")))?;
        entry.screen_media = Some(media);
        self.inner.store.save_project(entry)?;
        Ok(())
    }
}
