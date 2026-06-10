//! Frame-exact rational time (OTIO-style `value/rate`).
//!
//! The model persists times as `f64` seconds for a clean JSON edge, but every
//! boundary that lands in the timeline is snapped to a frame of the source
//! media first, via [`snap_to_frame`]. All arithmetic that decides where a
//! frame boundary lies goes through [`RationalTime`] so float drift can't
//! accumulate.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RationalTime {
    /// Count of units.
    pub value: i64,
    /// Units per second (e.g. the frame rate numerator over 1s).
    pub rate: i64,
}

impl RationalTime {
    pub fn new(value: i64, rate: i64) -> Self {
        assert!(rate > 0, "rate must be positive");
        Self { value, rate }
    }

    pub fn from_seconds(seconds: f64, rate: i64) -> Self {
        Self { value: (seconds * rate as f64).round() as i64, rate }
    }

    pub fn to_seconds(self) -> f64 {
        self.value as f64 / self.rate as f64
    }

    pub fn rescaled_to(self, rate: i64) -> Self {
        if self.rate == rate {
            return self;
        }
        Self { value: (self.value as i128 * rate as i128 / self.rate as i128) as i64, rate }
    }
}

/// Express an fps that may be fractional (29.97 = 30000/1001) as a tick rate
/// usable for exact frame indexing: ticks = round(fps * 1001).
pub fn fps_tick_rate(fps: f64) -> i64 {
    ((fps * 1001.0).round() as i64).max(1)
}

/// Snap a time in seconds to the nearest frame boundary of media at `fps`.
pub fn snap_to_frame(seconds: f64, fps: f64) -> f64 {
    if fps <= 0.0 {
        return seconds;
    }
    let frame = (seconds * fps).round();
    frame / fps
}

/// Convert seconds to a whole frame index at `fps` (nearest).
pub fn to_frame(seconds: f64, fps: f64) -> i64 {
    (seconds * fps).round() as i64
}

/// SMPTE-ish timecode (non-drop) from a whole frame count. EDL math must be
/// done in frames end-to-end — rounding endpoints independently can make
/// source and record durations differ by a frame, which NLE importers reject.
pub fn timecode_frames(total_frames: i64, fps_i: i64) -> String {
    let fps_i = fps_i.max(1);
    let f = total_frames % fps_i;
    let total_secs = total_frames / fps_i;
    let s = total_secs % 60;
    let m = (total_secs / 60) % 60;
    let h = total_secs / 3600;
    format!("{h:02}:{m:02}:{s:02}:{f:02}")
}

/// SMPTE-ish timecode (non-drop) from seconds.
pub fn timecode(seconds: f64, fps: f64) -> String {
    let fps_i = fps.round().max(1.0) as i64;
    timecode_frames(to_frame(seconds, fps_i as f64), fps_i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_is_frame_exact() {
        let s = snap_to_frame(1.004, 30.0);
        assert!((s - 1.0).abs() < 1e-9);
        assert_eq!(to_frame(s, 30.0), 30);
    }

    #[test]
    fn timecode_formats() {
        assert_eq!(timecode(3661.5, 30.0), "01:01:01:15");
    }
}
