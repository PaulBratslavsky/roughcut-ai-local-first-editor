//! Export / hand-off: NLE interchange (Premiere/Resolve via FCP7 xmeml,
//! Final Cut via fcpxml, CMX3600 EDL, OTIO JSON), SRT captions, MP4 render.

pub mod edl;
pub mod fcpxml;
pub mod otio;
pub mod srt;
pub mod xmeml;

use crate::engine::Editor;
use crate::error::{CoreError, Result};
use crate::model::{Project, Transcript};
use uuid::Uuid;

pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn require_media(project: &Project) -> Result<&crate::model::Media> {
    project
        .media
        .as_ref()
        .ok_or_else(|| CoreError::InvalidArg("project has no media to export".into()))
}

/// Render a text-based target to a string (everything except mp4).
pub fn render_text_target(
    project: &Project,
    transcript: Option<&Transcript>,
    target: &str,
) -> Result<String> {
    let media = require_media(project)?;
    match target {
        // Premiere and Resolve both import FCP7 "xmeml" XML.
        "premiere_xml" | "resolve_xml" => Ok(xmeml::write(project, media)),
        "fcp_xml" => Ok(fcpxml::write(project, media)),
        "edl" => Ok(edl::write(project, media)),
        "otio" => otio::write(project, media),
        "srt" => {
            let t = transcript
                .ok_or_else(|| CoreError::InvalidArg("no transcript; transcribe first".into()))?;
            Ok(srt::write(project, t))
        }
        other => Err(CoreError::InvalidArg(format!("unknown export target {other}"))),
    }
}

pub fn default_extension(target: &str) -> &'static str {
    match target {
        "premiere_xml" | "resolve_xml" | "fcp_xml" => "xml",
        "edl" => "edl",
        "otio" => "otio",
        "srt" => "srt",
        "mp4" => "mp4",
        _ => "txt",
    }
}

/// Expand a leading `~/` — callers (UI, MCP clients) naturally write
/// `~/Downloads/...`, and an unexpanded tilde becomes a literal directory.
fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().into_owned();
        }
    }
    path.to_string()
}

impl Editor {
    /// The `export` tool: writes the target file and returns its path.
    pub async fn export(&self, project_id: Uuid, target: &str, out_path: &str) -> Result<String> {
        let out_path = &expand_tilde(out_path);
        if target == "mp4" {
            let (project, _) = self.snapshot(project_id)?;
            let media = require_media(&project)?.clone();
            self.video().render_mp4(&media, &project.timeline, out_path, &self.sink()).await?;
            return Ok(out_path.to_string());
        }
        let (project, transcript) = self.snapshot(project_id)?;
        let content = render_text_target(&project, transcript.as_ref(), target)?;
        if let Some(parent) = std::path::Path::new(out_path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(out_path, content)?;
        Ok(out_path.to_string())
    }

    /// Clone of (project, transcript) for read-mostly consumers.
    pub fn snapshot(&self, project_id: Uuid) -> Result<(Project, Option<Transcript>)> {
        let p = self.with_project(project_id, |p, _| Ok(p.clone()))?;
        let t = self.get_transcript(project_id)?;
        Ok((p, t))
    }
}
