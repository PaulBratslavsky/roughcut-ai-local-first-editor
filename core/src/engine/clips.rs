//! Saved clips on the Editor: candidate storage and clip CRUD. Saved clips
//! are project metadata — persisted, NOT journaled (like checkpoints and
//! layout): undo/redo moves the main cut, never a saved clip.

use super::Editor;
use crate::error::{CoreError, Result};
use crate::model::{
    pieces_overlap, ClipBatchError, ClipCandidate, ClipFormat, ClipFraming, Project, SavedClip,
    SavedClipSummary, SourceRange, EPS,
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

    /// Replace the candidates with a finished run's. With `drop_saved`, also
    /// drop any that are the same moment (sharing more than that fraction) as
    /// a clip saved now — including one saved while the run was going.
    /// Returns what was written.
    pub fn replace_clip_candidates(
        &self,
        project_id: Uuid,
        candidates: Vec<ClipCandidate>,
        drop_saved: Option<f64>,
    ) -> Result<Vec<ClipCandidate>> {
        self.update_project_meta(project_id, |p| {
            let kept: Vec<ClipCandidate> = candidates
                .into_iter()
                .filter(|c| {
                    drop_saved.map_or(true, |max| {
                        p.saved_clips.iter().all(|s| pieces_overlap(&c.pieces, &s.ranges) <= max)
                    })
                })
                .collect();
            p.clip_candidates = kept.clone();
            Ok(kept)
        })
    }

    /// Claim the project's one `find_clips` slot; dropping the guard frees it.
    pub fn begin_clip_scan(&self, project_id: Uuid) -> Result<ClipScan> {
        if !self.inner.scans.lock().unwrap().insert(project_id) {
            return Err(CoreError::InvalidArg("a clip scan is already running for this project".into()));
        }
        Ok(ClipScan { editor: self.clone(), project_id })
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
        // A new clip snapshots the main cut, which is set aside meanwhile.
        self.require_no_clip_session(project_id, "making a clip")?;
        self.update_project_meta(project_id, |p| {
            let (mut clip, from_candidate) = match from {
                ClipSource::Candidate(id) => {
                    let c = p
                        .clip_candidates
                        .iter()
                        .find(|c| c.id == id)
                        .cloned()
                        .ok_or_else(|| CoreError::NotFound(format!("clip candidate {id}")))?;
                    (clip_from_candidate(p, &c)?, Some(id))
                }
                ClipSource::Ranges(ranges) => {
                    let mut clip = blank_clip(p);
                    set_ranges(p, &mut clip, &ranges)?;
                    (clip, None)
                }
            };
            if let Some(f) = format {
                clip.format = f;
            }
            if let Some(t) = title.map(str::trim).filter(|t| !t.is_empty()) {
                clip.title = t.to_string();
            }
            Ok(add_clip(p, clip, from_candidate))
        })
    }

    /// Make several candidates into saved clips, then frame each vertical one
    /// on the speaker before returning (one the user framed on the Shorts
    /// screen keeps that framing instead) (so an export right after never renders
    /// an unframed crop). A candidate that no longer validates — its material
    /// was cut from the main edit since the scan — stays listed and is
    /// reported in the errors; the rest are still made.
    pub async fn create_clips(
        &self,
        project_id: Uuid,
        candidate_ids: &[Uuid],
        format: Option<ClipFormat>,
    ) -> Result<(Vec<SavedClipSummary>, Vec<ClipBatchError>)> {
        self.require_no_clip_session(project_id, "making shorts")?;
        let (made, errors, framed) = self.update_project_meta(project_id, |p| {
            let mut made: Vec<SavedClipSummary> = vec![];
            let mut errors: Vec<ClipBatchError> = vec![];
            // Clips whose candidate the user framed: they keep that framing.
            let mut framed: Vec<Uuid> = vec![];
            for &id in candidate_ids {
                let Some(c) = p.clip_candidates.iter().find(|c| c.id == id).cloned() else {
                    errors.push(ClipBatchError {
                        candidate_id: id,
                        message: "not in the candidate list (made already, or replaced by a newer scan)".into(),
                    });
                    continue;
                };
                match clip_from_candidate(p, &c) {
                    Ok(mut clip) => {
                        if let Some(f) = format {
                            clip.format = f;
                        }
                        if c.framing.is_some() {
                            framed.push(clip.id);
                        }
                        made.push(add_clip(p, clip, Some(id)));
                    }
                    Err(e) => errors.push(ClipBatchError { candidate_id: id, message: e.to_string() }),
                }
            }
            Ok((made, errors, framed))
        })?;

        let vertical: Vec<&SavedClipSummary> =
            made.iter().filter(|c| c.format == ClipFormat::Vertical && !framed.contains(&c.id)).collect();
        let n = vertical.len();
        for (i, clip) in vertical.iter().enumerate() {
            make_progress(self, project_id, i as f64 / n as f64, format!("framing {}/{n}", i + 1));
            // A clip it can't frame (no confident face, a composited screen
            // recording, no ffmpeg) keeps its centred crop: not an error.
            let to = ClipFraming { crop_x: clip.crop_x, crop_y: clip.crop_y, crop_zoom: clip.crop_zoom };
            let _ = self.face_crop(project_id, clip.id, Some(to)).await;
        }
        if !made.is_empty() {
            let word = if made.len() == 1 { "short" } else { "shorts" };
            make_progress(self, project_id, 1.0, format!("made {} {word}", made.len()));
        }
        // Re-read: framing moved the crops. A clip deleted meanwhile is
        // simply gone from the result; the rest of the batch still reports.
        let made = made.iter().filter_map(|c| self.get_clip(project_id, c.id).ok().map(|clip| clip.summary())).collect();
        Ok((made, errors))
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
        if changes.ranges.is_some() {
            // New pieces re-snapshot the main cut, set aside meanwhile.
            self.require_no_clip_session(project_id, "changing a clip's pieces")?;
        }
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
            let f = clip.framing();
            clip.set_framing(ClipFraming {
                crop_x: changes.crop_x.unwrap_or(f.crop_x),
                crop_y: changes.crop_y.unwrap_or(f.crop_y),
                crop_zoom: changes.crop_zoom.unwrap_or(f.crop_zoom),
            });
            let summary = clip.summary();
            p.saved_clips[idx] = clip;
            Ok(summary)
        })
    }

    pub fn delete_clip(&self, project_id: Uuid, clip_id: Uuid) -> Result<()> {
        if self.clip_session(project_id)?.is_some_and(|s| s.clip_id == clip_id) {
            self.require_no_clip_session(project_id, "deleting it")?;
        }
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
    /// Up and down, for a zoomed-in crop (0.5 when not zoomed); None with `crop_x`.
    pub crop_y: Option<f64>,
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
            let _ = editor.face_crop(project_id, clip_id, Some(clip.framing())).await;
        });
    }

    /// Frame a candidate's 9:16 crop before it's made (the Shorts screen's
    /// drag), or with `None` go back to framing it on the speaker when made.
    pub fn frame_candidate(
        &self,
        project_id: Uuid,
        candidate_id: Uuid,
        framing: Option<ClipFraming>,
    ) -> Result<ClipCandidate> {
        self.update_project_meta(project_id, |p| {
            let c = p
                .clip_candidates
                .iter_mut()
                .find(|c| c.id == candidate_id)
                .ok_or_else(|| CoreError::NotFound(format!("clip candidate {candidate_id}")))?;
            c.framing = framing.map(ClipFraming::clamped);
            Ok(c.clone())
        })
    }

    async fn face_crop(&self, project_id: Uuid, clip_id: Uuid, expect: Option<ClipFraming>) -> Result<FaceCrop> {
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

        let dir = crate::asset_cache::root().join(crate::asset_cache::FACES).join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(&dir)?;
        let found = self.find_faces(&media, &times, &dir).await;
        let _ = std::fs::remove_dir_all(&dir);
        let (frames, size) = found?;

        let (w, h) = size.unwrap_or((media.width, media.height));
        // Keeps the clip's zoom; moves the window onto the speaker.
        let found = face::crop_for_faces(&frames, w, h, clip.crop_zoom);
        let frames_with_face = frames.iter().filter(|f| face::subject(f).is_some()).count();
        let clip = match found {
            Some((x, y)) => {
                let to = ClipFraming { crop_x: x, crop_y: y, ..clip.framing() };
                self.set_crop_if(project_id, clip_id, to, expect)?
            }
            None => clip.summary(),
        };
        Ok(FaceCrop {
            clip,
            crop_x: found.map(|f| f.0),
            crop_y: found.map(|f| f.1),
            frames_sampled: frames.len(),
            frames_with_face,
        })
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

    /// Set a clip's framing, unless `expect` is given and the framing has
    /// changed since (then the clip is returned unchanged).
    fn set_crop_if(
        &self,
        project_id: Uuid,
        clip_id: Uuid,
        to: ClipFraming,
        expect: Option<ClipFraming>,
    ) -> Result<SavedClipSummary> {
        self.update_project_meta(project_id, |p| {
            let clip = p
                .saved_clips
                .iter_mut()
                .find(|c| c.id == clip_id)
                .ok_or_else(|| CoreError::NotFound(format!("clip {clip_id}")))?;
            let now = clip.framing();
            let unchanged = |e: ClipFraming| {
                (now.crop_x - e.crop_x).abs() < 1e-9
                    && (now.crop_y - e.crop_y).abs() < 1e-9
                    && (now.crop_zoom - e.crop_zoom).abs() < 1e-9
            };
            if expect.is_none_or(unchanged) {
                clip.set_framing(to);
            }
            Ok(clip.summary())
        })
    }
}

/// Holds a project's `find_clips` slot (see [`Editor::begin_clip_scan`]).
pub struct ClipScan {
    editor: Editor,
    project_id: Uuid,
}

impl Drop for ClipScan {
    fn drop(&mut self) {
        self.editor.inner.scans.lock().unwrap().remove(&self.project_id);
    }
}

fn make_progress(editor: &Editor, project_id: Uuid, fraction: f64, message: String) {
    crate::events::send(
        &editor.sink(),
        crate::events::CoreEvent::progress(crate::events::ProgressTask::MakeClips, Some(project_id), fraction, message),
    );
}

/// A clip with nothing in it yet, seeded with the current main cut.
fn blank_clip(p: &Project) -> SavedClip {
    SavedClip {
        id: Uuid::new_v4(),
        title: String::new(),
        ranges: vec![],
        format: ClipFormat::Landscape,
        crop_x: 0.5,
        crop_y: 0.5,
        crop_zoom: 1.0,
        start: 0.0,
        end: 0.0,
        timeline: p.timeline.clone(),
        score: None,
        reason: String::new(),
        hook: String::new(),
        created_at: Utc::now(),
    }
}

/// The clip a candidate makes (a vertical short), validated against the
/// current cut. Doesn't touch the project: the caller adds it and takes the
/// candidate off the list only once this succeeded.
fn clip_from_candidate(p: &Project, c: &ClipCandidate) -> Result<SavedClip> {
    let mut clip = blank_clip(p);
    clip.title = c.title.clone();
    clip.score = Some(c.score);
    clip.reason = c.reason.clone();
    clip.hook = c.hook.clone();
    clip.format = ClipFormat::Vertical;
    if let Some(f) = c.framing {
        clip.set_framing(f);
    }
    let pieces =
        if c.pieces.is_empty() { vec![SourceRange { start: c.start, end: c.end }] } else { c.pieces.clone() };
    set_ranges(p, &mut clip, &pieces)?;
    Ok(clip)
}

/// Add a validated clip (naming an untitled one) and take the candidate it
/// came from, if any, off the list.
fn add_clip(p: &mut Project, mut clip: SavedClip, from_candidate: Option<Uuid>) -> SavedClipSummary {
    if let Some(id) = from_candidate {
        p.clip_candidates.retain(|c| c.id != id);
    }
    if clip.title.is_empty() {
        clip.title = format!("Clip {}", p.saved_clips.len() + 1);
    }
    let summary = clip.summary();
    p.saved_clips.push(clip);
    summary
}

/// Partial update for [`Editor::update_clip`]; `None` leaves a field alone.
#[derive(Default)]
pub struct ClipChanges {
    pub title: Option<String>,
    pub ranges: Option<Vec<SourceRange>>,
    pub format: Option<ClipFormat>,
    pub crop_x: Option<f64>,
    pub crop_y: Option<f64>,
    pub crop_zoom: Option<f64>,
}

impl ClipChanges {
    /// It moves or zooms the crop.
    pub fn reframes(&self) -> bool {
        self.crop_x.is_some() || self.crop_y.is_some() || self.crop_zoom.is_some()
    }
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

    fn candidate(start: f64, end: f64) -> ClipCandidate {
        ClipCandidate {
            id: Uuid::new_v4(),
            start,
            end,
            pieces: vec![SourceRange { start, end }],
            duration: end - start,
            title: "t".into(),
            hook: "h".into(),
            reason: "r".into(),
            score: 50.0,
            segment_ids: vec![],
            method: "heuristic".into(),
            theme: None,
            scorer: None,
            framing: None,
        }
    }

    #[tokio::test]
    async fn a_scan_result_drops_a_moment_saved_while_it_ran() {
        let e = Editor::test_instance();
        let p = e.create_project("t", Some("/demo/footage.mp4")).await.unwrap();
        // Saved after the scan read the project, before it wrote its list.
        e.create_clip(p.id, ClipSource::Ranges(vec![SourceRange { start: 10.0, end: 30.0 }]), None, None).unwrap();
        let found = vec![candidate(12.0, 32.0), candidate(40.0, 60.0)];
        let kept = e.replace_clip_candidates(p.id, found.clone(), Some(0.3)).unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].start, 40.0);
        assert_eq!(e.get_clip_candidates(p.id).unwrap().len(), 1);
        // A plain run keeps everything.
        assert_eq!(e.replace_clip_candidates(p.id, found, None).unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_candidate_that_no_longer_validates_stays_listed() {
        let e = Editor::test_instance();
        let p = e.create_project("t", Some("/demo/footage.mp4")).await.unwrap();
        // Past the end of the source: it can't become a clip.
        let bad = candidate(1.0e7, 1.0e7 + 20.0);
        let good = candidate(10.0, 30.0);
        e.set_clip_candidates(p.id, vec![bad.clone(), good.clone()]).unwrap();
        assert!(e.create_clip(p.id, ClipSource::Candidate(bad.id), None, None).is_err());
        assert_eq!(e.get_clip_candidates(p.id).unwrap().len(), 2, "a failed make removes nothing");
        let (made, errors) = e.create_clips(p.id, &[bad.id, good.id, Uuid::new_v4()], None).await.unwrap();
        assert_eq!(made.len(), 1);
        assert_eq!(made[0].format, ClipFormat::Vertical);
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].candidate_id, bad.id);
        let left = e.get_clip_candidates(p.id).unwrap();
        assert_eq!(left.iter().map(|c| c.id).collect::<Vec<_>>(), vec![bad.id]);
    }

    #[tokio::test]
    async fn background_face_pass_never_overrides_a_hand_set_crop() {
        let e = Editor::test_instance();
        let p = e.create_project("t", Some("/demo/footage.mp4")).await.unwrap();
        let ranges = vec![SourceRange { start: 10.0, end: 40.0 }];
        let c = e.create_clip(p.id, ClipSource::Ranges(ranges), None, Some(ClipFormat::Vertical)).unwrap();
        assert_eq!(c.crop_x, 0.5);

        let at = |x: f64| ClipFraming { crop_x: x, ..ClipFraming::CENTRED };
        // The pass started at 0.5; the user dragged the crop to 0.2 meanwhile.
        e.update_clip(p.id, c.id, ClipChanges { crop_x: Some(0.2), ..Default::default() }).unwrap();
        assert_eq!(e.set_crop_if(p.id, c.id, at(0.66), Some(at(0.5))).unwrap().crop_x, 0.2);

        // Untouched since the pass started: it lands. The explicit button
        // (no expectation) always lands.
        assert_eq!(e.set_crop_if(p.id, c.id, at(0.3), Some(at(0.2))).unwrap().crop_x, 0.3);
        assert_eq!(e.set_crop_if(p.id, c.id, at(0.66), None).unwrap().crop_x, 0.66);

        // Zoomed in meanwhile (same crop_x): that's a hand-set framing too.
        e.update_clip(p.id, c.id, ClipChanges { crop_zoom: Some(2.0), ..Default::default() }).unwrap();
        let s = e.set_crop_if(p.id, c.id, at(0.1), Some(at(0.66))).unwrap();
        assert_eq!((s.crop_x, s.crop_zoom), (0.66, 2.0));
    }

    #[tokio::test]
    async fn a_crop_zooms_within_bounds_and_moves_up_and_down() {
        let e = Editor::test_instance();
        let p = e.create_project("t", Some("/demo/footage.mp4")).await.unwrap();
        let ranges = vec![SourceRange { start: 10.0, end: 40.0 }];
        let c = e.create_clip(p.id, ClipSource::Ranges(ranges), None, Some(ClipFormat::Vertical)).unwrap();
        assert_eq!((c.crop_y, c.crop_zoom), (0.5, 1.0), "a new clip is the full height, centred");
        let set = |zoom: f64, y: f64| {
            e.update_clip(p.id, c.id, ClipChanges { crop_zoom: Some(zoom), crop_y: Some(y), ..Default::default() })
                .unwrap()
        };
        let s = set(2.0, 0.25);
        assert_eq!((s.crop_x, s.crop_y, s.crop_zoom), (0.5, 0.25, 2.0), "other fields stay");
        let s = set(9.0, 4.0);
        assert_eq!((s.crop_y, s.crop_zoom), (1.0, ClipFraming::MAX_ZOOM), "clamped");
        let s = set(0.2, f64::NAN);
        assert_eq!((s.crop_y, s.crop_zoom), (0.5, 1.0), "never wider than the frame; NaN is centred");
    }

    #[tokio::test]
    async fn a_framed_candidate_makes_a_short_with_that_framing_and_no_face_pass() {
        let e = Editor::test_instance();
        let p = e.create_project("t", Some("/demo/footage.mp4")).await.unwrap();
        let (framed, plain) = (candidate(10.0, 30.0), candidate(40.0, 60.0));
        e.set_clip_candidates(p.id, vec![framed.clone(), plain.clone()]).unwrap();
        let f = ClipFraming { crop_x: 0.2, crop_y: 0.3, crop_zoom: 1.5 };
        let c = e.frame_candidate(p.id, framed.id, Some(f)).unwrap();
        assert_eq!(c.framing, Some(f));
        assert!(e.frame_candidate(p.id, Uuid::new_v4(), Some(f)).is_err(), "no such candidate");
        // Out-of-range values are clamped when stored.
        let wild = ClipFraming { crop_x: -3.0, crop_y: 0.3, crop_zoom: 0.5 };
        assert_eq!(e.frame_candidate(p.id, plain.id, Some(wild)).unwrap().framing, Some(ClipFraming { crop_x: 0.0, crop_y: 0.3, crop_zoom: 1.0 }));
        assert_eq!(e.frame_candidate(p.id, plain.id, None).unwrap().framing, None, "reset");

        let (made, errors) = e.create_clips(p.id, &[framed.id, plain.id], None).await.unwrap();
        assert!(errors.is_empty(), "{errors:?}");
        let by_start = |s: f64| made.iter().find(|c| (c.start - s).abs() < 0.1).unwrap();
        let a = by_start(10.0);
        assert_eq!((a.crop_x, a.crop_y, a.crop_zoom), (0.2, 0.3, 1.5), "the user's framing, untouched");
        let b = by_start(40.0);
        assert_eq!((b.crop_y, b.crop_zoom), (0.5, 1.0), "unframed: full height (a face pass may move it across)");
    }
}
