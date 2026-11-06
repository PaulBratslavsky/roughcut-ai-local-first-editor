//! Cut proposals: cuts worked out but not made (see [`CutProposal`]). The
//! Rough cut button and the in-app chat's cut tools propose instead of
//! cutting; the user reviews, plays the result, and applies. Proposing runs
//! the very same `apply_op` an edit would, on a copy of the project, and keeps
//! the ranges it took out — so what is applied is exactly what was shown.

use super::edits::{apply_op, normalize_op, validate_range};
use super::Editor;
use crate::detect;
use crate::error::{CoreError, Result};
use crate::events::{send, CoreEvent};
use crate::model::*;
use chrono::Utc;
use uuid::Uuid;

/// Differences shorter than this are frame rounding, not cuts.
const MIN_PROPOSED_S: f64 = 0.04;
/// How much of a cut's words its `text` keeps.
const TEXT_CHARS: usize = 120;

/// What proposing added.
pub struct ProposeOutcome {
    /// The pending proposal afterwards (None: nothing to cut and none pending).
    pub proposal: Option<CutProposal>,
    /// The cuts this call added.
    pub added: Vec<ProposedCut>,
}

/// What the rough cut found (and, with `apply`, did).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RoughCutReport {
    /// Cuts proposed (or made, when `applied`).
    pub cut_count: u32,
    /// Seconds they remove.
    pub seconds: f64,
    pub pauses: u32,
    pub pause_seconds: f64,
    pub fillers: u32,
    pub filler_seconds: f64,
    /// Cuts that take out other words too (a pause or filler glued to them).
    pub words: u32,
    pub word_seconds: f64,
    /// Repeated takes and repetition flagged for review (not in the cuts).
    pub suggestions: u32,
    /// The pauses were checked against the audio (false: transcript only).
    pub audio_checked: bool,
    pub applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<EditAction>,
}

impl RoughCutReport {
    pub fn from_cuts(cuts: &[ProposedCut]) -> Self {
        let mut r = Self::default();
        for c in cuts {
            let d = c.end - c.start;
            r.cut_count += 1;
            r.seconds += d;
            match c.kind.as_str() {
                "pause" => (r.pauses += 1, r.pause_seconds += d),
                "filler" => (r.fillers += 1, r.filler_seconds += d),
                _ => (r.words += 1, r.word_seconds += d),
            };
        }
        let round = |x: f64| (x * 10.0).round() / 10.0;
        r.seconds = round(r.seconds);
        r.pause_seconds = round(r.pause_seconds);
        r.filler_seconds = round(r.filler_seconds);
        r.word_seconds = round(r.word_seconds);
        r
    }
}

/// What applying a proposal did.
pub struct AppliedProposal {
    /// None when every cut was skipped (nothing was cut).
    pub action: Option<EditAction>,
    pub applied: usize,
    pub skipped: usize,
    pub seconds: f64,
    pub timeline: Timeline,
}

impl Editor {
    /// The pending cut proposal, if any.
    pub fn get_cut_proposal(&self, project_id: Uuid) -> Result<Option<CutProposal>> {
        self.with_project(project_id, |p, _| Ok(p.cut_proposal.clone()))
    }

    /// Work out what `ops` would cut without cutting: run them on a copy of
    /// the project and propose every range they take out. `replace` starts a
    /// fresh proposal (Rough cut); otherwise the new cuts join the pending
    /// one (the chat proposing several cuts in a turn), and only what isn't
    /// already proposed is added. Restores, trims and padding in `ops` have
    /// no effect here: a proposal only ever adds cuts. `peaks` feeds the
    /// rough cut's silence check.
    pub fn propose_ops(
        &self,
        project_id: Uuid,
        ops: Vec<EditOp>,
        title: &str,
        source: ActionSource,
        peaks: Option<&[u8]>,
        replace: bool,
    ) -> Result<ProposeOutcome> {
        self.ensure_loaded(project_id)?;
        let prefs = self.inner.store.load_preferences()?;
        let outcome = {
            let mut state = self.inner.state.lock().unwrap();
            let entry =
                state.get_mut(&project_id).ok_or_else(|| CoreError::NotFound("project".into()))?;
            let project =
                entry.project.as_mut().ok_or_else(|| CoreError::NotFound("project".into()))?;
            let pending = if replace { None } else { project.cut_proposal.clone() };

            // The base: the timeline as it is, plus what's already proposed.
            let mut base = project.clone();
            base.cut_proposal = None;
            for c in pending.iter().flat_map(|p| &p.cuts) {
                base.timeline.set_range_included(c.start, c.end, false, ClipOrigin::AiCut);
            }
            base.timeline.normalize();

            let mut scratch = base.clone();
            let mut transcript = entry.transcript.clone();
            let mut split_ids = vec![];
            for op in ops {
                let op = normalize_op(op, scratch.fps());
                validate_range(&op, scratch.timeline.duration)?;
                apply_op(&mut scratch, transcript.as_mut(), &op, &prefs, &mut split_ids, peaks)?;
            }
            scratch.timeline.normalize();

            let mut cuts = pending.as_ref().map(|p| p.cuts.clone()).unwrap_or_default();
            let mut next_id = cuts.iter().map(|c| c.id).max().unwrap_or(0) + 1;
            let mut added = vec![];
            for (start, end) in newly_cut(&base.timeline, &scratch.timeline) {
                if end - start < MIN_PROPOSED_S {
                    continue;
                }
                let (kind, text) =
                    describe_range(entry.transcript.as_ref(), start, end, &prefs.custom_filler_words);
                let cut = ProposedCut { id: next_id, start, end, kind, text, skipped: false };
                next_id += 1;
                added.push(cut.clone());
                cuts.push(cut);
            }
            cuts.sort_by(|a, b| a.start.total_cmp(&b.start));

            let proposal = if cuts.is_empty() {
                None
            } else {
                let title = match &pending {
                    Some(p) if !added.is_empty() && !p.title.split(" + ").any(|t| t == title) => {
                        format!("{} + {title}", p.title)
                    }
                    Some(p) => p.title.clone(),
                    None => title.to_string(),
                };
                Some(CutProposal {
                    id: pending.as_ref().map(|p| p.id).unwrap_or_else(Uuid::new_v4),
                    title,
                    source,
                    created_at: pending.as_ref().map(|p| p.created_at).unwrap_or_else(Utc::now),
                    cuts,
                })
            };
            project.cut_proposal = proposal.clone();
            self.inner.store.save_project(project)?;
            ProposeOutcome { proposal, added }
        };
        send(&self.inner.sink, CoreEvent::CutProposalChanged { project_id });
        Ok(outcome)
    }

    /// Make the proposal's cuts that aren't skipped, as ONE undoable edit
    /// (after an automatic checkpoint), and clear the proposal.
    pub fn apply_cut_proposal(&self, project_id: Uuid, source: ActionSource) -> Result<AppliedProposal> {
        let proposal = self
            .get_cut_proposal(project_id)?
            .ok_or_else(|| CoreError::InvalidArg("no proposed cuts are waiting to be applied".into()))?;
        let ranges: Vec<SourceRange> = proposal
            .cuts
            .iter()
            .filter(|c| !c.skipped)
            .map(|c| SourceRange { start: c.start, end: c.end })
            .collect();
        let applied = ranges.len();
        let skipped = proposal.cuts.len() - applied;
        let seconds = proposal.seconds();
        let action = if ranges.is_empty() {
            None
        } else {
            let _ = self.create_checkpoint(project_id, &format!("Before {}", proposal.title), true);
            let label = format!(
                "{}: {applied} cut{} ({})",
                proposal.title,
                if applied == 1 { "" } else { "s" },
                fmt_seconds(seconds)
            );
            Some(self.apply_edit_labeled(project_id, EditOp::CutRanges { ranges }, source, Some(label))?.action)
        };
        self.set_cut_proposal(project_id, None)?;
        let timeline = self.get_timeline(project_id)?;
        Ok(AppliedProposal { action, applied, skipped, seconds, timeline })
    }

    /// Drop the pending proposal without cutting. False when none was pending.
    pub fn discard_cut_proposal(&self, project_id: Uuid) -> Result<bool> {
        let had = self.get_cut_proposal(project_id)?.is_some();
        if had {
            self.set_cut_proposal(project_id, None)?;
        }
        Ok(had)
    }

    /// Leave cuts out of the proposal (`skip`), or put them back. Picks cuts
    /// by `ids`, by `kind` ("pause" | "filler" | "words"), both, or — with
    /// neither — every cut.
    pub fn skip_proposed_cuts(
        &self,
        project_id: Uuid,
        ids: Option<&[u32]>,
        kind: Option<&str>,
        skip: bool,
    ) -> Result<(CutProposal, usize)> {
        let mut proposal = self
            .get_cut_proposal(project_id)?
            .ok_or_else(|| CoreError::InvalidArg("no proposed cuts are waiting".into()))?;
        if let Some(k) = kind {
            if !["pause", "filler", "words"].contains(&k) {
                return Err(CoreError::InvalidArg("kind: pause | filler | words".into()));
            }
        }
        if let Some(ids) = ids {
            if let Some(bad) = ids.iter().find(|id| !proposal.cuts.iter().any(|c| c.id == **id)) {
                return Err(CoreError::InvalidArg(format!(
                    "no proposed cut {bad}; the proposal's cuts are numbered 1–{}",
                    proposal.cuts.iter().map(|c| c.id).max().unwrap_or(0)
                )));
            }
        }
        let mut changed = 0;
        for c in &mut proposal.cuts {
            let by_id = ids.is_none_or(|ids| ids.contains(&c.id));
            let by_kind = kind.is_none_or(|k| c.kind == k);
            if by_id && by_kind && c.skipped != skip {
                c.skipped = skip;
                changed += 1;
            }
        }
        self.set_cut_proposal(project_id, Some(proposal.clone()))?;
        Ok((proposal, changed))
    }

    /// Take proposed cuts that overlap `start..end` out of the proposal: a
    /// restore from the chat means "keep this part", whether its cut was made
    /// or only proposed. Returns how many were dropped.
    pub fn drop_proposed_cuts_in(&self, project_id: Uuid, start: f64, end: f64) -> Result<usize> {
        let Some(mut proposal) = self.get_cut_proposal(project_id)? else { return Ok(0) };
        let before = proposal.cuts.len();
        proposal.cuts.retain(|c| c.end <= start || c.start >= end);
        let dropped = before - proposal.cuts.len();
        if dropped > 0 {
            self.set_cut_proposal(project_id, (!proposal.cuts.is_empty()).then_some(proposal))?;
        }
        Ok(dropped)
    }

    fn set_cut_proposal(&self, project_id: Uuid, proposal: Option<CutProposal>) -> Result<()> {
        self.ensure_loaded(project_id)?;
        {
            let mut state = self.inner.state.lock().unwrap();
            let project = state
                .get_mut(&project_id)
                .and_then(|e| e.project.as_mut())
                .ok_or_else(|| CoreError::NotFound("project".into()))?;
            project.cut_proposal = proposal;
            self.inner.store.save_project(project)?;
        }
        send(&self.inner.sink, CoreEvent::CutProposalChanged { project_id });
        Ok(())
    }
}

/// "3:12" / "41.5s" — how much a set of cuts removes.
pub fn fmt_seconds(s: f64) -> String {
    if s >= 60.0 {
        format!("{}:{:02}", (s / 60.0) as u64, (s % 60.0) as u64)
    } else {
        format!("{s:.1}s")
    }
}

/// Sorted, merged source ranges of a timeline that are in (`included`) or
/// out of the cut.
fn ranges_where(timeline: &Timeline, included: bool) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = vec![];
    let mut clips: Vec<&Clip> = timeline.clips.iter().filter(|c| c.included == included).collect();
    clips.sort_by(|a, b| a.source_in.total_cmp(&b.source_in));
    for c in clips {
        match out.last_mut() {
            Some(last) if c.source_in <= last.1 + 1e-6 => last.1 = last.1.max(c.source_out),
            _ => out.push((c.source_in, c.source_out)),
        }
    }
    out
}

/// Ranges `after` cuts that `before` kept.
fn newly_cut(before: &Timeline, after: &Timeline) -> Vec<(f64, f64)> {
    let kept = ranges_where(before, true);
    let cut = ranges_where(after, false);
    let (mut i, mut j, mut out) = (0, 0, vec![]);
    while i < kept.len() && j < cut.len() {
        let s = kept[i].0.max(cut[j].0);
        let e = kept[i].1.min(cut[j].1);
        if e > s {
            out.push((s, e));
        }
        if kept[i].1 < cut[j].1 {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// What a cut removes: "pause" (no words), "filler" (only filler words) or
/// "words", with the words.
fn describe_range(
    transcript: Option<&Transcript>,
    start: f64,
    end: f64,
    custom_fillers: &[String],
) -> (String, String) {
    let words: Vec<&str> = transcript
        .map(|t| {
            t.segments
                .iter()
                .filter(|s| !s.is_silence && s.end > start && s.start < end)
                .flat_map(|s| s.words.iter())
                .filter(|w| {
                    let mid = (w.start + w.end) / 2.0;
                    mid >= start && mid < end
                })
                .map(|w| w.text.trim())
                .filter(|w| !w.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if words.is_empty() {
        return ("pause".into(), String::new());
    }
    let kind = if words.iter().all(|w| detect::is_filler_word(w, custom_fillers)) {
        "filler"
    } else {
        "words"
    };
    let text = words.join(" ");
    let text = if text.chars().count() > TEXT_CHARS {
        format!("{}…", text.chars().take(TEXT_CHARS).collect::<String>().trim_end())
    } else {
        text
    };
    (kind.into(), text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tl(duration: f64, cuts: &[(f64, f64)]) -> Timeline {
        let mut t = Timeline::new(duration);
        for (s, e) in cuts {
            t.set_range_included(*s, *e, false, ClipOrigin::Manual);
        }
        t.normalize();
        t
    }

    #[test]
    fn newly_cut_is_only_what_was_kept_before() {
        let before = tl(100.0, &[(10.0, 20.0)]);
        let after = tl(100.0, &[(5.0, 25.0), (50.0, 60.0)]);
        assert_eq!(newly_cut(&before, &after), vec![(5.0, 10.0), (20.0, 25.0), (50.0, 60.0)]);
        assert!(newly_cut(&after, &after).is_empty());
    }

    #[test]
    fn fmt_seconds_reads_as_time() {
        assert_eq!(fmt_seconds(41.54), "41.5s");
        assert_eq!(fmt_seconds(317.0), "5:17");
    }
}
