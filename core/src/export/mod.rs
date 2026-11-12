//! Export / hand-off: NLE interchange (Premiere/Resolve via FCP7 xmeml,
//! Final Cut via fcpxml, CMX3600 EDL, OTIO JSON), SRT captions, plain-text
//! transcript, MP4 render.

pub mod edl;
pub mod fcpxml;
pub mod otio;
pub mod srt;
pub mod txt;
pub mod xmeml;

use crate::engine::Editor;
use crate::error::{CoreError, Result};
use crate::events::{CoreEvent, EventSink, ProgressTask, SharedSink};
use crate::model::{ClipExportResult, PlaySpan, Project, Transcript};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// A `file://` URL for a local path, percent-encoded so any file name
/// survives: spaces, `&`, `#`, `%`, quotes and non-ASCII letters included.
/// Only unreserved characters and `/` stay as they are.
pub fn file_url(path: &str) -> String {
    let mut url = String::from("file://");
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            url.push(b as char);
        } else {
            url.push_str(&format!("%{b:02X}"));
        }
    }
    url
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
        "txt" => {
            let t = transcript
                .ok_or_else(|| CoreError::InvalidArg("no transcript; transcribe first".into()))?;
            Ok(txt::write(project, t))
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
        "txt" => "txt",
        "mp4" => "mp4",
        _ => "txt",
    }
}

/// Expand a leading `~/` (or a bare `~`) — callers (UI, MCP clients)
/// naturally write `~/Downloads/...`, and an unexpanded tilde becomes a
/// literal directory.
fn expand_tilde(path: &str) -> String {
    if path == "~" {
        if let Some(home) = dirs::home_dir() {
            return home.to_string_lossy().into_owned();
        }
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().into_owned();
        }
    }
    path.to_string()
}

/// Where a write to `path` would really land: absolute, `~` expanded, with
/// the nearest existing ancestor symlink-resolved and the rest appended.
/// None for a relative path or one with `..` (or `.`) parts, so a path can't
/// walk out of a folder after the check.
fn resolve_for_write(path: &str) -> Option<std::path::PathBuf> {
    use std::path::{Component, Path};
    let p = std::path::PathBuf::from(expand_tilde(path));
    if !p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
        return None;
    }
    let mut existing: &Path = &p;
    let mut rest = vec![];
    while !existing.exists() {
        rest.push(existing.file_name()?);
        existing = existing.parent()?;
    }
    let mut out = existing.canonicalize().ok()?;
    for part in rest.iter().rev() {
        out.push(part);
    }
    Some(out)
}

/// True when writing to `target` (a file or folder) lands inside `folder`
/// (an existing folder), after resolving `~` and symlinks. The folder itself
/// counts, so an export into it as a folder is inside.
pub fn is_inside_folder(folder: &str, target: &str) -> bool {
    let Ok(folder) = std::path::Path::new(&expand_tilde(folder)).canonicalize() else {
        return false;
    };
    if !folder.is_dir() {
        return false;
    }
    resolve_for_write(target).is_some_and(|t| t.starts_with(&folder))
}

/// `folder` as an absolute, symlink-resolved path, checked to be an existing
/// folder — the form `trusted_export_folder` is stored in.
pub(crate) fn canonical_folder(folder: &str) -> Result<String> {
    let path = Path::new(&expand_tilde(folder))
        .canonicalize()
        .map_err(|e| CoreError::InvalidArg(format!("folder {folder}: {e}")))?;
    if !path.is_dir() {
        return Err(CoreError::InvalidArg(format!("{folder} is not a folder")));
    }
    Ok(path.to_string_lossy().into_owned())
}

impl Editor {
    /// True when an export an external MCP client asked for may skip the
    /// user's approval: `target` lands inside the trusted export folder the
    /// user set in the app. Never true when no folder is set.
    pub fn export_is_trusted(&self, target: &str) -> bool {
        self.get_preferences()
            .ok()
            .and_then(|p| p.trusted_export_folder)
            .is_some_and(|folder| is_inside_folder(&folder, target))
    }

    /// The `export` tool: writes the target file and returns its path.
    pub async fn export(&self, project_id: Uuid, target: &str, out_path: &str) -> Result<String> {
        let (project, transcript) = self.snapshot(project_id)?;
        let pieces = vec![project.timeline.play_spans()];
        self.write_export(&project, transcript.as_ref(), target, out_path, &pieces, None, &self.sink()).await
    }

    /// Export one saved clip: the same writers, run over the clip's
    /// timeline. The NLE sequence is named after the clip; a vertical clip's
    /// MP4 renders 9:16.
    pub async fn export_clip(
        &self,
        project_id: Uuid,
        clip_id: Uuid,
        target: &str,
        out_path: &str,
    ) -> Result<String> {
        self.export_clip_with(project_id, clip_id, target, out_path, &self.sink()).await
    }

    async fn export_clip_with(
        &self,
        project_id: Uuid,
        clip_id: Uuid,
        target: &str,
        out_path: &str,
        sink: &SharedSink,
    ) -> Result<String> {
        let clip = self.get_clip(project_id, clip_id)?;
        let (mut project, transcript) = self.snapshot(project_id)?;
        let pieces = clip.piece_spans();
        project.name = format!("{} — {}", project.name, clip.title);
        project.timeline = clip.timeline;
        let vertical = (clip.format == crate::model::ClipFormat::Vertical).then_some(clip.crop_x);
        self.write_export(&project, transcript.as_ref(), target, out_path, &pieces, vertical, sink).await
    }

    /// Export several saved clips into one folder, one file each, named
    /// `NN-<title>.<ext>` in the order given. Never overwrites: a name that
    /// exists gets `-2`, `-3`, …. One clip failing doesn't stop the rest; each
    /// result carries its path or its error. Progress is one `export_clips`
    /// task over the whole batch.
    pub async fn export_clips(
        &self,
        project_id: Uuid,
        clip_ids: &[Uuid],
        out_dir: &str,
        target: &str,
    ) -> Result<Vec<ClipExportResult>> {
        if clip_ids.is_empty() {
            return Err(CoreError::InvalidArg("export_clips needs at least one clip id".into()));
        }
        check_target(target)?;
        // An unknown project is one error, not one per clip (and no folder).
        self.with_project(project_id, |_, _| Ok(()))?;
        let dir = PathBuf::from(expand_tilde(out_dir));
        std::fs::create_dir_all(&dir)?;
        let n = clip_ids.len();
        let ext = default_extension(target);
        let mut results = Vec::with_capacity(n);
        for (i, &clip_id) in clip_ids.iter().enumerate() {
            let title = match self.get_clip(project_id, clip_id) {
                Ok(c) => c.title,
                Err(e) => {
                    results.push(ClipExportResult { clip_id, path: None, error: Some(e.to_string()) });
                    continue;
                }
            };
            // Reserved (created empty) before rendering, so two batches into
            // one folder can't pick the same name.
            let path = match reserve_path(&dir, &batch_file_name(i + 1, n, &title, ext)) {
                Ok(p) => p,
                Err(e) => {
                    results.push(ClipExportResult { clip_id, path: None, error: Some(e.to_string()) });
                    continue;
                }
            };
            let sink: SharedSink = Arc::new(BatchSink {
                inner: self.sink(),
                project_id,
                index: i,
                total: n,
                title: title.clone(),
            });
            batch_progress(&self.sink(), project_id, i as f64 / n as f64, format!("{}/{n} · {title}", i + 1));
            let out = self.export_clip_with(project_id, clip_id, target, &path.to_string_lossy(), &sink).await;
            results.push(match out {
                Ok(path) => ClipExportResult { clip_id, path: Some(path), error: None },
                Err(e) => {
                    // The name was ours (reserved above): don't leave a
                    // placeholder or a half-written file behind.
                    let _ = std::fs::remove_file(&path);
                    ClipExportResult { clip_id, path: None, error: Some(e.to_string()) }
                }
            });
        }
        let ok = results.iter().filter(|r| r.path.is_some()).count();
        batch_progress(&self.sink(), project_id, 1.0, format!("exported {ok} of {n}"));
        Ok(results)
    }

    #[allow(clippy::too_many_arguments)]
    async fn write_export(
        &self,
        project: &Project,
        transcript: Option<&Transcript>,
        target: &str,
        out_path: &str,
        pieces: &[Vec<PlaySpan>],
        vertical_crop_x: Option<f64>,
        sink: &SharedSink,
    ) -> Result<String> {
        let out_path = &expand_tilde(out_path);
        if target == "mp4" {
            let media = require_media(project)?.clone();
            let job = crate::adapters::video::RenderJob {
                media: &media,
                pieces,
                out_path,
                audio_offset_s: project.audio_offset_s,
                screen: project.screen_media.as_ref().map(|m| (m, &project.layout)),
                vertical_crop_x,
            };
            self.video().render_mp4(&job, sink).await?;
            return Ok(out_path.to_string());
        }
        let content = render_text_target(project, transcript, target)?;
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

/// Fail early on a target no writer knows (before creating folders).
pub fn check_target(target: &str) -> Result<()> {
    match target {
        "premiere_xml" | "resolve_xml" | "fcp_xml" | "edl" | "otio" | "srt" | "txt" | "mp4" => Ok(()),
        other => Err(CoreError::InvalidArg(format!("unknown export target {other}"))),
    }
}

/// `NN-<slug>.<ext>`: `NN` is the 1-based position, two digits (three past
/// 99); the slug is the title in lowercase ASCII letters and digits, every
/// other run of characters one `-`, at most 40 characters ("clip" if empty).
fn batch_file_name(position: usize, total: usize, title: &str, ext: &str) -> String {
    let width = if total > 99 { 3 } else { 2 };
    format!("{position:0width$}-{}.{ext}", slug(title))
}

fn slug(title: &str) -> String {
    let mut out = String::new();
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let mut out: String = out.trim_matches('-').chars().take(40).collect();
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "clip".into()
    } else {
        out
    }
}

/// Claim `dir/name`, or `dir/<stem>-2.<ext>`, `-3`, … when that exists, by
/// creating it empty (`create_new` is atomic, so a name is never handed out
/// twice). The writer then replaces the placeholder.
fn reserve_path(dir: &Path, name: &str) -> Result<PathBuf> {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (name.to_string(), String::new()),
    };
    for k in 1.. {
        let path = if k == 1 { dir.join(name) } else { dir.join(format!("{stem}-{k}{ext}")) };
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    unreachable!("an unused name exists")
}

fn batch_progress(sink: &SharedSink, project_id: Uuid, fraction: f64, message: String) {
    crate::events::send(sink, CoreEvent::progress(ProgressTask::ExportClips, Some(project_id), fraction, message));
}

/// Folds one clip's render progress into the batch's: an `export` event at
/// fraction `f` for clip `i` of `n` becomes an `export_clips` event at
/// `(i + f) / n`, its message prefixed with the position and title. The
/// per-clip "render complete" is dropped (the batch sends its own end), and
/// every other event passes through.
struct BatchSink {
    inner: SharedSink,
    project_id: Uuid,
    index: usize,
    total: usize,
    title: String,
}

impl EventSink for BatchSink {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        let is_render = event == "progress" && payload["task"] == "export";
        if !is_render {
            return self.inner.emit(event, payload);
        }
        let fraction = payload["fraction"].as_f64().unwrap_or(0.0);
        let message = payload["message"].as_str().unwrap_or("");
        if fraction >= 1.0 {
            return;
        }
        let n = self.total as f64;
        batch_progress(
            &self.inner,
            self.project_id,
            ((self.index as f64 + fraction) / n).clamp(0.0, 1.0),
            format!("{}/{} · {} · {message}", self.index + 1, self.total, self.title),
        );
    }
}

#[cfg(test)]
mod trust_tests {
    use super::is_inside_folder;

    #[test]
    fn only_paths_that_land_inside_the_folder_count() {
        let folder = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let f = folder.path().to_str().unwrap();
        let inside = |rel: &str| folder.path().join(rel).to_string_lossy().into_owned();

        assert!(is_inside_folder(f, &inside("cut.mp4")));
        assert!(is_inside_folder(f, &inside("new/sub/cut.mp4")), "folders that don't exist yet");
        assert!(is_inside_folder(f, f), "the folder itself, as an export_clips out_dir");
        assert!(!is_inside_folder(f, &other.path().join("cut.mp4").to_string_lossy()));
        assert!(!is_inside_folder(f, &format!("{f}/../escape.mp4")), "`..` never counts");
        assert!(!is_inside_folder(f, "cut.mp4"), "relative paths never count");
        assert!(!is_inside_folder("/no/such/folder", &inside("cut.mp4")));
        // A sibling whose name starts with the folder's name is not inside it.
        let sibling = format!("{f}-evil/cut.mp4");
        assert!(!is_inside_folder(f, &sibling));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(other.path(), folder.path().join("link")).unwrap();
            assert!(!is_inside_folder(f, &inside("link/cut.mp4")), "symlinks out of the folder");
        }
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn file_urls_encode_every_character_a_path_can_hold() {
        assert_eq!(file_url("/Users/me/clip.mp4"), "file:///Users/me/clip.mp4");
        assert_eq!(
            file_url("/Users/me/Tom & Jerry #2 100%.mov"),
            "file:///Users/me/Tom%20%26%20Jerry%20%232%20100%25.mov"
        );
        assert_eq!(file_url("/v/\"a\"'b'<c>.mp4"), "file:///v/%22a%22%27b%27%3Cc%3E.mp4");
        assert_eq!(file_url("/v/café.mp4"), "file:///v/caf%C3%A9.mp4");
    }

    #[test]
    fn batch_names_pad_slug_and_never_collide() {
        assert_eq!(batch_file_name(3, 6, "Why Timelines Are a Trap!", "mp4"), "03-why-timelines-are-a-trap.mp4");
        assert_eq!(batch_file_name(7, 120, "x", "srt"), "007-x.srt");
        assert_eq!(batch_file_name(1, 1, "¿Qué? — ", "mp4"), "01-qu.mp4");
        assert_eq!(batch_file_name(1, 1, "日本語", "mp4"), "01-clip.mp4");
        let long = "a".repeat(39) + " bcd";
        assert_eq!(slug(&long), "a".repeat(39), "cut to 40, no trailing dash");

        let dir = std::env::temp_dir().join(format!("roughcut-batch-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(reserve_path(&dir, "01-a.mp4").unwrap(), dir.join("01-a.mp4"));
        assert!(dir.join("01-a.mp4").exists(), "reserved on disk");
        assert_eq!(reserve_path(&dir, "01-a.mp4").unwrap(), dir.join("01-a-2.mp4"));
        assert_eq!(reserve_path(&dir, "01-a.mp4").unwrap(), dir.join("01-a-3.mp4"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    struct Capture(Mutex<Vec<(String, serde_json::Value)>>);
    impl EventSink for Capture {
        fn emit(&self, event: &str, payload: serde_json::Value) {
            self.0.lock().unwrap().push((event.to_string(), payload));
        }
    }

    #[test]
    fn batch_sink_folds_render_progress_into_the_batch() {
        let capture = Arc::new(Capture(Mutex::new(vec![])));
        let pid = Uuid::new_v4();
        let sink: SharedSink =
            Arc::new(BatchSink { inner: capture.clone(), project_id: pid, index: 1, total: 4, title: "Hook".into() });
        crate::events::send(&sink, CoreEvent::progress(ProgressTask::Export, None, 0.5, "rendering MP4… 50%"));
        crate::events::send(&sink, CoreEvent::progress(ProgressTask::Export, None, 1.0, "render complete"));
        crate::events::send(&sink, CoreEvent::ClipsChanged { project_id: pid });
        let got = capture.0.lock().unwrap();
        assert_eq!(got.len(), 2, "render complete is dropped; other events pass");
        assert_eq!(got[0].1["task"], "export_clips");
        assert!((got[0].1["fraction"].as_f64().unwrap() - 0.375).abs() < 1e-9);
        assert_eq!(got[0].1["message"], "2/4 · Hook · rendering MP4… 50%");
        assert_eq!(got[0].1["project_id"], pid.to_string());
        assert_eq!(got[1].0, "clips-changed");
    }

}
