//! OpenTimelineIO JSON writer (native .otio schema, hand-emitted — there is
//! no mature Rust OTIO crate yet; this stays import-compatible with otio
//! tooling for single-track cut lists).

use crate::error::Result;
use crate::model::{Media, Project};
use crate::time::to_frame;
use serde_json::json;

pub fn write(project: &Project, media: &Media) -> Result<String> {
    let fps = media.frame_rate;
    let clips: Vec<serde_json::Value> = project
        .timeline
        .included_clips()
        .enumerate()
        .map(|(i, c)| {
            json!({
                "OTIO_SCHEMA": "Clip.1",
                "name": format!("{} clip {}", project.name, i + 1),
                "source_range": {
                    "OTIO_SCHEMA": "TimeRange.1",
                    "start_time": { "OTIO_SCHEMA": "RationalTime.1", "rate": fps, "value": to_frame(c.source_in, fps) },
                    "duration": { "OTIO_SCHEMA": "RationalTime.1", "rate": fps, "value": to_frame(c.duration(), fps) }
                },
                "media_reference": {
                    "OTIO_SCHEMA": "ExternalReference.1",
                    "target_url": format!("file://{}", media.file_path)
                }
            })
        })
        .collect();
    let doc = json!({
        "OTIO_SCHEMA": "Timeline.1",
        "name": project.name,
        "tracks": {
            "OTIO_SCHEMA": "Stack.1",
            "name": "tracks",
            "children": [{
                "OTIO_SCHEMA": "Track.1",
                "name": "V1",
                "kind": "Video",
                "children": clips
            }]
        }
    });
    Ok(serde_json::to_string_pretty(&doc)?)
}
