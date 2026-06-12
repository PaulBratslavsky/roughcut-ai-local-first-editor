//! FCP7 "xmeml" XML — the interchange dialect Premiere Pro and DaVinci
//! Resolve import ("Final Cut Pro XML").

use super::xml_escape;
use crate::model::{Media, Project};
use crate::time::to_frame;

pub fn write(project: &Project, media: &Media) -> String {
    let fps = media.frame_rate;
    let timebase = fps.round() as i64;
    let ntsc = if (fps - fps.round()).abs() > 0.001 { "TRUE" } else { "FALSE" };
    let name = xml_escape(&project.name);
    let file_name = std::path::Path::new(&media.file_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| media.file_path.clone());
    let file_url = format!("file://{}", media.file_path.replace(' ', "%20"));
    let total_frames: i64 = project
        .timeline
        .included_clips()
        .map(|c| to_frame(c.duration(), fps))
        .sum();

    let mut video_items = String::new();
    let mut screen_items = String::new();
    let mut audio_items = String::new();
    let mut record = 0_i64;
    let screen = project.screen_media.as_ref();
    for (i, clip) in project.timeline.included_clips().enumerate() {
        let in_f = to_frame(clip.source_in, fps);
        let out_f = to_frame(clip.source_out, fps);
        let dur = out_f - in_f;
        let file_def = if i == 0 {
            format!(
                "<file id=\"file-1\"><name>{fname}</name><pathurl>{url}</pathurl>\
                 <rate><timebase>{timebase}</timebase><ntsc>{ntsc}</ntsc></rate>\
                 <duration>{media_dur}</duration>\
                 <media><video><samplecharacteristics>\
                 <width>{w}</width><height>{h}</height>\
                 </samplecharacteristics></video><audio><channelcount>2</channelcount></audio></media></file>",
                fname = xml_escape(&file_name),
                url = xml_escape(&file_url),
                media_dur = to_frame(media.duration, fps),
                w = media.width,
                h = media.height,
            )
        } else {
            "<file id=\"file-1\"/>".to_string()
        };
        video_items.push_str(&format!(
            "<clipitem id=\"clipitem-v{n}\"><name>{fname}</name>\
             <rate><timebase>{timebase}</timebase><ntsc>{ntsc}</ntsc></rate>\
             <start>{start}</start><end>{end}</end><in>{in_f}</in><out>{out_f}</out>\
             {file_def}</clipitem>",
            n = i + 1,
            fname = xml_escape(&file_name),
            start = record,
            end = record + dur,
        ));
        audio_items.push_str(&format!(
            "<clipitem id=\"clipitem-a{n}\"><name>{fname}</name>\
             <rate><timebase>{timebase}</timebase><ntsc>{ntsc}</ntsc></rate>\
             <start>{start}</start><end>{end}</end><in>{in_f}</in><out>{out_f}</out>\
             <file id=\"file-1\"/>\
             <sourcetrack><mediatype>audio</mediatype><trackindex>1</trackindex></sourcetrack>\
             </clipitem>",
            n = i + 1,
            fname = xml_escape(&file_name),
            start = record,
            end = record + dur,
        ));
        // Dual-capture: the screen rides as a second video track with the
        // SAME record/source timings (shared clock — cuts apply to both).
        // No baked transforms: the finishing tool positions the face.
        if let Some(scr) = screen {
            let s_name = std::path::Path::new(&scr.file_path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| scr.file_path.clone());
            let s_url = format!("file://{}", scr.file_path.replace(' ', "%20"));
            let s_file = if i == 0 {
                format!(
                    "<file id=\"file-2\"><name>{fname}</name><pathurl>{url}</pathurl>\
                     <rate><timebase>{timebase}</timebase><ntsc>{ntsc}</ntsc></rate>\
                     <duration>{sdur}</duration>\
                     <media><video><samplecharacteristics>\
                     <width>{w}</width><height>{h}</height>\
                     </samplecharacteristics></video></media></file>",
                    fname = xml_escape(&s_name),
                    url = xml_escape(&s_url),
                    sdur = to_frame(scr.duration, fps),
                    w = scr.width,
                    h = scr.height,
                )
            } else {
                "<file id=\"file-2\"/>".to_string()
            };
            // Clamp to the screen file's own length: importers reject
            // source ranges past a clip's media EOF.
            let s_total = to_frame(scr.duration, fps);
            let s_out = out_f.min(s_total);
            if in_f < s_total {
                screen_items.push_str(&format!(
                    "<clipitem id=\"clipitem-s{n}\"><name>{fname}</name>\
                     <rate><timebase>{timebase}</timebase><ntsc>{ntsc}</ntsc></rate>\
                     <start>{start}</start><end>{end}</end><in>{in_f}</in><out>{s_out}</out>\
                     {s_file}</clipitem>",
                    n = i + 1,
                    fname = xml_escape(&s_name),
                    start = record,
                    end = record + (s_out - in_f),
                ));
            }
        }
        record += dur;
    }

    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE xmeml>\n\
         <xmeml version=\"4\"><sequence id=\"sequence-1\"><name>{name}</name>\
         <duration>{total_frames}</duration>\
         <rate><timebase>{timebase}</timebase><ntsc>{ntsc}</ntsc></rate>\
         <media><video><format><samplecharacteristics>\
         <width>{w}</width><height>{h}</height>\
         <rate><timebase>{timebase}</timebase><ntsc>{ntsc}</ntsc></rate>\
         </samplecharacteristics></format>\
         {screen_track}<track>{video_items}</track></video>\
         <audio><track>{audio_items}</track></audio></media>\
         </sequence></xmeml>\n",
        w = screen.map(|s| s.width).unwrap_or(media.width),
        h = screen.map(|s| s.height).unwrap_or(media.height),
        screen_track = if screen_items.is_empty() {
            String::new()
        } else {
            // V1 (under) = screen; the camera track stacks above it.
            format!("<track>{screen_items}</track>")
        },
    )
}
