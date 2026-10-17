//! Shorts on the Editor: candidate storage and short CRUD. Shorts are
//! project metadata — persisted, NOT journaled (like checkpoints and
//! layout): undo/redo moves the main cut, never a short.

use super::Editor;
use crate::error::{CoreError, Result};
use crate::model::{Project, Short, ShortCandidate, ShortSummary};
use crate::time::snap_to_frame;
use chrono::Utc;
use uuid::Uuid;

/// What a short is cut from: a `find_shorts` candidate, or a hand-picked range.
pub enum ShortSource {
    Candidate(Uuid),
    Range { start: f64, end: f64 },
}

impl Editor {
    /// Mutate project metadata in place and persist it.
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
        crate::events::send(&self.sink(), crate::events::CoreEvent::ShortsChanged { project_id });
        Ok(out)
    }

    /// Replace the project's short candidates (one `find_shorts` run).
    pub fn set_short_candidates(&self, project_id: Uuid, candidates: Vec<ShortCandidate>) -> Result<()> {
        self.update_project_meta(project_id, |p| {
            p.short_candidates = candidates;
            Ok(())
        })
    }

    pub fn get_short_candidates(&self, project_id: Uuid) -> Result<Vec<ShortCandidate>> {
        self.with_project(project_id, |p, _| Ok(p.short_candidates.clone()))
    }

    /// Cut a short from a candidate or a source range. Its timeline snapshots
    /// the main cut inside the range (silences/fillers already removed stay
    /// removed). Creating from a candidate takes it off the candidate list.
    pub fn create_short(
        &self,
        project_id: Uuid,
        from: ShortSource,
        title: Option<&str>,
    ) -> Result<ShortSummary> {
        self.update_project_meta(project_id, |p| {
            let (start, end, mut short) = match from {
                ShortSource::Candidate(id) => {
                    let idx = p
                        .short_candidates
                        .iter()
                        .position(|c| c.id == id)
                        .ok_or_else(|| CoreError::NotFound(format!("short candidate {id}")))?;
                    let c = p.short_candidates.remove(idx);
                    let short = Short {
                        id: Uuid::new_v4(),
                        title: c.title,
                        start: 0.0,
                        end: 0.0,
                        timeline: p.timeline.clone(),
                        score: Some(c.score),
                        reason: c.reason,
                        hook: c.hook,
                        created_at: Utc::now(),
                    };
                    (c.start, c.end, short)
                }
                ShortSource::Range { start, end } => {
                    let short = Short {
                        id: Uuid::new_v4(),
                        title: String::new(),
                        start: 0.0,
                        end: 0.0,
                        timeline: p.timeline.clone(),
                        score: None,
                        reason: String::new(),
                        hook: String::new(),
                        created_at: Utc::now(),
                    };
                    (start, end, short)
                }
            };
            set_range(p, &mut short, start, end)?;
            if let Some(t) = title.map(str::trim).filter(|t| !t.is_empty()) {
                short.title = t.to_string();
            }
            if short.title.is_empty() {
                short.title = format!("Short {}", p.shorts.len() + 1);
            }
            let summary = short.summary();
            p.shorts.push(short);
            Ok(summary)
        })
    }

    /// Shorts in creation order.
    pub fn list_shorts(&self, project_id: Uuid) -> Result<Vec<ShortSummary>> {
        self.with_project(project_id, |p, _| Ok(p.shorts.iter().map(Short::summary).collect()))
    }

    /// The full short, timeline included (for export and preview).
    pub fn get_short(&self, project_id: Uuid, short_id: Uuid) -> Result<Short> {
        self.with_project(project_id, |p, _| {
            p.shorts
                .iter()
                .find(|s| s.id == short_id)
                .cloned()
                .ok_or_else(|| CoreError::NotFound(format!("short {short_id}")))
        })
    }

    /// Rename and/or re-range a short. A new range re-snapshots the CURRENT
    /// main cut inside it.
    pub fn update_short(
        &self,
        project_id: Uuid,
        short_id: Uuid,
        title: Option<&str>,
        start: Option<f64>,
        end: Option<f64>,
    ) -> Result<ShortSummary> {
        self.update_project_meta(project_id, |p| {
            let idx = p
                .shorts
                .iter()
                .position(|s| s.id == short_id)
                .ok_or_else(|| CoreError::NotFound(format!("short {short_id}")))?;
            let mut short = p.shorts[idx].clone();
            if start.is_some() || end.is_some() {
                let (s, e) = (start.unwrap_or(short.start), end.unwrap_or(short.end));
                short.timeline = p.timeline.clone();
                set_range(p, &mut short, s, e)?;
            }
            if let Some(t) = title.map(str::trim).filter(|t| !t.is_empty()) {
                short.title = t.to_string();
            }
            let summary = short.summary();
            p.shorts[idx] = short;
            Ok(summary)
        })
    }

    pub fn delete_short(&self, project_id: Uuid, short_id: Uuid) -> Result<()> {
        self.update_project_meta(project_id, |p| {
            let before = p.shorts.len();
            p.shorts.retain(|s| s.id != short_id);
            if p.shorts.len() == before {
                return Err(CoreError::NotFound(format!("short {short_id}")));
            }
            Ok(())
        })
    }
}

/// Validate + frame-snap `[start, end]` and rebuild the short's timeline from
/// `short.timeline` (the main cut it was seeded with) restricted to it.
fn set_range(p: &Project, short: &mut Short, start: f64, end: f64) -> Result<()> {
    let fps = p.fps();
    let dur = p.timeline.duration;
    let (start, end) = (snap_to_frame(start.max(0.0), fps), snap_to_frame(end.min(dur), fps));
    if !(start < end) || start >= dur {
        return Err(CoreError::InvalidArg(format!(
            "short range {start:.2}s–{end:.2}s is invalid: need 0 <= start < end and start < {dur:.2}s (the source duration)"
        )));
    }
    let timeline = short.timeline.restricted_to(start, end);
    if timeline.included_duration() < 1.0 {
        return Err(CoreError::InvalidArg(format!(
            "short range {start:.2}s–{end:.2}s has under a second of kept footage in the current cut"
        )));
    }
    short.start = start;
    short.end = end;
    short.timeline = timeline;
    Ok(())
}
