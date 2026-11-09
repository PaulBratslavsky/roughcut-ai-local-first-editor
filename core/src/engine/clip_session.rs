//! A saved clip open in the Editor (`docs/designs/clip-edit-session.md`).
//! The clip's timeline is swapped into `Project.timeline`, so every edit
//! tool, the chat, proposals and undo work on the clip unchanged; the main
//! edit (its timeline, undo history, checkpoints, proposed cuts and
//! suggestions) waits in the key-value store until Save or Cancel.

use super::{Editor, JournalEntry, ProjectState};
use crate::error::{CoreError, Result};
use crate::events::{send, CoreEvent};
use crate::model::{
    Checkpoint, ClipEditSession, CutProposal, CutSuggestion, SavedClipSummary, SourceRange, Timeline, EPS,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Under this much playing, a saved clip is refused (as `set_ranges` does).
const MIN_CLIP_S: f64 = 1.0;

/// Everything that belongs to the main edit, set aside while a clip is open.
#[derive(Serialize, Deserialize)]
struct MainEdit {
    timeline: Timeline,
    cut_proposal: Option<CutProposal>,
    checkpoints: Vec<Checkpoint>,
    suggestions: Vec<CutSuggestion>,
    undo: Vec<JournalEntry>,
    redo: Vec<JournalEntry>,
}

fn main_key(project_id: Uuid) -> String {
    format!("clip-session-main:{project_id}")
}

impl Editor {
    /// The clip open in the Editor, if any.
    pub fn clip_session(&self, project_id: Uuid) -> Result<Option<ClipEditSession>> {
        self.with_project(project_id, |p, _| Ok(p.clip_session.clone()))
    }

    /// Refuse `what` while a clip is open: it would act on the clip's
    /// timeline as if it were the whole edit.
    pub(crate) fn require_no_clip_session(&self, project_id: Uuid, what: &str) -> Result<()> {
        match self.clip_session(project_id)? {
            Some(s) => Err(CoreError::InvalidArg(format!(
                "you're editing the clip “{}” in the Editor: save or cancel it before {what}",
                s.title
            ))),
            None => Ok(()),
        }
    }

    /// Open a saved clip in the Editor: its timeline becomes the project's,
    /// the main edit is set aside. Opening the clip that is already open is a
    /// no-op; another one is refused.
    pub fn begin_clip_edit(&self, project_id: Uuid, clip_id: Uuid) -> Result<(ClipEditSession, Timeline)> {
        self.ensure_loaded(project_id)?;
        let out = {
            let mut state = self.inner.state.lock().unwrap();
            let entry = state.get_mut(&project_id).ok_or_else(|| CoreError::NotFound("project".into()))?;
            let ProjectState { project, undo, redo, .. } = entry;
            let p = project.as_mut().ok_or_else(|| CoreError::NotFound("project".into()))?;
            if let Some(s) = &p.clip_session {
                if s.clip_id == clip_id {
                    return Ok((s.clone(), p.timeline.clone()));
                }
                return Err(CoreError::InvalidArg(format!(
                    "the clip “{}” is open in the Editor: save or cancel it first",
                    s.title
                )));
            }
            let clip = p
                .saved_clips
                .iter()
                .find(|c| c.id == clip_id)
                .cloned()
                .ok_or_else(|| CoreError::NotFound(format!("clip {clip_id}")))?;
            // Set the main edit aside first: if that write fails, nothing changed.
            let main = MainEdit {
                timeline: p.timeline.clone(),
                cut_proposal: p.cut_proposal.clone(),
                checkpoints: p.checkpoints.clone(),
                suggestions: p.suggestions.clone(),
                undo: undo.clone(),
                redo: redo.clone(),
            };
            self.inner.store.set_kv(&main_key(project_id), &serde_json::to_string(&main)?)?;

            let session = ClipEditSession { clip_id, title: clip.title.clone(), started_at: Utc::now() };
            p.timeline = clip.timeline.clone();
            p.cut_proposal = None;
            p.checkpoints.clear();
            p.suggestions.clear();
            p.clip_session = Some(session.clone());
            p.updated_at = Utc::now();
            undo.clear();
            redo.clear();
            self.inner.store.save_project(p)?;
            self.persist_journal(project_id, undo, redo)?;
            (session, p.timeline.clone())
        };
        self.session_events(project_id);
        Ok(out)
    }

    /// Close the clip open in the Editor. With `save`, its edit (and the
    /// pieces it now covers) becomes the clip's; refused while proposed cuts
    /// are pending or when nothing would play. Either way the main edit comes
    /// back as it was. Returns the saved clip (None on cancel) and the main
    /// timeline.
    pub fn end_clip_edit(&self, project_id: Uuid, save: bool) -> Result<(Option<SavedClipSummary>, Timeline)> {
        self.ensure_loaded(project_id)?;
        let out = {
            let mut state = self.inner.state.lock().unwrap();
            let entry = state.get_mut(&project_id).ok_or_else(|| CoreError::NotFound("project".into()))?;
            let ProjectState { project, undo, redo, .. } = entry;
            let p = project.as_mut().ok_or_else(|| CoreError::NotFound("project".into()))?;
            let session =
                p.clip_session.clone().ok_or_else(|| CoreError::InvalidArg("no clip is open in the Editor".into()))?;
            let main: MainEdit = self
                .inner
                .store
                .get_kv(&main_key(project_id))?
                .filter(|doc| !doc.is_empty())
                .and_then(|doc| serde_json::from_str(&doc).ok())
                .ok_or_else(|| {
                    CoreError::Storage("the main edit set aside for this clip can't be read".into())
                })?;

            let mut saved = None;
            if save {
                if p.cut_proposal.is_some() {
                    return Err(CoreError::InvalidArg(
                        "apply or discard the proposed cuts first, then save the clip".into(),
                    ));
                }
                if let Some(clip) = p.saved_clips.iter_mut().find(|c| c.id == session.clip_id) {
                    let pieces = pieces_after_edit(&clip.ranges, &p.timeline);
                    if pieces.is_empty() || p.timeline.included_duration() < MIN_CLIP_S {
                        return Err(CoreError::InvalidArg(format!(
                            "under {MIN_CLIP_S:.0} s of the clip would play: restore some of it, or cancel"
                        )));
                    }
                    clip.start = pieces[0].start;
                    clip.end = pieces[pieces.len() - 1].end;
                    clip.ranges = pieces;
                    clip.timeline = p.timeline.clone();
                    saved = Some(clip.summary());
                }
            }

            p.timeline = main.timeline;
            p.cut_proposal = main.cut_proposal;
            p.checkpoints = main.checkpoints;
            p.suggestions = main.suggestions;
            p.clip_session = None;
            p.updated_at = Utc::now();
            *undo = main.undo;
            *redo = main.redo;
            self.inner.store.save_project(p)?;
            self.persist_journal(project_id, undo, redo)?;
            self.inner.store.set_kv(&main_key(project_id), "")?;
            (saved, p.timeline.clone())
        };
        self.session_events(project_id);
        Ok(out)
    }

    /// The timeline, clips, proposal and history all changed at once.
    fn session_events(&self, project_id: Uuid) {
        send(&self.inner.sink, CoreEvent::TimelineChanged { project_id });
        send(&self.inner.sink, CoreEvent::ClipsChanged { project_id });
        send(&self.inner.sink, CoreEvent::CutProposalChanged { project_id });
    }
}

/// A clip's pieces after an edit session. Each stretch that plays goes to
/// the old piece it overlaps (most), or else the nearest; each piece becomes
/// the span from its first stretch to its last (cuts inside it stay in the
/// clip's timeline). Pieces with nothing left are dropped; ones that now
/// touch or overlap are merged. Sorted, like every clip's pieces.
pub fn pieces_after_edit(old: &[SourceRange], timeline: &Timeline) -> Vec<SourceRange> {
    let plays: Vec<(f64, f64)> = timeline
        .included_clips()
        .map(|c| (c.source_in, c.source_out))
        .filter(|(a, b)| b - a > EPS)
        .collect();
    if plays.is_empty() {
        return vec![];
    }
    // No pieces to go by: one piece over everything that plays.
    let old: Vec<SourceRange> = if old.is_empty() {
        vec![SourceRange { start: plays[0].0, end: plays[plays.len() - 1].1 }]
    } else {
        old.to_vec()
    };
    let mut spans: Vec<Option<(f64, f64)>> = vec![None; old.len()];
    for &(a, b) in &plays {
        // Overlap counts as negative distance: the piece it overlaps most wins.
        let distance = |r: &SourceRange| {
            let overlap = b.min(r.end) - a.max(r.start);
            if overlap > 0.0 {
                -overlap
            } else {
                (r.start - b).max(a - r.end)
            }
        };
        let k = (0..old.len())
            .min_by(|&i, &j| distance(&old[i]).total_cmp(&distance(&old[j])))
            .unwrap_or(0);
        spans[k] = Some(match spans[k] {
            Some((x, y)) => (x.min(a), y.max(b)),
            None => (a, b),
        });
    }
    let mut out: Vec<SourceRange> =
        spans.into_iter().flatten().map(|(start, end)| SourceRange { start, end }).collect();
    out.sort_by(|x, y| x.start.total_cmp(&y.start));
    let mut merged: Vec<SourceRange> = vec![];
    for r in out {
        match merged.last_mut() {
            Some(last) if r.start <= last.end + EPS => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{ClipChanges, ClipSource};
    use crate::model::{ActionSource, ClipFormat, ClipOrigin, EditOp};

    fn r(start: f64, end: f64) -> SourceRange {
        SourceRange { start, end }
    }

    fn clip_timeline(pieces: &[SourceRange]) -> Timeline {
        Timeline::new(600.0).restricted_to(pieces)
    }

    #[test]
    fn pieces_follow_what_plays_after_an_edit() {
        let pieces = [r(100.0, 130.0)];
        // Unchanged.
        assert_eq!(pieces_after_edit(&pieces, &clip_timeline(&pieces)), vec![r(100.0, 130.0)]);
        // The last sentence cut: the piece ends where playing ends.
        let mut tl = clip_timeline(&pieces);
        tl.set_range_included(124.0, 130.0, false, ClipOrigin::Manual);
        assert_eq!(pieces_after_edit(&pieces, &tl), vec![r(100.0, 124.0)]);
        // A word cut inside stays a cut inside: one piece, same span.
        let mut tl = clip_timeline(&pieces);
        tl.set_range_included(110.0, 111.0, false, ClipOrigin::Manual);
        assert_eq!(pieces_after_edit(&pieces, &tl), vec![r(100.0, 130.0)]);
        // The sentence before restored: the piece starts earlier.
        let mut tl = clip_timeline(&pieces);
        tl.set_range_included(94.0, 100.0, true, ClipOrigin::Manual);
        assert_eq!(pieces_after_edit(&pieces, &tl), vec![r(94.0, 130.0)]);
        // Nothing plays: no pieces.
        let mut tl = clip_timeline(&pieces);
        tl.set_range_included(100.0, 130.0, false, ClipOrigin::Manual);
        assert!(pieces_after_edit(&pieces, &tl).is_empty());
    }

    #[test]
    fn several_pieces_keep_their_shape_and_merge_when_they_meet() {
        let pieces = [r(100.0, 110.0), r(200.0, 210.0)];
        let mut tl = clip_timeline(&pieces);
        // Restored just after the first piece: it grows, the second stays.
        tl.set_range_included(110.0, 113.0, true, ClipOrigin::Manual);
        assert_eq!(pieces_after_edit(&pieces, &tl), vec![r(100.0, 113.0), r(200.0, 210.0)]);
        // The second piece cut entirely: dropped.
        tl.set_range_included(200.0, 210.0, false, ClipOrigin::Manual);
        assert_eq!(pieces_after_edit(&pieces, &tl), vec![r(100.0, 113.0)]);
        // Everything between restored: the two become one.
        let mut tl = clip_timeline(&pieces);
        tl.set_range_included(110.0, 200.0, true, ClipOrigin::Manual);
        assert_eq!(pieces_after_edit(&pieces, &tl), vec![r(100.0, 210.0)]);
    }

    async fn project_with_clip(e: &Editor) -> (Uuid, Uuid) {
        let p = e.create_project("t", Some("/demo/footage.mp4")).await.unwrap();
        let c = e
            .create_clip(p.id, ClipSource::Ranges(vec![r(20.0, 50.0)]), Some("Short"), Some(ClipFormat::Vertical))
            .unwrap();
        (p.id, c.id)
    }

    #[tokio::test]
    async fn a_clip_session_edits_the_clip_and_gives_the_main_edit_back() {
        let e = Editor::test_instance();
        let (pid, cid) = project_with_clip(&e).await;
        // A main edit with history and a checkpoint.
        e.apply_edit(pid, EditOp::CutRange { start: 5.0, end: 8.0 }, ActionSource::Ui).unwrap();
        e.create_checkpoint(pid, "mine", false).unwrap();
        let main = e.get_timeline(pid).unwrap();

        let (session, tl) = e.begin_clip_edit(pid, cid).unwrap();
        assert_eq!((session.clip_id, session.title.as_str()), (cid, "Short"));
        assert!((tl.included_duration() - 30.0).abs() < 0.1, "the clip's timeline is the project's now");
        assert!(e.history(pid).unwrap().entries.is_empty(), "a fresh history");
        assert!(e.get_checkpoints(pid).unwrap().is_empty());
        assert_eq!(e.begin_clip_edit(pid, cid).unwrap().0.started_at, session.started_at, "opening it again is a no-op");

        // Cut the clip's last 5 s and restore the 4 s before it, then save.
        e.apply_edit(pid, EditOp::CutRange { start: 45.0, end: 50.0 }, ActionSource::Ui).unwrap();
        e.apply_edit(pid, EditOp::RestoreRange { start: 16.0, end: 20.0 }, ActionSource::Ui).unwrap();
        let (saved, back) = e.end_clip_edit(pid, true).unwrap();
        let saved = saved.unwrap();
        assert_eq!(saved.ranges, vec![r(16.0, 45.0)]);
        assert!((saved.duration - 29.0).abs() < 0.1, "{}", saved.duration);

        // The main edit is exactly as it was: timeline, history, checkpoint.
        assert_eq!(back.clips.len(), main.clips.len());
        assert!((back.included_duration() - main.included_duration()).abs() < 1e-9);
        assert_eq!(e.history(pid).unwrap().entries.len(), 1);
        assert_eq!(e.get_checkpoints(pid).unwrap().len(), 1);
        assert!(e.clip_session(pid).unwrap().is_none());
        // Undo is the main edit's undo again.
        let (undone, _) = e.undo(pid).unwrap();
        assert!(matches!(undone.unwrap().op, EditOp::CutRange { start, .. } if (start - 5.0).abs() < 1e-6));
    }

    #[tokio::test]
    async fn cancel_drops_the_changes_and_whole_video_tools_wait() {
        let e = Editor::test_instance();
        let (pid, cid) = project_with_clip(&e).await;
        let before = e.get_clip(pid, cid).unwrap();
        e.begin_clip_edit(pid, cid).unwrap();

        // Whole-video work is refused while the clip is open.
        let refused = |r: Result<()>| match r {
            Err(CoreError::InvalidArg(m)) => assert!(m.contains("you're editing the clip “Short”"), "{m}"),
            other => panic!("expected a refusal, got {other:?}"),
        };
        refused(e.create_clip(pid, ClipSource::Ranges(vec![r(60.0, 90.0)]), None, None).map(|_| ()));
        refused(
            e.update_clip(pid, cid, ClipChanges { ranges: Some(vec![r(60.0, 90.0)]), ..Default::default() }).map(|_| ()),
        );
        refused(e.delete_clip(pid, cid));
        refused(e.export(pid, "txt", "/tmp/roughcut-clip-session.txt").await.map(|_| ()));
        // Another clip can't be opened, and its crop can still change.
        let other = e.get_clip(pid, cid).unwrap();
        assert!(e.begin_clip_edit(pid, Uuid::new_v4()).is_err());
        e.update_clip(pid, cid, ClipChanges { crop_x: Some(0.3), ..Default::default() }).unwrap();

        e.apply_edit(pid, EditOp::CutRange { start: 20.0, end: 30.0 }, ActionSource::Ui).unwrap();
        let (saved, _) = e.end_clip_edit(pid, false).unwrap();
        assert!(saved.is_none());
        let after = e.get_clip(pid, cid).unwrap();
        assert_eq!(after.ranges, before.ranges);
        assert!((after.timeline.included_duration() - other.timeline.included_duration()).abs() < 1e-9);
        assert!(e.end_clip_edit(pid, false).is_err(), "nothing open any more");
        e.create_clip(pid, ClipSource::Ranges(vec![r(60.0, 90.0)]), None, None).unwrap();
    }

    #[tokio::test]
    async fn an_open_clip_survives_a_restart() {
        use crate::adapters::{MockTranscriber, MockVideoEngine};
        use crate::store::SqliteStore;
        let db = std::env::temp_dir().join(format!("roughcut-clip-session-{}.db", Uuid::new_v4()));
        let open = || {
            Editor::new(
                Box::new(SqliteStore::open(&db).unwrap()),
                Box::new(MockVideoEngine),
                Box::new(MockTranscriber),
                std::sync::Arc::new(crate::events::NullSink),
                true,
            )
        };
        let (pid, cid) = {
            let e = open();
            let (pid, cid) = project_with_clip(&e).await;
            e.apply_edit(pid, EditOp::CutRange { start: 5.0, end: 8.0 }, ActionSource::Ui).unwrap();
            e.begin_clip_edit(pid, cid).unwrap();
            e.apply_edit(pid, EditOp::CutRange { start: 45.0, end: 50.0 }, ActionSource::Ui).unwrap();
            (pid, cid)
        };
        // The app quit with the clip open; it opens again.
        let e = open();
        assert_eq!(e.clip_session(pid).unwrap().map(|s| s.clip_id), Some(cid));
        assert_eq!(e.history(pid).unwrap().entries.len(), 1, "the session's own history");
        assert!((e.get_timeline(pid).unwrap().included_duration() - 25.0).abs() < 0.1);
        let (saved, main) = e.end_clip_edit(pid, true).unwrap();
        assert_eq!(saved.unwrap().ranges, vec![r(20.0, 45.0)]);
        assert_eq!(main.cut_count, 1, "the main edit's one cut");
        assert_eq!(e.history(pid).unwrap().entries.len(), 1, "the main edit's history");
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
    }

    #[tokio::test]
    async fn save_waits_for_proposed_cuts_and_refuses_an_empty_clip() {
        let e = Editor::test_instance();
        let (pid, cid) = project_with_clip(&e).await;
        e.begin_clip_edit(pid, cid).unwrap();
        e.propose_ops(pid, vec![EditOp::CutRange { start: 25.0, end: 27.0 }], "Chat", ActionSource::LocalAi, None, true)
            .unwrap();
        assert!(matches!(e.end_clip_edit(pid, true), Err(CoreError::InvalidArg(m)) if m.contains("proposed cuts")));
        e.discard_cut_proposal(pid).unwrap();
        e.apply_edit(pid, EditOp::CutRange { start: 20.0, end: 50.0 }, ActionSource::Ui).unwrap();
        assert!(matches!(e.end_clip_edit(pid, true), Err(CoreError::InvalidArg(m)) if m.contains("would play")));
        // Cancel still works, and the session's proposal never reaches the main edit.
        e.end_clip_edit(pid, false).unwrap();
        assert!(e.get_cut_proposal(pid).unwrap().is_none());
    }
}
