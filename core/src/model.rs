//! Data model (see docs/05-tech-requirements.md). All local, all serde.
//! Times cross the API as seconds; boundaries are frame-snapped before they
//! enter the model (see `crate::time`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const EPS: f64 = 1e-6;

// ---------------------------------------------------------------- media

#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Media {
    pub id: Uuid,
    pub file_path: String,
    pub duration: f64,
    pub frame_rate: f64,
    pub width: u32,
    pub height: u32,
    pub audio_sample_rate: u32,
    pub codec: String,
    pub imported_at: DateTime<Utc>,
}

// ------------------------------------------------------------ transcript

#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
    pub confidence: f64,
}

#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub id: Uuid,
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub words: Vec<Word>,
    pub is_filler: bool,
    pub is_silence: bool,
    pub take_group_id: Option<Uuid>,
    pub is_best_take: bool,
}

#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcript {
    pub id: Uuid,
    pub media_id: Uuid,
    pub language: String,
    pub segments: Vec<TranscriptSegment>,
    pub model_used: String,
}

impl Transcript {
    pub fn segment(&self, id: Uuid) -> Option<&TranscriptSegment> {
        self.segments.iter().find(|s| s.id == id)
    }
}

// -------------------------------------------------------------- timeline

#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Padding {
    pub start_s: f64,
    pub end_s: f64,
    pub linked: bool,
}

impl Default for Padding {
    fn default() -> Self {
        Self { start_s: 0.0, end_s: 0.0, linked: true }
    }
}

#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipOrigin {
    Initial,
    AiCut,
    Manual,
    Split,
}

#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub id: Uuid,
    /// In/out points in the SOURCE media, seconds, frame-snapped. Non-destructive:
    /// a "cut" flips `included`, it never deletes source.
    pub source_in: f64,
    pub source_out: f64,
    pub included: bool,
    pub order: u32,
    pub origin: ClipOrigin,
    pub linked_segment_ids: Vec<Uuid>,
}

impl Clip {
    pub fn duration(&self) -> f64 {
        (self.source_out - self.source_in).max(0.0)
    }
}

#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timeline {
    pub id: Uuid,
    /// Ordered clips forming a contiguous partition of [0, duration] over the source.
    pub clips: Vec<Clip>,
    /// Padding currently APPLIED to the clip boundaries (so re-applying a new
    /// value computes a delta and stays idempotent).
    pub global_padding: Padding,
    /// Source media duration in seconds.
    pub duration: f64,
    /// Derived: number of excluded regions (merged). Kept in sync by `normalize`.
    pub cut_count: u32,
}

impl Timeline {
    pub fn new(duration: f64) -> Self {
        let clip = Clip {
            id: Uuid::new_v4(),
            source_in: 0.0,
            source_out: duration,
            included: true,
            order: 0,
            origin: ClipOrigin::Initial,
            linked_segment_ids: vec![],
        };
        let mut tl = Self {
            id: Uuid::new_v4(),
            clips: if duration > 0.0 { vec![clip] } else { vec![] },
            global_padding: Padding::default(),
            duration,
            cut_count: 0,
        };
        tl.normalize();
        tl
    }

    pub fn clip(&self, id: Uuid) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id == id)
    }

    /// Sort by source_in, drop empty clips, merge adjacent clips with the same
    /// included flag, refresh `order` and `cut_count`.
    pub fn normalize(&mut self) {
        self.clips.retain(|c| c.duration() > EPS);
        self.clips
            .sort_by(|a, b| a.source_in.partial_cmp(&b.source_in).unwrap_or(std::cmp::Ordering::Equal));
        let mut merged: Vec<Clip> = Vec::with_capacity(self.clips.len());
        for clip in self.clips.drain(..) {
            match merged.last_mut() {
                // Merge adjacent same-state clips, except deliberate splits of
                // included clips (the user asked for that boundary to exist).
                Some(last)
                    if last.included == clip.included
                        && (last.source_out - clip.source_in).abs() < EPS
                        && (!last.included
                            || (last.origin != ClipOrigin::Split
                                && clip.origin != ClipOrigin::Split)) =>
                {
                    last.source_out = clip.source_out;
                    for id in clip.linked_segment_ids {
                        if !last.linked_segment_ids.contains(&id) {
                            last.linked_segment_ids.push(id);
                        }
                    }
                }
                _ => merged.push(clip),
            }
        }
        for (i, c) in merged.iter_mut().enumerate() {
            c.order = i as u32;
        }
        self.cut_count = merged.iter().filter(|c| !c.included).count() as u32;
        self.clips = merged;
    }

    /// Ensure clip boundaries exist at `t`; returns nothing, splitting the
    /// covering clip in place when `t` falls strictly inside one.
    pub fn ensure_boundary(&mut self, t: f64, origin: ClipOrigin) {
        if t <= EPS || t >= self.duration - EPS {
            return;
        }
        if let Some(idx) = self
            .clips
            .iter()
            .position(|c| c.source_in + EPS < t && t < c.source_out - EPS)
        {
            let mut right = self.clips[idx].clone();
            right.id = Uuid::new_v4();
            right.source_in = t;
            right.origin = origin;
            self.clips[idx].source_out = t;
            self.clips.insert(idx + 1, right);
        }
    }

    /// Mark every part of [start, end] as included/excluded (the core of
    /// cut_range / restore_range). Splits boundary clips as needed.
    pub fn set_range_included(&mut self, start: f64, end: f64, included: bool, origin: ClipOrigin) {
        let start = start.clamp(0.0, self.duration);
        let end = end.clamp(0.0, self.duration);
        if end - start < EPS {
            return;
        }
        self.ensure_boundary(start, origin);
        self.ensure_boundary(end, origin);
        for clip in &mut self.clips {
            if clip.source_in + EPS >= start && clip.source_out <= end + EPS {
                if clip.included != included {
                    clip.included = included;
                    clip.origin = origin;
                }
            }
        }
        self.normalize();
    }

    /// Like `set_range_included(.., false, ..)` but also records which transcript
    /// segments motivated the cut.
    pub fn cut_linked(&mut self, start: f64, end: f64, segment_ids: &[Uuid], origin: ClipOrigin) {
        self.set_range_included(start, end, false, origin);
        for clip in &mut self.clips {
            if !clip.included && clip.source_in + EPS >= start && clip.source_out <= end + EPS {
                for id in segment_ids {
                    if !clip.linked_segment_ids.contains(id) {
                        clip.linked_segment_ids.push(*id);
                    }
                }
            }
        }
    }

    /// Move a clip's boundaries, keeping the partition contiguous by giving or
    /// taking time from the immediate neighbors. Frame snapping happens
    /// upstream. Dragging an edge all the way across an EXCLUDED neighbor
    /// consumes it exactly — the cut heals and the included clips merge.
    /// Included neighbors keep a minimum width so kept content can't be
    /// silently trimmed to nothing.
    pub fn trim_clip(&mut self, clip_id: Uuid, new_in: f64, new_out: f64) -> Result<(), String> {
        const MIN_KEEP: f64 = 0.05;
        let (new_in_req, new_out_req) = (new_in, new_out);
        let idx = self
            .clips
            .iter()
            .position(|c| c.id == clip_id)
            .ok_or_else(|| format!("clip {clip_id} not found"))?;
        let prev_floor = if idx == 0 {
            0.0
        } else {
            let p = &self.clips[idx - 1];
            if p.included { p.source_in + MIN_KEEP } else { p.source_in }
        };
        let next_ceil = if idx + 1 < self.clips.len() {
            let n = &self.clips[idx + 1];
            if n.included { n.source_out - MIN_KEEP } else { n.source_out }
        } else {
            self.duration
        };
        let mut new_in = new_in.clamp(prev_floor.min(self.clips[idx].source_out), self.clips[idx].source_out - EPS);
        let mut new_out = new_out.clamp(new_in + EPS, next_ceil.max(new_in + EPS));
        // Magnet: when the drag leaves only a sliver (<0.1s) of an excluded
        // neighbor, snap across it so the cut heals instead of leaving crumbs.
        // For an INCLUDED neighbor, dragging to (or past) its far edge is the
        // "dissolve this boundary" gesture: split halves merge back into one
        // clip — a lossless union, since the source in between is identical.
        const SNAP: f64 = 0.1;
        let mut dissolve_next = false;
        let mut dissolve_prev = false;
        if idx + 1 < self.clips.len() {
            let n = &self.clips[idx + 1];
            if !n.included && n.source_out - new_out < SNAP {
                new_out = n.source_out;
            } else if n.included && new_out_req >= n.source_out - SNAP {
                new_out = n.source_out;
                dissolve_next = true;
            }
        }
        if idx > 0 {
            let p = &self.clips[idx - 1];
            if !p.included && new_in - p.source_in < SNAP {
                new_in = p.source_in;
            } else if p.included && new_in_req <= p.source_in + SNAP {
                new_in = p.source_in;
                dissolve_prev = true;
            }
        }
        // The deliberate split boundary is gone; clear Split origin on BOTH
        // sides or normalize() (which spares Split/Split pairs) won't merge —
        // clearing only the dragged clip left heals half-armed and broken.
        if dissolve_next || dissolve_prev {
            self.clips[idx].origin = ClipOrigin::Manual;
        }
        if dissolve_next {
            self.clips[idx + 1].origin = ClipOrigin::Manual;
        }
        if dissolve_prev {
            self.clips[idx - 1].origin = ClipOrigin::Manual;
        }
        if idx > 0 {
            self.clips[idx - 1].source_out = new_in;
        }
        if idx + 1 < self.clips.len() {
            self.clips[idx + 1].source_in = new_out;
        }
        self.clips[idx].source_in = new_in;
        self.clips[idx].source_out = new_out;
        self.normalize();
        Ok(())
    }

    pub fn split_clip(&mut self, clip_id: Uuid, at: f64) -> Result<(Uuid, Uuid), String> {
        let idx = self
            .clips
            .iter()
            .position(|c| c.id == clip_id)
            .ok_or_else(|| format!("clip {clip_id} not found"))?;
        let clip = &self.clips[idx];
        if at <= clip.source_in + EPS || at >= clip.source_out - EPS {
            return Err("split point must fall inside the clip".into());
        }
        let mut right = clip.clone();
        right.id = Uuid::new_v4();
        right.source_in = at;
        right.origin = ClipOrigin::Split;
        let right_id = right.id;
        let left_id = clip.id;
        self.clips[idx].source_out = at;
        // Mark the left side as a split product too so normalize() doesn't
        // immediately merge the two halves back together.
        self.clips[idx].origin = ClipOrigin::Split;
        self.clips.insert(idx + 1, right);
        for (i, c) in self.clips.iter_mut().enumerate() {
            c.order = i as u32;
        }
        Ok((left_id, right_id))
    }

    /// Apply a padding delta: every included clip adjacent to an excluded gap
    /// breathes outward (positive delta) or tightens (negative), without
    /// crossing the middle of the gap.
    pub fn apply_padding(&mut self, start_s: f64, end_s: f64, linked: bool) {
        let d_start = start_s - self.global_padding.start_s;
        let d_end = end_s - self.global_padding.end_s;
        if d_start.abs() > EPS || d_end.abs() > EPS {
            // Walk excluded gaps that sit between two included clips (or edges).
            let n = self.clips.len();
            for i in 0..n {
                if self.clips[i].included {
                    continue;
                }
                let gap_in = self.clips[i].source_in;
                let gap_out = self.clips[i].source_out;
                let mid = (gap_in + gap_out) / 2.0;
                // End padding extends the PREVIOUS included clip into the gap.
                let new_in = if i > 0 { (gap_in + d_end).clamp(self.clips[i - 1].source_in + EPS, mid) } else { gap_in };
                // Start padding extends the NEXT included clip backwards into the gap.
                let new_out = if i + 1 < n {
                    (gap_out - d_start).clamp(mid, self.clips[i + 1].source_out - EPS)
                } else {
                    gap_out
                };
                if i > 0 {
                    self.clips[i - 1].source_out = new_in;
                }
                if i + 1 < n {
                    self.clips[i + 1].source_in = new_out;
                }
                self.clips[i].source_in = new_in;
                self.clips[i].source_out = new_out;
            }
        }
        self.global_padding = Padding { start_s, end_s, linked };
        self.normalize();
    }

    pub fn included_clips(&self) -> impl Iterator<Item = &Clip> {
        self.clips.iter().filter(|c| c.included)
    }

    pub fn included_duration(&self) -> f64 {
        self.included_clips().map(|c| c.duration()).sum()
    }

    /// Map a source time to output (record) time, skipping excluded regions.
    pub fn source_to_output(&self, t: f64) -> f64 {
        let mut out = 0.0;
        for c in self.included_clips() {
            if t >= c.source_out {
                out += c.duration();
            } else if t > c.source_in {
                out += t - c.source_in;
                break;
            } else {
                break;
            }
        }
        out
    }
}

// ------------------------------------------------------------ edit actions

#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionSource {
    Ui,
    LocalAi,
    McpClient,
}

/// An edit, as data. Every mutation of the cut — from the UI, the local
/// agent, or an MCP client — is one of these values applied through a single
/// Editor entry point. Ops are serializable, so the undo journal and the
/// audit trail are replayable.
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EditOp {
    CutRange { start: f64, end: f64 },
    RestoreRange { start: f64, end: f64 },
    CutSegments { segment_ids: Vec<Uuid> },
    RestoreSegments { segment_ids: Vec<Uuid> },
    TrimClip { clip_id: Uuid, new_source_in: f64, new_source_out: f64 },
    SplitClip { clip_id: Uuid, at_time: f64 },
    ReorderClip { clip_id: Uuid, new_order: u32 },
    SetGlobalPadding { start_s: f64, end_s: f64, linked: bool },
    RoughCut { aggressiveness: Aggressiveness },
    /// Restore a prior clip arrangement wholesale — the universal exact
    /// inverse (and exact redo) for every other op.
    SetClips { clips: Vec<Clip>, global_padding: Padding },
}

impl EditOp {
    pub fn kind(&self) -> &'static str {
        match self {
            EditOp::CutRange { .. } | EditOp::CutSegments { .. } => "cut",
            EditOp::RestoreRange { .. } | EditOp::RestoreSegments { .. } => "restore",
            EditOp::TrimClip { .. } => "trim",
            EditOp::SplitClip { .. } => "split",
            EditOp::ReorderClip { .. } => "reorder",
            EditOp::SetGlobalPadding { .. } => "pad",
            EditOp::RoughCut { .. } => "ai_batch",
            EditOp::SetClips { .. } => "set_clips",
        }
    }

    pub fn describe(&self) -> String {
        match self {
            EditOp::CutRange { start, end } => format!("Cut {start:.2}s–{end:.2}s"),
            EditOp::RestoreRange { start, end } => format!("Restore {start:.2}s–{end:.2}s"),
            EditOp::CutSegments { segment_ids } => {
                format!("Cut {} transcript segment(s)", segment_ids.len())
            }
            EditOp::RestoreSegments { segment_ids } => {
                format!("Restore {} transcript segment(s)", segment_ids.len())
            }
            EditOp::TrimClip { new_source_in, new_source_out, .. } => {
                format!("Trim clip to {new_source_in:.2}s–{new_source_out:.2}s")
            }
            EditOp::SplitClip { at_time, .. } => format!("Split clip at {at_time:.2}s"),
            EditOp::ReorderClip { new_order, .. } => {
                format!("Move clip to position {new_order}")
            }
            EditOp::SetGlobalPadding { start_s, end_s, .. } => {
                format!("Set global padding start {start_s:.2}s / end {end_s:.2}s")
            }
            EditOp::RoughCut { .. } => "AI rough cut (silences, fillers, takes)".into(),
            EditOp::SetClips { .. } => "Restore previous cut state".into(),
        }
    }
}

#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditAction {
    pub id: Uuid,
    pub kind: String,
    pub source: ActionSource,
    pub timestamp: DateTime<Utc>,
    pub description: String,
    /// What was requested, normalized (frame-snapped) — the audit record.
    /// (Exact inverse/redo snapshots live in the journal, NOT here — they are
    /// clip-array dumps that would bloat every tool response.)
    pub op: EditOp,
}

impl EditAction {
    pub fn new(source: ActionSource, op: EditOp) -> Self {
        Self {
            id: Uuid::new_v4(),
            kind: op.kind().to_string(),
            source,
            timestamp: Utc::now(),
            description: op.describe(),
            op,
        }
    }
}

// ------------------------------------------------------------- preferences

#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Aggressiveness {
    Natural,
    Aggressive,
}

#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    Auto,
    Small,
    Medium,
    Large,
}

#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preferences {
    pub default_padding: Padding,
    pub cut_aggressiveness: Aggressiveness,
    pub custom_filler_words: Vec<String>,
    pub silence_min_duration_s: f64,
    pub export_target: String,
    pub language: String,
    pub model_tier: ModelTier,
    pub inference_endpoint: String,
    pub inference_model: String,
    /// Local embedding model for semantic transcript search (served by the
    /// same OpenAI-compatible endpoint as the chat model).
    #[serde(default = "default_embedding_model")]
    pub embedding_model: String,
}

fn default_embedding_model() -> String {
    std::env::var("EMBEDDING_MODEL").unwrap_or_else(|_| "nomic-embed-text".into())
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            default_padding: Padding { start_s: 0.15, end_s: 0.15, linked: true },
            cut_aggressiveness: Aggressiveness::Natural,
            custom_filler_words: vec![],
            silence_min_duration_s: 0.8,
            export_target: "premiere_xml".into(),
            language: "auto".into(),
            model_tier: ModelTier::Auto,
            inference_endpoint: std::env::var("INFERENCE_ENDPOINT")
                .unwrap_or_else(|_| "http://localhost:11434/v1".into()),
            inference_model: std::env::var("INFERENCE_MODEL")
                .unwrap_or_else(|_| "gemma4:26b".into()),
            embedding_model: default_embedding_model(),
        }
    }
}

// ---------------------------------------------------------------- project

#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub media: Option<Media>,
    pub timeline: Timeline,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub schema_version: u32,
    /// Trash: set on delete, cleared on restore; purged after 30 days.
    #[serde(default)]
    pub deleted_at: Option<DateTime<Utc>>,
    /// Audio/video sync nudge, seconds. Positive = delay audio (fixes audio
    /// arriving ahead of lips — avfoundation mics start before the camera
    /// warms up). Applied non-destructively at preview and export.
    #[serde(default)]
    pub audio_offset_s: f64,
    /// Dual-capture companion: the screen recording that runs alongside the
    /// primary (camera) media on the same clock. Layout (M4) composites the
    /// two at preview/export; the timeline + transcript stay single-source.
    #[serde(default)]
    pub screen_media: Option<Media>,
}

impl Project {
    pub fn new(name: &str) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            name: name.to_string(),
            media: None,
            timeline: Timeline::new(0.0),
            created_at: now,
            updated_at: now,
            // v2: per-project `settings` removed — Preferences are global and
            // read fresh at use time (one source of truth).
            schema_version: 2,
            deleted_at: None,
            audio_offset_s: 0.0,
            screen_media: None,
        }
    }

    pub fn fps(&self) -> f64 {
        self.media.as_ref().map(|m| m.frame_rate).unwrap_or(30.0)
    }
}

#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub id: Uuid,
    pub name: String,
    pub updated_at: DateTime<Utc>,
    /// Present when the project sits in the trash.
    #[serde(default)]
    pub deleted_at: Option<DateTime<Utc>>,
}

// --------------------------------------------------- external connections

#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-bindings", ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalConnection {
    pub id: Uuid,
    pub provider: String,
    /// Reference into the OS keychain (never the raw key).
    pub api_key_ref: String,
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tl() -> Timeline {
        Timeline::new(100.0)
    }

    #[test]
    fn cut_and_restore_roundtrip() {
        let mut t = tl();
        t.set_range_included(10.0, 20.0, false, ClipOrigin::AiCut);
        assert_eq!(t.cut_count, 1);
        assert!((t.included_duration() - 90.0).abs() < 1e-6);
        t.set_range_included(10.0, 20.0, true, ClipOrigin::Manual);
        assert_eq!(t.cut_count, 0);
        assert_eq!(t.clips.len(), 1);
    }

    #[test]
    fn split_then_trim() {
        let mut t = tl();
        let id = t.clips[0].id;
        let (l, r) = t.split_clip(id, 50.0).unwrap();
        assert_eq!(t.clips.len(), 2);
        assert_ne!(l, r);
        t.set_range_included(40.0, 60.0, false, ClipOrigin::Manual);
        assert_eq!(t.cut_count, 1);
    }

    #[test]
    fn trimming_across_a_cut_heals_it() {
        let mut t = tl();
        t.set_range_included(10.0, 20.0, false, ClipOrigin::AiCut);
        let left = t.clips[0].id;
        t.trim_clip(left, 0.0, 25.0).unwrap(); // overshoots; clamps to gap end
        assert_eq!(t.cut_count, 0, "gap should be fully restored");
        assert_eq!(t.clips.len(), 1, "clips should merge");

        // Stopping a hair short still heals (magnet).
        let mut t = tl();
        t.set_range_included(10.0, 20.0, false, ClipOrigin::AiCut);
        let left = t.clips[0].id;
        t.trim_clip(left, 0.0, 19.95).unwrap();
        assert_eq!(t.cut_count, 0, "sliver should snap shut");
        assert_eq!(t.clips.len(), 1);

        let mut t = tl();
        t.set_range_included(10.0, 20.0, false, ClipOrigin::AiCut);
        let right = t.clips[2].id;
        t.trim_clip(right, 5.0, 100.0).unwrap();
        assert_eq!(t.cut_count, 0);
        assert_eq!(t.clips.len(), 1);

        // A partial roll-drag into an included (split) neighbor keeps both…
        let mut t = tl();
        let first = t.clips[0].id;
        t.split_clip(first, 50.0).unwrap();
        let left = t.clips[0].id;
        t.trim_clip(left, 0.0, 80.0).unwrap();
        assert_eq!(t.clips.len(), 2, "partial drag keeps the split");
        // …but dragging to/past the neighbor's far edge dissolves the split.
        t.trim_clip(left, 0.0, 150.0).unwrap();
        assert_eq!(t.clips.len(), 1, "full drag heals the split boundary");
        assert_eq!(t.cut_count, 0);
    }

    #[test]
    fn padding_breathes() {
        let mut t = tl();
        t.set_range_included(10.0, 20.0, false, ClipOrigin::AiCut);
        t.apply_padding(0.5, 0.5, true);
        let gap = t.clips.iter().find(|c| !c.included).unwrap();
        assert!((gap.source_in - 10.5).abs() < 1e-6);
        assert!((gap.source_out - 19.5).abs() < 1e-6);
        // Idempotent re-apply.
        t.apply_padding(0.5, 0.5, true);
        let gap = t.clips.iter().find(|c| !c.included).unwrap();
        assert!((gap.source_in - 10.5).abs() < 1e-6);
    }

    #[test]
    fn source_to_output_skips_cuts() {
        let mut t = tl();
        t.set_range_included(10.0, 20.0, false, ClipOrigin::AiCut);
        assert!((t.source_to_output(30.0) - 20.0).abs() < 1e-6);
    }
}

#[cfg(test)]
mod heal_tests {
    use super::*;

    #[test]
    fn trim_to_neighbor_edge_heals_a_split() {
        let mut tl = Timeline::new(60.0);
        let clip_id = tl.clips[0].id;
        let (left, _right) = tl.split_clip(clip_id, 30.0).expect("split");
        assert_eq!(tl.clips.len(), 2, "split holds (normalize must not undo it)");
        // Drag the left clip's right edge onto the neighbor's far edge —
        // the dissolve gesture.
        tl.trim_clip(left, 0.0, 60.0).expect("trim");
        assert_eq!(tl.clips.len(), 1, "clips heal back into one");
        assert!((tl.clips[0].source_out - 60.0).abs() < 1e-6);
    }
}
