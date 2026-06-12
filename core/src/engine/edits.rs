//! THE mutation seam. Every edit — UI, local agent, or MCP client — is an
//! [`EditOp`] applied through [`Editor::apply_edit`]: frame-snap → apply →
//! record a [`JournalEntry`] carrying the exact inverse and redo → persist →
//! notify. Undo/redo replay the same `apply_op` the edit used, so history can
//! never drift from the original edit.

use super::{EditOutcome, Editor, JournalEntry, ProjectState};
use crate::detect;
use crate::error::{CoreError, Result};
use crate::events::{send, CoreEvent};
use crate::model::*;
use crate::time::snap_to_frame;
use chrono::Utc;
use uuid::Uuid;

const UNDO_CAP: usize = 100;

impl Editor {
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
            // Bounds validation for range ops BEFORE mutating: clamped
            // garbage (start>end, both ends past EOF) used to no-op
            // silently, journal a phantom entry, and report success — the
            // exact false-positive a small model cannot repair from.
            if let EditOp::CutRange { start, end } | EditOp::RestoreRange { start, end } = &op {
                let dur = project.timeline.duration;
                if *start >= *end || *start < 0.0 || *start >= dur {
                    return Err(CoreError::InvalidArg(format!(
                        "range {:.2}s–{:.2}s is invalid: need 0 <= start < end and start < {dur:.2}s (the source duration)",
                        start, end
                    )));
                }
            }
            apply_op(
                project,
                transcript.as_mut(),
                &op,
                &prefs,
                &mut split_ids,
                self.inner.rough_cut_peaks.lock().unwrap().take().as_deref(),
            )?;
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
    ) -> Result<(EditAction, Timeline, u32)> {
        self.ensure_loaded(project_id)?;
        if self.get_transcript(project_id)?.is_none() {
            self.transcribe(project_id, None).await?;
        }
        let prefs = self.inner.store.load_preferences()?;
        let aggr = aggressiveness.unwrap_or(prefs.cut_aggressiveness);
        // Hybrid silences: stage raw audio peaks for the RoughCut apply
        // (best effort — no ffmpeg means transcript-only detection).
        let media = self.with_project(project_id, |p, _| Ok(p.media.clone()))?;
        if let Some(media) = media {
            let peaks = crate::adapters::video::load_raw_peaks(&media).await;
            *self.inner.rough_cut_peaks.lock().unwrap() = peaks;
        }
        let outcome = self.apply_edit(project_id, EditOp::RoughCut { aggressiveness: aggr }, source)?;
        let cut_count = outcome.timeline.cut_count;
        Ok((outcome.action, outcome.timeline, cut_count))
    }

    /// Undo a specific action set (one chat turn) — ONLY when they are
    /// still the newest journal entries, so a stale button can never revert
    /// unrelated later work.
    pub fn undo_actions(&self, project_id: Uuid, ids: &[Uuid]) -> Result<(usize, Timeline)> {
        if ids.is_empty() {
            return Err(CoreError::InvalidArg("action_ids must be non-empty".into()));
        }
        self.ensure_loaded(project_id)?;
        {
            let state = self.inner.state.lock().unwrap();
            let entry = state
                .get(&project_id)
                .ok_or_else(|| CoreError::NotFound(format!("project {project_id}")))?;
            let top: Vec<Uuid> =
                entry.undo.iter().rev().take(ids.len()).map(|j| j.action.id).collect();
            let want: std::collections::HashSet<Uuid> = ids.iter().copied().collect();
            if top.len() != ids.len() || !top.iter().all(|id| want.contains(id)) {
                return Err(CoreError::InvalidArg(
                    "those actions are no longer the newest edits — undo them from the \
                     top bar instead (newer changes exist)"
                        .into(),
                ));
            }
        }
        let mut timeline = self.get_timeline(project_id)?;
        for _ in 0..ids.len() {
            let (_, tl) = self.step_history(project_id, true)?;
            timeline = tl;
        }
        Ok((ids.len(), timeline))
    }

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
                    apply_op(project, transcript.as_mut(), replay, &prefs, &mut vec![], None)?;
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
    // Raw audio peaks for RoughCut's hybrid silence refinement; None for
    // journal replays (snapshots replay exactly, never re-detect).
    rough_cut_peaks: Option<&[u8]>,
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
                rough_cut_peaks,
                crate::adapters::video::PEAKS_PER_SECOND,
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
