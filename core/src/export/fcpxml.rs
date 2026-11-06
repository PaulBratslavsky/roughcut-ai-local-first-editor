//! FCPXML 1.9 writer for Final Cut Pro. Times are rational seconds with a
//! denominator derived from the frame rate so every value is frame-aligned.

use super::{file_url, xml_escape};
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
    // Percent-encoded, so nothing in it needs escaping for XML.
    let src = file_url(&media.file_path);
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

#[cfg(test)]
mod tests {
    use crate::model::{Media, Project, Timeline};

    #[test]
    fn a_media_path_with_xml_characters_stays_well_formed() {
        let media = Media {
            id: uuid::Uuid::new_v4(),
            file_path: "/Users/me/Tom & Jerry \"take\" <2>.mov".into(),
            duration: 10.0,
            frame_rate: 30.0,
            width: 1920,
            height: 1080,
            audio_sample_rate: 48_000,
            codec: "h264".into(),
            imported_at: chrono::Utc::now(),
            source_url: None,
        };
        let mut project = Project::new("Q&A");
        project.timeline = Timeline::new(media.duration);
        let xml = super::write(&project, &media);
        assert!(xml.contains("src=\"file:///Users/me/Tom%20%26%20Jerry%20%22take%22%20%3C2%3E.mov\""), "{xml}");
        assert!(!xml.contains("Tom &"), "no raw ampersand from the path: {xml}");
        assert!(xml.contains("name=\"Q&amp;A\""));
    }
}
