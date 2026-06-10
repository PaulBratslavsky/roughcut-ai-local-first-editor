//! CMX3600 EDL writer. One event per included clip; record times accumulate.

use crate::model::{Media, Project};
use crate::time::{timecode_frames, to_frame};

pub fn write(project: &Project, media: &Media) -> String {
    let fps_i = media.frame_rate.round().max(1.0) as i64;
    let name = std::path::Path::new(&media.file_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| media.file_path.clone());
    let mut out = String::new();
    out.push_str(&format!("TITLE: {}\nFCM: NON-DROP FRAME\n\n", project.name));
    // All math in whole frames: source duration and record duration must
    // match EXACTLY or importers reject the event.
    let mut record_f: i64 = 0;
    for (i, clip) in project.timeline.included_clips().enumerate() {
        let in_f = to_frame(clip.source_in, fps_i as f64);
        let out_f = to_frame(clip.source_out, fps_i as f64);
        let dur_f = out_f - in_f;
        out.push_str(&format!(
            "{:03}  AX       AA/V  C        {} {} {} {}\n* FROM CLIP NAME: {}\n\n",
            i + 1,
            timecode_frames(in_f, fps_i),
            timecode_frames(out_f, fps_i),
            timecode_frames(record_f, fps_i),
            timecode_frames(record_f + dur_f, fps_i),
            name,
        ));
        record_f += dur_f;
    }
    out
}
