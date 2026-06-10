//! FCPXML 1.9 writer for Final Cut Pro. Times are rational seconds with a
//! denominator derived from the frame rate so every value is frame-aligned.

use super::xml_escape;
use crate::model::{Media, Project};
use crate::time::to_frame;

fn rt(frames: i64, fps_num: i64, fps_den: i64) -> String {
    // frames * den / num seconds, expressed as "value/timescale s".
    format!("{}/{}s", frames * fps_den, fps_num)
}

pub fn write(project: &Project, media: &Media) -> String {
    let fps = media.frame_rate;
    // 29.97 -> 30000/1001; integer rates -> n/1.
    let (num, den) = if (fps - fps.round()).abs() > 0.001 {
        ((fps * 1001.0).round() as i64, 1001_i64)
    } else {
        (fps.round() as i64, 1_i64)
    };
    let frame_dur = format!("{den}/{num}s");
    let name = xml_escape(&project.name);
    let src = format!("file://{}", media.file_path.replace(' ', "%20"));
    let media_frames = to_frame(media.duration, fps);

    let mut clips = String::new();
    let mut offset = 0_i64;
    for clip in project.timeline.included_clips() {
        let start_f = to_frame(clip.source_in, fps);
        let dur_f = to_frame(clip.duration(), fps);
        clips.push_str(&format!(
            "<asset-clip ref=\"a1\" name=\"{name}\" offset=\"{}\" start=\"{}\" duration=\"{}\"/>",
            rt(offset, num, den),
            rt(start_f, num, den),
            rt(dur_f, num, den),
        ));
        offset += dur_f;
    }

    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE fcpxml>\n\
         <fcpxml version=\"1.9\"><resources>\
         <format id=\"f1\" name=\"FFVideoFormatRateUndefined\" frameDuration=\"{frame_dur}\" \
         width=\"{w}\" height=\"{h}\"/>\
         <asset id=\"a1\" name=\"{name}\" start=\"0s\" duration=\"{}\" hasVideo=\"1\" hasAudio=\"1\" format=\"f1\">\
         <media-rep kind=\"original-media\" src=\"{src}\"/></asset>\
         </resources>\
         <library><event name=\"{name}\"><project name=\"{name}\">\
         <sequence format=\"f1\"><spine>{clips}</spine></sequence>\
         </project></event></library></fcpxml>\n",
        rt(media_frames, num, den),
        w = media.width,
        h = media.height,
    )
}
