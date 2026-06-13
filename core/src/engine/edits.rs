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

    /// Cut ONLY the dead-air the audio waveform confirms — never words,
    /// fillers, takes, or order. The single source of truth for silence
    /// removal: both `remove_silences` and story_edit's pre-pass call this,
    /// so the pure path and the narrative pre-pass can never drift.
    /// `transcript_only` candidates are skipped (no audio proof). Returns the
    /// applied actions (empty when there's no audio or no confirmed dead air).
    pub async fn cut_confirmed_silences(
        &self,
        project_id: Uuid,
        source: ActionSource,
    ) -> Result<Vec<EditAction>> {
        self.ensure_loaded(project_id)?;
        // Read inputs, then decode peaks OUTSIDE the state lock (slow).
        let (transcript, duration, media) = self.with_project(project_id, |p, t| {
            Ok((t.cloned(), p.timeline.duration, p.media.clone()))
        })?;
        let (Some(t), Some(media)) = (transcript, media) else { return Ok(vec![]) };
        let Some(peaks) = crate::adapters::video::load_raw_peaks(&media).await else {
            return Ok(vec![]); // no audio energy to confirm against — refuse to guess
        };
        let prefs = self.inner.store.load_preferences()?;
        let min = prefs.silence_min_duration_s.max(0.6);
        let cands = detect::silence_candidates(&t, duration, (min * 0.6).max(0.3));
        let confirmed: Vec<_> = detect::refine_silences(
            &cands,
            Some(&peaks),
            crate::adapters::video::PEAKS_PER_SECOND,
            min,
        )
        .into_iter()
        .filter(|r| r.source == "confirmed")
        .collect();
        let mut actions = vec![];
        for r in confirmed {
            if let Ok(outcome) =
                self.apply_edit(project_id, EditOp::CutRange { start: r.start, end: r.end }, source)
            {
                actions.push(outcome.action);
            }
        }
        Ok(actions)
    }

    pub async fn generate_rough_cut(
        &self,
        project_id: Uuid,
        aggressiveness: Option<Aggressiveness>,
        source: ActionSource,
    ) -> Result<(EditAction, Timeline, u32, u32)> {
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
        // TIER 2 — compute reviewable suggestions (repeated takes + semantic
        // repetition) and stash them on the project. Not applied: the user
        // accepts/dismisses. Embeddings are best-effort (no index → takes only).
        let embeddings = self
            .inner
            .store
            .load_embeddings(project_id)?
            .map(|(_, v)| v.into_iter().collect::<std::collections::HashMap<_, _>>());
        let suggestions = match self.get_transcript(project_id)? {
            Some(t) => detect::suggest_cuts(&t, embeddings.as_ref()),
            None => vec![],
        };
        let suggestion_count = suggestions.len() as u32;
        {
            let mut state = self.inner.state.lock().unwrap();
            if let Some(p) = state.get_mut(&project_id).and_then(|e| e.project.as_mut()) {
                p.suggestions = suggestions;
                self.inner.store.save_project(p)?;
            }
        }
        Ok((outcome.action, outcome.timeline, cut_count, suggestion_count))
    }

    /// Read the pending rough-cut suggestions (Tier-2 flags).
    pub fn get_suggestions(&self, project_id: Uuid) -> Result<Vec<crate::model::CutSuggestion>> {
        self.with_project(project_id, |p, _| Ok(p.suggestions.clone()))
    }

    /// Accept one suggestion: apply it as a normal undoable cut, then drop it
    /// from the list. Returns the edit action.
    pub fn accept_suggestion(
        &self,
        project_id: Uuid,
        suggestion_id: Uuid,
        source: ActionSource,
    ) -> Result<EditAction> {
        // Only cut segment ids that still exist — a transcript edit since the
        // rough cut can leave a suggestion pointing at gone ids. Stale ones
        // are dropped (the suggestion still drains), never erroring the cut.
        let seg_ids = self.with_project(project_id, |p, t| {
            let s = p
                .suggestions
                .iter()
                .find(|s| s.id == suggestion_id)
                .ok_or_else(|| CoreError::NotFound(format!("suggestion {suggestion_id}")))?;
            Ok(live_segment_ids(&s.segment_ids, t))
        })?;
        let action = if seg_ids.is_empty() {
            None
        } else {
            Some(self.apply_edit(project_id, EditOp::CutSegments { segment_ids: seg_ids }, source)?.action)
        };
        self.remove_suggestions(project_id, &[suggestion_id])?;
        action.ok_or_else(|| {
            CoreError::InvalidArg("that suggestion was stale (transcript changed) — cleared".into())
        })
    }

    /// Accept ALL pending suggestions in one undoable batch. Stale ids are
    /// filtered out so one gone segment can't abort the whole batch; every
    /// listed suggestion is drained regardless.
    pub fn accept_all_suggestions(
        &self,
        project_id: Uuid,
        source: ActionSource,
    ) -> Result<(Option<EditAction>, u32)> {
        let (ids, seg_ids): (Vec<Uuid>, Vec<Uuid>) = self.with_project(project_id, |p, t| {
            let ids: Vec<Uuid> = p.suggestions.iter().map(|s| s.id).collect();
            let raw: Vec<Uuid> = p.suggestions.iter().flat_map(|s| s.segment_ids.clone()).collect();
            Ok((ids, live_segment_ids(&raw, t)))
        })?;
        let count = ids.len() as u32;
        let action = if seg_ids.is_empty() {
            None
        } else {
            Some(self.apply_edit(project_id, EditOp::CutSegments { segment_ids: seg_ids }, source)?.action)
        };
        // Drain ALL listed suggestions even if some were stale.
        self.remove_suggestions(project_id, &ids)?;
        Ok((action, count))
    }

    /// Dismiss a suggestion without cutting.
    pub fn dismiss_suggestion(&self, project_id: Uuid, suggestion_id: Uuid) -> Result<()> {
        self.remove_suggestions(project_id, &[suggestion_id])
    }

    fn remove_suggestions(&self, project_id: Uuid, ids: &[Uuid]) -> Result<()> {
        let mut state = self.inner.state.lock().unwrap();
        if let Some(p) = state.get_mut(&project_id).and_then(|e| e.project.as_mut()) {
            p.suggestions.retain(|s| !ids.contains(&s.id));
            self.inner.store.save_project(p)?;
        }
        send(&self.inner.sink, CoreEvent::TimelineChanged { project_id });
        Ok(())
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

    /// The full edit history, oldest → newest: every applied edit (the undo
    /// stack) followed by every undone edit still available to redo (the
    /// future). `current_index` is the newest APPLIED entry (-1 = at the
    /// original, before any edit). The "go back in time" data source.
    pub fn history(&self, project_id: Uuid) -> Result<crate::model::History> {
        use crate::model::{History, HistoryEntry};
        self.ensure_loaded(project_id)?;
        let state = self.inner.state.lock().unwrap();
        let entry =
            state.get(&project_id).ok_or_else(|| CoreError::NotFound("project".into()))?;
        let to_entry = |j: &JournalEntry, applied: bool| HistoryEntry {
            id: j.action.id,
            kind: j.action.kind.clone(),
            source: j.action.source,
            description: j.action.description.clone(),
            timestamp: j.action.timestamp,
            applied,
        };
        let mut entries: Vec<HistoryEntry> =
            entry.undo.iter().map(|j| to_entry(j, true)).collect();
        // redo.last() is the next to re-apply (chronologically just after the
        // current point), so reversed gives true chronological future order.
        entries.extend(entry.redo.iter().rev().map(|j| to_entry(j, false)));
        Ok(History { current_index: entry.undo.len() as i64 - 1, entries })
    }

    /// Jump to a point in history: undo/redo until `target` is the newest
    /// applied edit. `None` = the original state (undo everything). The
    /// snapshots make every landing exact.
    pub fn jump_to(&self, project_id: Uuid, target: Option<Uuid>) -> Result<Timeline> {
        self.ensure_loaded(project_id)?;
        // TARGET-DRIVEN, not length-driven: re-derive the direction from the
        // LIVE stacks each iteration so a concurrent edit between steps can't
        // strand us (a frozen `desired` length could spin forever after a
        // redo.clear, or land on the wrong entry after an eviction). Each step
        // moves one entry between stacks; the iteration cap is a paranoia
        // backstop (the journal is bounded by UNDO_CAP anyway).
        enum Step {
            Done,
            Undo,
            Redo,
            Vanished,
        }
        let max_steps = 2 * UNDO_CAP + 8;
        let mut stepped = false;
        for _ in 0..max_steps {
            let step = {
                let state = self.inner.state.lock().unwrap();
                let entry = state
                    .get(&project_id)
                    .ok_or_else(|| CoreError::NotFound("project".into()))?;
                match target {
                    None => {
                        if entry.undo.is_empty() {
                            Step::Done
                        } else {
                            Step::Undo
                        }
                    }
                    Some(id) => {
                        if entry.undo.last().map(|j| j.action.id) == Some(id) {
                            Step::Done
                        } else if entry.undo.iter().any(|j| j.action.id == id) {
                            // Target is applied but below the top → undo the
                            // newer entries until it's the newest.
                            Step::Undo
                        } else if entry.redo.iter().any(|j| j.action.id == id) {
                            Step::Redo // target is in the future → redo toward it
                        } else {
                            Step::Vanished // a concurrent edit cleared/evicted it
                        }
                    }
                }
            };
            match step {
                Step::Done => {
                    // Rough-cut suggestions describe a specific timeline state;
                    // after moving through time they no longer match. Drop them
                    // (re-run the rough cut to regenerate for the new state).
                    if stepped {
                        let mut state = self.inner.state.lock().unwrap();
                        if let Some(p) = state.get_mut(&project_id).and_then(|e| e.project.as_mut())
                        {
                            if !p.suggestions.is_empty() {
                                p.suggestions.clear();
                                let _ = self.inner.store.save_project(p);
                            }
                        }
                    }
                    return self.get_timeline(project_id);
                }
                Step::Undo => {
                    self.step_history(project_id, true)?;
                    stepped = true;
                }
                Step::Redo => {
                    self.step_history(project_id, false)?;
                    stepped = true;
                }
                Step::Vanished => {
                    return Err(CoreError::NotFound(format!(
                        "history entry {target:?} is no longer in the timeline (a newer edit \
                         discarded it)"
                    )))
                }
            }
        }
        Err(CoreError::Other("jump_to did not converge".into()))
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

/// Keep only ids the current transcript still has (suggestion segment_ids
/// can go stale after a transcript edit) — order/dedup preserved.
fn live_segment_ids(ids: &[Uuid], transcript: Option<&Transcript>) -> Vec<Uuid> {
    match transcript {
        Some(t) => ids.iter().copied().filter(|id| t.segment(*id).is_some()).collect(),
        None => vec![],
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
