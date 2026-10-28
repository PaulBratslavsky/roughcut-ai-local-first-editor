//! Saved clips on the Editor: candidate storage and clip CRUD. Saved clips
//! are project metadata — persisted, NOT journaled (like checkpoints and
//! layout): undo/redo moves the main cut, never a saved clip.

use super::Editor;
use crate::error::{CoreError, Result};
use crate::model::{
    ClipCandidate, ClipFormat, Project, SavedClip, SavedClipSummary, SourceRange, EPS,
};
use crate::time::snap_to_frame;
use chrono::Utc;
use serde::Serialize;
use uuid::Uuid;

/// Frames a face pass looks at, at most, and how wide each grab is.
const FACE_SAMPLES: usize = 12;
const FACE_FRAME_WIDTH: u32 = 640;

/// What a clip is cut from: a `find_clips` candidate, or pieces of the source.
pub enum ClipSource {
    Candidate(Uuid),
    Ranges(Vec<SourceRange>),
}

impl Editor {
    /// Mutate project metadata in place, persist it, and tell every view.
    fn update_project_meta<T>(
        &self,
        project_id: Uuid,
        f: impl FnOnce(&mut Project) -> Result<T>,
    ) -> Result<T> {
        self.ensure_loaded(project_id)?;
        let mut state = self.inner.state.lock().unwrap();
        let project = state
            .get_mut(&project_id)
            .and_then(|e| e.project.as_mut())
            .ok_or_else(|| CoreError::NotFound(format!("project {project_id}")))?;
        let out = f(project)?;
        project.updated_at = Utc::now();
        self.inner.store.save_project(project)?;
        drop(state);
        crate::events::send(&self.sink(), crate::events::CoreEvent::ClipsChanged { project_id });
        Ok(out)
    }

    /// Replace the project's clip candidates (one `find_clips` run).
    pub fn set_clip_candidates(&self, project_id: Uuid, candidates: Vec<ClipCandidate>) -> Result<()> {
        self.update_project_meta(project_id, |p| {
            p.clip_candidates = candidates;
            Ok(())
        })
    }

    pub fn get_clip_candidates(&self, project_id: Uuid) -> Result<Vec<ClipCandidate>> {
        self.with_project(project_id, |p, _| Ok(p.clip_candidates.clone()))
    }

    /// Save a clip from a candidate or from pieces of the source. Its timeline
    /// snapshots the main cut inside the pieces (silences/fillers already
    /// removed stay removed). Saving a candidate takes it off the list.
    /// Format defaults: candidates are shorts (vertical), hand-picked pieces
    /// keep the source frame (landscape).
    pub fn create_clip(
        &self,
        project_id: Uuid,
        from: ClipSource,
        title: Option<&str>,
        format: Option<ClipFormat>,
    ) -> Result<SavedClipSummary> {
        self.update_project_meta(project_id, |p| {
            let mut clip = SavedClip {
                id: Uuid::new_v4(),
                title: String::new(),
                ranges: vec![],
                format: ClipFormat::Landscape,
                crop_x: 0.5,
                start: 0.0,
                end: 0.0,
                timeline: p.timeline.clone(),
                score: None,
                reason: String::new(),
                hook: String::new(),
                created_at: Utc::now(),
            };
            let ranges = match from {
                ClipSource::Candidate(id) => {
                    let idx = p
                        .clip_candidates
                        .iter()
                        .position(|c| c.id == id)
                        .ok_or_else(|| CoreError::NotFound(format!("clip candidate {id}")))?;
                    let c = p.clip_candidates.remove(idx);
                    clip.title = c.title;
                    clip.score = Some(c.score);
                    clip.reason = c.reason;
                    clip.hook = c.hook;
                    clip.format = ClipFormat::Vertical;
                    vec![SourceRange { start: c.start, end: c.end }]
                }
                ClipSource::Ranges(ranges) => ranges,
            };
            set_ranges(p, &mut clip, &ranges)?;
            if let Some(f) = format {
                clip.format = f;
            }
            if let Some(t) = title.map(str::trim).filter(|t| !t.is_empty()) {
                clip.title = t.to_string();
            }
            if clip.title.is_empty() {
                clip.title = format!("Clip {}", p.saved_clips.len() + 1);
            }
            let summary = clip.summary();
            p.saved_clips.push(clip);
            Ok(summary)
        })
    }

    /// Saved clips in creation order.
    pub fn list_clips(&self, project_id: Uuid) -> Result<Vec<SavedClipSummary>> {
        self.with_project(project_id, |p, _| Ok(p.saved_clips.iter().map(SavedClip::summary).collect()))
    }

    /// The full clip, timeline included (for export and preview).
    pub fn get_clip(&self, project_id: Uuid, clip_id: Uuid) -> Result<SavedClip> {
        self.with_project(project_id, |p, _| {
            p.saved_clips
                .iter()
                .find(|c| c.id == clip_id)
                .cloned()
                .ok_or_else(|| CoreError::NotFound(format!("clip {clip_id}")))
        })
    }

    /// Rename, reformat, re-crop, and/or replace a clip's pieces. New pieces
    /// re-snapshot the CURRENT main cut inside them.
    pub fn update_clip(
        &self,
        project_id: Uuid,
        clip_id: Uuid,
        changes: ClipChanges,
    ) -> Result<SavedClipSummary> {
        self.update_project_meta(project_id, |p| {
            let idx = p
                .saved_clips
                .iter()
                .position(|c| c.id == clip_id)
                .ok_or_else(|| CoreError::NotFound(format!("clip {clip_id}")))?;
            let mut clip = p.saved_clips[idx].clone();
            if let Some(ranges) = changes.ranges {
                clip.timeline = p.timeline.clone();
                set_ranges(p, &mut clip, &ranges)?;
            }
            if let Some(t) = changes.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                clip.title = t.to_string();
            }
            if let Some(f) = changes.format {
                clip.format = f;
            }
            if let Some(x) = changes.crop_x {
                clip.crop_x = x.clamp(0.0, 1.0);
            }
            let summary = clip.summary();
            p.saved_clips[idx] = clip;
            Ok(summary)
        })
    }

    pub fn delete_clip(&self, project_id: Uuid, clip_id: Uuid) -> Result<()> {
        self.update_project_meta(project_id, |p| {
            let before = p.saved_clips.len();
            p.saved_clips.retain(|c| c.id != clip_id);
            if p.saved_clips.len() == before {
                return Err(CoreError::NotFound(format!("clip {clip_id}")));
            }
            Ok(())
        })
    }
}

/// What a face pass found for one clip.
#[derive(Debug, Clone, Serialize)]
pub struct FaceCrop {
    pub clip: SavedClipSummary,
    /// The crop it set; None when too few frames showed a face (crop unchanged).
    pub crop_x: Option<f64>,
    pub frames_sampled: usize,
    pub frames_with_face: usize,
}

impl Editor {
    /// Point a clip's 9:16 crop at the speaker: sample frames across its kept
    /// footage, find the faces, centre the crop on the median speaker
    /// position. Leaves the crop alone when it can't see a face.
    pub async fn center_clip_on_face(&self, project_id: Uuid, clip_id: Uuid) -> Result<FaceCrop> {
        self.face_crop(project_id, clip_id, None).await
    }

    /// The same pass in the background, for a clip that just became 9:16. It
    /// only moves the crop if nobody changed it meanwhile (a drag in the
    /// preview wins). Failures are silent: the clip keeps its crop.
    pub fn auto_center_clip(&self, project_id: Uuid, clip_id: Uuid) {
        let Ok(clip) = self.get_clip(project_id, clip_id) else { return };
        let editor = self.clone();
        tokio::spawn(async move {
            let _ = editor.face_crop(project_id, clip_id, Some(clip.crop_x)).await;
        });
    }

    async fn face_crop(&self, project_id: Uuid, clip_id: Uuid, expect: Option<f64>) -> Result<FaceCrop> {
        use crate::adapters::face;
        let clip = self.get_clip(project_id, clip_id)?;
        let project = self.with_project(project_id, |p, _| Ok(p.clone()))?;
        if project.screen_media.is_some() {
            // The 9:16 crop applies to the composite (screen + camera
            // bubble), which isn't a frame we can grab from one file.
            return Err(CoreError::InvalidArg(
                "this project lays a camera bubble over a screen recording, so the face can't be found \
                 automatically: drag the 9:16 crop in the preview instead"
                    .into(),
            ));
        }
        let media = project
            .media
            .clone()
            .ok_or_else(|| CoreError::InvalidArg("the project has no media to look at".into()))?;
        let kept: Vec<(f64, f64)> =
            clip.timeline.included_clips().map(|c| (c.source_in, c.source_out)).collect();
        let times = face::sample_times(&kept, FACE_SAMPLES);

        let dir = crate::store::data_dir().join("cache").join("faces").join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(&dir)?;
        let found = self.find_faces(&media, &times, &dir).await;
        let _ = std::fs::remove_dir_all(&dir);
        let (frames, size) = found?;

        let (w, h) = size.unwrap_or((media.width, media.height));
        let crop_x = face::crop_x_for_faces(&frames, w, h);
        let frames_with_face = frames.iter().filter(|f| face::subject(f).is_some()).count();
        let clip = match crop_x {
            Some(x) => self.set_crop_if(project_id, clip_id, x, expect)?,
            None => clip.summary(),
        };
        Ok(FaceCrop { clip, crop_x, frames_sampled: frames.len(), frames_with_face })
    }

    /// Grab a frame at each time and run face detection on it. Also returns
    /// the grabbed frame size (what the render will crop).
    async fn find_faces(
        &self,
        media: &crate::model::Media,
        times: &[f64],
        dir: &std::path::Path,
    ) -> Result<(Vec<Vec<crate::adapters::face::FaceBox>>, Option<(u32, u32)>)> {
        use crate::adapters::face;
        let mut frames = Vec::with_capacity(times.len());
        let mut size = None;
        for (i, &t) in times.iter().enumerate() {
            let path = dir.join(format!("frame_{i:02}.png"));
            self.video().extract_frame(media, t, FACE_FRAME_WIDTH, &path.to_string_lossy()).await?;
            if size.is_none() {
                size = face::png_size(&path).ok();
            }
            let faces = tokio::task::spawn_blocking(move || face::detect_faces(&path))
                .await
                .map_err(|e| CoreError::Other(format!("face detection task failed: {e}")))??;
            frames.push(faces);
        }
        Ok((frames, size))
    }

    /// Set a clip's crop, unless `expect` is given and the crop has moved
    /// away from it since (then the clip is returned unchanged).
    fn set_crop_if(&self, project_id: Uuid, clip_id: Uuid, x: f64, expect: Option<f64>) -> Result<SavedClipSummary> {
        self.update_project_meta(project_id, |p| {
            let clip = p
                .saved_clips
                .iter_mut()
                .find(|c| c.id == clip_id)
                .ok_or_else(|| CoreError::NotFound(format!("clip {clip_id}")))?;
            if expect.map_or(true, |e| (clip.crop_x - e).abs() < 1e-9) {
                clip.crop_x = x.clamp(0.0, 1.0);
            }
            Ok(clip.summary())
        })
    }
}

/// Partial update for [`Editor::update_clip`]; `None` leaves a field alone.
#[derive(Default)]
pub struct ClipChanges {
    pub title: Option<String>,
    pub ranges: Option<Vec<SourceRange>>,
    pub format: Option<ClipFormat>,
    pub crop_x: Option<f64>,
}

/// Validate, frame-snap, sort, and merge the pieces, then rebuild the clip's
/// timeline from `clip.timeline` (the main cut it was seeded with).
fn set_ranges(p: &Project, clip: &mut SavedClip, ranges: &[SourceRange]) -> Result<()> {
    if ranges.is_empty() {
        return Err(CoreError::InvalidArg("a clip needs at least one range".into()));
    }
    let fps = p.fps();
    let dur = p.timeline.duration;
    let mut pieces: Vec<SourceRange> = vec![];
    for r in ranges {
        let (start, end) = (snap_to_frame(r.start.max(0.0), fps), snap_to_frame(r.end.min(dur), fps));
        if !(start < end) || start >= dur {
            return Err(CoreError::InvalidArg(format!(
                "range {:.2}s–{:.2}s is invalid: need 0 <= start < end and start < {dur:.2}s (the source duration)",
                r.start, r.end
            )));
        }
        pieces.push(SourceRange { start, end });
    }
    pieces.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
    // Touching or overlapping pieces are one piece.
    let mut merged: Vec<SourceRange> = vec![];
    for r in pieces {
        match merged.last_mut() {
            Some(last) if r.start <= last.end + EPS => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    let timeline = clip.timeline.restricted_to(&merged);
    if timeline.included_duration() < 1.0 {
        return Err(CoreError::InvalidArg(
            "those ranges have under a second of kept footage in the current cut".into(),
        ));
    }
    clip.start = merged.first().map(|r| r.start).unwrap_or(0.0);
    clip.end = merged.last().map(|r| r.end).unwrap_or(0.0);
    clip.ranges = merged;
    clip.timeline = timeline;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn background_face_pass_never_overrides_a_hand_set_crop() {
        let e = Editor::test_instance();
        let p = e.create_project("t", Some("/demo/footage.mp4")).await.unwrap();
        let ranges = vec![SourceRange { start: 10.0, end: 40.0 }];
        let c = e.create_clip(p.id, ClipSource::Ranges(ranges), None, Some(ClipFormat::Vertical)).unwrap();
        assert_eq!(c.crop_x, 0.5);

        // The pass started at 0.5; the user dragged the crop to 0.2 meanwhile.
        e.update_clip(p.id, c.id, ClipChanges { crop_x: Some(0.2), ..Default::default() }).unwrap();
        assert_eq!(e.set_crop_if(p.id, c.id, 0.66, Some(0.5)).unwrap().crop_x, 0.2);

        // Untouched since the pass started: it lands. The explicit button
        // (no expectation) always lands.
        assert_eq!(e.set_crop_if(p.id, c.id, 0.3, Some(0.2)).unwrap().crop_x, 0.3);
        assert_eq!(e.set_crop_if(p.id, c.id, 0.66, None).unwrap().crop_x, 0.66);
    }
}
