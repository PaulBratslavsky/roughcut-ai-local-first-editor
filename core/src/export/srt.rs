//! SRT captions for the CURRENT CUT: excluded segments are dropped and times
//! are remapped to output time.

use crate::model::{Project, Transcript};

fn srt_time(seconds: f64) -> String {
    let ms = (seconds * 1000.0).round() as i64;
    let (h, m, s, milli) = (ms / 3_600_000, (ms / 60_000) % 60, (ms / 1000) % 60, ms % 1000);
    format!("{h:02}:{m:02}:{s:02},{milli:03}")
}

pub fn write(project: &Project, transcript: &Transcript) -> String {
    let tl = &project.timeline;
    let mut out = String::new();
    let mut n = 1;
    for seg in &transcript.segments {
        if seg.is_silence || seg.text.is_empty() {
            continue;
        }
        let mid = (seg.start + seg.end) / 2.0;
        let included = tl.clips.iter().any(|c| c.included && c.source_in <= mid && mid < c.source_out);
        if !included {
            continue;
        }
        let start = tl.source_to_output(seg.start.max(0.0));
        let end = tl.source_to_output(seg.end);
        if end <= start {
            continue;
        }
        out.push_str(&format!("{n}\n{} --> {}\n{}\n\n", srt_time(start), srt_time(end), seg.text));
        n += 1;
    }
    out
}
