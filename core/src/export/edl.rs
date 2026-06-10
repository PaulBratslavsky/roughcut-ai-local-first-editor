//! CMX3600 EDL writer. One event per included clip; record times accumulate.

use crate::model::{Media, Project};
use crate::time::timecode;

pub fn write(project: &Project, media: &Media) -> String {
    let fps = media.frame_rate;
    let name = std::path::Path::new(&media.file_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| media.file_path.clone());
    let mut out = String::new();
    out.push_str(&format!("TITLE: {}\nFCM: NON-DROP FRAME\n\n", project.name));
    let mut record = 0.0_f64;
    for (i, clip) in project.timeline.included_clips().enumerate() {
        let dur = clip.duration();
        out.push_str(&format!(
            "{:03}  AX       AA/V  C        {} {} {} {}\n* FROM CLIP NAME: {}\n\n",
            i + 1,
            timecode(clip.source_in, fps),
            timecode(clip.source_out, fps),
            timecode(record, fps),
            timecode(record + dur, fps),
            name,
        ));
        record += dur;
    }
    out
}
