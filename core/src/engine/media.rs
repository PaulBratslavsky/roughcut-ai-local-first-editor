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
        let mut media = self.inner.video.probe(file_path).await?;
        if let Some(pid) = project_id {
            self.ensure_loaded(pid)?;
            {
                let mut state = self.inner.state.lock().unwrap();
                let entry =
                    state.get_mut(&pid).ok_or_else(|| CoreError::NotFound("project".into()))?;
                let project =
                    entry.project.as_mut().ok_or_else(|| CoreError::NotFound("project".into()))?;
                match &project.media {
                    // The same video at a new path (moved or renamed): relink.
                    // Cuts, history, transcript and clips all still apply.
                    Some(current) if same_source(current, &media) => media.id = current.id,
                    // A different video would leave every cut, undo step,
                    // checkpoint, transcript and saved clip describing the
                    // old one — refuse rather than silently invalidate them.
                    Some(current) => {
                        return Err(CoreError::InvalidArg(format!(
                            "this project already has a different video ({:.1}s, {}x{}); \
                             create a new project for it, or use append_media to add it to the end",
                            current.duration, current.width, current.height
                        )))
                    }
                    None => project.timeline = Timeline::new(media.duration),
                }
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
        let span = crate::model::Clip {
            id: uuid::Uuid::new_v4(),
            source_in: current.duration,
            source_out: new_media.duration,
            included: true,
            origin: crate::model::ClipOrigin::Manual,
            order: 0,
            linked_segment_ids: vec![],
        };
        {
            let mut state = self.inner.state.lock().unwrap();
            let entry = state
                .get_mut(&project_id)
                .ok_or_else(|| CoreError::NotFound(format!("project {project_id}")))?;
            extend_history(entry, &span);
            let project = entry
                .project
                .as_mut()
                .ok_or_else(|| CoreError::NotFound(format!("project {project_id}")))?;
            project.timeline.duration = new_media.duration;
            push_span(&mut project.timeline.clips, &span);
            project.timeline.normalize();
            project.media = Some(new_media.clone());
            self.inner.store.save_project(project)?;
            self.persist_journal(project_id, &entry.undo, &entry.redo)?;
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

/// Is `b` the same video as `a` (e.g. the file was moved)? Duration, frame
/// size and frame rate must match.
fn same_source(a: &Media, b: &Media) -> bool {
    (a.duration - b.duration).abs() < 0.05
        && a.width == b.width
        && a.height == b.height
        && (a.frame_rate - b.frame_rate).abs() < 0.01
}

/// Append `span` (an included clip over newly appended footage) to a clip list.
fn push_span(clips: &mut Vec<Clip>, span: &Clip) {
    let mut clip = span.clone();
    clip.order = clips.len() as u32;
    clips.push(clip);
}

/// Undo/redo entries and checkpoints hold full clip snapshots of the source
/// as it was. After an append the source is longer, so each snapshot gets the
/// same new span — otherwise an undo or restore would leave the appended
/// footage outside the timeline's clips.
fn extend_history(entry: &mut super::ProjectState, span: &Clip) {
    for journal in entry.undo.iter_mut().chain(entry.redo.iter_mut()) {
        for op in [&mut journal.inverse, &mut journal.redo] {
            if let EditOp::SetClips { clips, .. } = op {
                push_span(clips, span);
            }
        }
    }
    if let Some(project) = entry.project.as_mut() {
        for checkpoint in &mut project.checkpoints {
            push_span(&mut checkpoint.clips, span);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{extend_history, same_source};
    use crate::engine::{JournalEntry, ProjectState};
    use crate::model::*;

    fn media(duration: f64, width: u32) -> Media {
        Media {
            id: uuid::Uuid::new_v4(),
            file_path: "/x.mp4".into(),
            duration,
            frame_rate: 30.0,
            width,
            height: 1080,
            audio_sample_rate: 48000,
            codec: "h264".into(),
            imported_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn same_source_needs_matching_duration_size_and_rate() {
        assert!(same_source(&media(120.0, 1920), &media(120.02, 1920)));
        assert!(!same_source(&media(120.0, 1920), &media(95.0, 1920)));
        assert!(!same_source(&media(120.0, 1920), &media(120.0, 3840)));
    }

    #[test]
    fn append_extends_every_snapshot_to_cover_the_new_footage() {
        let mut timeline = Timeline::new(100.0);
        timeline.set_range_included(10.0, 20.0, false, ClipOrigin::Manual);
        let before = timeline.clips.clone();
        let mut project = Project::new("p");
        project.timeline = timeline.clone();
        project.checkpoints.push(Checkpoint {
            id: uuid::Uuid::new_v4(),
            name: "cp".into(),
            created_at: chrono::Utc::now(),
            clips: before.clone(),
            global_padding: Padding::default(),
            cut_count: 1,
            auto: false,
        });
        let snapshot = |clips: Vec<Clip>| EditOp::SetClips { clips, global_padding: Padding::default() };
        let mut entry = ProjectState {
            project: Some(project),
            transcript: None,
            undo: vec![JournalEntry {
                action: EditAction::new(ActionSource::Ui, EditOp::CutRange { start: 10.0, end: 20.0 }),
                inverse: snapshot(Timeline::new(100.0).clips),
                redo: snapshot(before),
            }],
            redo: vec![],
        };
        let span = Clip {
            id: uuid::Uuid::new_v4(),
            source_in: 100.0,
            source_out: 160.0,
            included: true,
            order: 0,
            origin: ClipOrigin::Manual,
            linked_segment_ids: vec![],
        };
        extend_history(&mut entry, &span);

        // Restoring any snapshot over the 160s source keeps the appended
        // minute and still partitions the whole source.
        let covered = |clips: &[Clip]| {
            let mut t = Timeline::new(160.0);
            t.clips = clips.to_vec();
            t.normalize();
            assert_eq!(t.clips.last().unwrap().source_out, 160.0);
            assert!(t.clips.windows(2).all(|w| (w[0].source_out - w[1].source_in).abs() < 1e-9));
            t.included_duration()
        };
        let EditOp::SetClips { clips, .. } = &entry.undo[0].inverse else { unreachable!() };
        assert_eq!(covered(clips), 160.0, "undo to the original keeps the new footage");
        let EditOp::SetClips { clips, .. } = &entry.undo[0].redo else { unreachable!() };
        assert_eq!(covered(clips), 150.0, "the cut survives, plus the new footage");
        let cp = &entry.project.as_ref().unwrap().checkpoints[0];
        assert_eq!(covered(&cp.clips), 150.0);
    }
}
