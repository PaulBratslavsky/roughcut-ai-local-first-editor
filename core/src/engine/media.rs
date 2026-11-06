//! Media import and transcription, including the background LLM cleanup +
//! semantic-index pass that follows a real transcription.

use super::Editor;
use crate::applog;
use crate::error::{CoreError, Result};
use crate::events::{send, CoreEvent, ProgressTask};
use crate::model::*;
use chrono::Utc;
use uuid::Uuid;

impl Editor {
    pub async fn import_media(
        &self,
        project_id: Option<Uuid>,
        file_path: &str,
    ) -> Result<Media> {
        let mut media = self.inner.video.probe(file_path).await?;
        if let Some(pid) = project_id {
            self.ensure_loaded(pid)?;
            {
                let mut state = self.inner.state.lock().unwrap();
                let entry =
                    state.get_mut(&pid).ok_or_else(|| CoreError::NotFound("project".into()))?;
                let project =
                    entry.project.as_mut().ok_or_else(|| CoreError::NotFound("project".into()))?;
                match &project.media {
                    // The same video at a new path (moved or renamed): relink.
                    // Cuts, history, transcript and clips all still apply.
                    Some(current) if same_source(current, &media) => {
                        media.id = current.id;
                        media.source_url = current.source_url.clone();
                    }
                    // A different video would leave every cut, undo step,
                    // checkpoint, transcript and saved clip describing the
                    // old one — refuse rather than silently invalidate them.
                    Some(current) => {
                        return Err(CoreError::InvalidArg(format!(
                            "this project already has a different video ({:.1}s, {}x{}); \
                             create a new project for it, or use append_media to add it to the end",
                            current.duration, current.width, current.height
                        )))
                    }
                    None => project.timeline = Timeline::new(media.duration),
                }
                project.media = Some(media.clone());
                project.updated_at = Utc::now();
                self.inner.store.save_project(project)?;
            }
            send(&self.inner.sink, CoreEvent::TimelineChanged { project_id: pid });
        }
        Ok(media)
    }

    pub async fn transcribe(&self, project_id: Uuid, language: Option<&str>) -> Result<Transcript> {
        self.ensure_loaded(project_id)?;
        let media = self
            .with_project(project_id, |p, _| Ok(p.media.clone()))?
            .ok_or_else(|| CoreError::InvalidArg("project has no media; import first".into()))?;
        let prefs = self.inner.store.load_preferences()?;
        let lang = language.map(|s| s.to_string()).unwrap_or(prefs.language);
        let demo = self.demo_mode();
        send(&self.inner.sink, CoreEvent::progress(ProgressTask::Transcribe, Some(project_id), 0.0, "starting"));

        // The app log: start, then end or the stage that failed. A start
        // with neither after it was cut off (quit or crash).
        let started = std::time::Instant::now();
        let (engine, whisper_model) = if demo {
            ("mock", None)
        } else {
            let engine = if crate::adapters::transcribe::whisper_native_enabled() { "whisper-rs" } else { "whisper-cli" };
            let model = crate::adapters::transcribe::resolve_whisper_model()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
            (engine, model)
        };
        applog::write(
            applog::Level::Info,
            applog::Event::TranscribeStart { project: project_id, media_s: media.duration, language: lang.clone(), engine, whisper_model },
        );
        let failed = |stage: &'static str, e: CoreError| {
            applog::write(
                applog::Level::Error,
                applog::Event::TranscribeFailed {
                    project: project_id,
                    stage,
                    ms: started.elapsed().as_millis() as u64,
                    code: e.code(),
                    error: applog::ErrText::from(&e),
                },
            );
            e
        };

        let transcript = if demo {
            self.inner
                .transcriber
                .transcribe(&media, "", &lang, Some(project_id), &self.inner.sink)
                .await
                .map_err(|e| failed("whisper", e))?
        } else {
            let wav = std::env::temp_dir().join(format!("roughcut-{}.wav", media.id));
            let wav_str = wav.to_string_lossy().to_string();
            send(
                &self.inner.sink,
                CoreEvent::progress(ProgressTask::Transcribe, Some(project_id), 0.1, "extracting audio"),
            );
            self.inner.video.extract_audio_wav(&media, &wav_str).await.map_err(|e| failed("audio", e))?;
            send(
                &self.inner.sink,
                CoreEvent::progress(ProgressTask::Transcribe, Some(project_id), 0.25, "transcribing on-device"),
            );
            let t = self.inner.transcriber.transcribe(&media, &wav_str, &lang, Some(project_id), &self.inner.sink).await;
            let _ = std::fs::remove_file(&wav);
            t.map_err(|e| failed("whisper", e))?
        };

        {
            let mut state = self.inner.state.lock().unwrap();
            let entry = state
                .get_mut(&project_id)
                .ok_or_else(|| failed("save", CoreError::NotFound("project".into())))?;
            entry.transcript = Some(transcript.clone());
            // Rough-cut suggestions reference the OLD segment ids — a fresh
            // transcribe replaces every id, so the old flags are stale.
            if let Some(p) = entry.project.as_mut() {
                if !p.suggestions.is_empty() {
                    p.suggestions.clear();
                    let _ = self.inner.store.save_project(p);
                }
            }
        }
        self.inner.store.save_transcript(project_id, &transcript).map_err(|e| failed("save", e))?;
        applog::write(
            applog::Level::Info,
            applog::Event::TranscribeEnd {
                project: project_id,
                ms: started.elapsed().as_millis() as u64,
                segments: transcript.segments.len(),
                model: transcript.model_used.clone(),
            },
        );
        send(&self.inner.sink, CoreEvent::progress(ProgressTask::Transcribe, Some(project_id), 1.0, "done"));
        send(&self.inner.sink, CoreEvent::TranscriptChanged { project_id });

        // Gemma post-pass (real transcriptions only): clean up casing /
        // punctuation / mis-hearings in the background; the UI refreshes via
        // transcript-changed when it lands. Timestamps are never touched.
        {
            let editor = self.clone();
            tokio::spawn(async move {
                if !demo {
                    if let Ok(n) = crate::agent::clean_transcript(&editor, project_id).await {
                        if n > 0 {
                            send(
                                &editor.inner.sink,
                                CoreEvent::progress(ProgressTask::Transcribe,
                                    Some(project_id),
                                    1.0,
                                    format!("polished {n} segment(s) with {}", editor.model_label()),
                                ),
                            );
                        }
                    }
                }
                // Semantic index AFTER cleanup so vectors match final text.
                let _ = editor.index_transcript(project_id).await;
            });
        }
        Ok(transcript)
    }

    /// Replace segment texts (and word texts when counts align) keeping ALL
    /// timestamps intact — used by the LLM transcript cleanup pass.
    pub fn update_segment_texts(&self, project_id: Uuid, changes: &[(Uuid, String)]) -> Result<u32> {
        self.ensure_loaded(project_id)?;
        let mut applied = 0;
        {
            let mut state = self.inner.state.lock().unwrap();
            let entry =
                state.get_mut(&project_id).ok_or_else(|| CoreError::NotFound("project".into()))?;
            let t = entry
                .transcript
                .as_mut()
                .ok_or_else(|| CoreError::InvalidArg("no transcript".into()))?;
            for (id, text) in changes {
                if let Some(seg) = t.segments.iter_mut().find(|s| s.id == *id) {
                    let new_words: Vec<&str> = text.split_whitespace().collect();
                    if new_words.len() == seg.words.len() {
                        for (w, nw) in seg.words.iter_mut().zip(&new_words) {
                            w.text = (*nw).to_string();
                        }
                    }
                    seg.text = text.clone();
                    applied += 1;
                }
            }
            if applied > 0 {
                t.model_used =
                    format!("{}+llm-cleanup", t.model_used.trim_end_matches("+llm-cleanup"));
                self.inner.store.save_transcript(project_id, t)?;
            }
        }
        if applied > 0 {
            send(&self.inner.sink, CoreEvent::TranscriptChanged { project_id });
        }
        Ok(applied)
    }
}

impl super::Editor {
    /// Append another video to the END of this project's source: lossless
    /// concat into a new file in `<data dir>/media/` (originals untouched),
    /// media swapped, timeline extended with an included clip over the new
    /// span. Existing cuts keep their timestamps. Caller re-transcribes.
    /// Inputs must match codec/resolution (stream copy) — mismatches refuse.
    pub async fn append_media(&self, project_id: uuid::Uuid, file_path: &str) -> crate::error::Result<crate::model::Media> {
        use crate::error::CoreError;
        let current = self.with_project(project_id, |p, _| {
            p.media.clone().ok_or_else(|| CoreError::InvalidArg("project has no media".into()))
        })?;
        if !std::path::Path::new(file_path).is_file() {
            return Err(CoreError::NotFound(format!("file {file_path}")));
        }
        // concat_media carries the codec/resolution guard.
        let combined = crate::adapters::video::concat_media(&[
            current.file_path.clone(),
            file_path.to_string(),
        ])
        .await?;
        let new_media = self.video().probe(&combined).await?;
        let span = crate::model::Clip {
            id: uuid::Uuid::new_v4(),
            source_in: current.duration,
            source_out: new_media.duration,
            included: true,
            origin: crate::model::ClipOrigin::Manual,
            order: 0,
            linked_segment_ids: vec![],
        };
        {
            let mut state = self.inner.state.lock().unwrap();
            let entry = state
                .get_mut(&project_id)
                .ok_or_else(|| CoreError::NotFound(format!("project {project_id}")))?;
            extend_history(entry, &span);
            let project = entry
                .project
                .as_mut()
                .ok_or_else(|| CoreError::NotFound(format!("project {project_id}")))?;
            project.timeline.duration = new_media.duration;
            push_span(&mut project.timeline.clips, &span);
            project.timeline.normalize();
            project.media = Some(new_media.clone());
            self.inner.store.save_project(project)?;
            self.persist_journal(project_id, &entry.undo, &entry.redo)?;
        }
        crate::events::send(
            &self.sink(),
            crate::events::CoreEvent::TimelineChanged { project_id },
        );
        crate::events::send(
            &self.sink(),
            crate::events::CoreEvent::MediaAssetsChanged { project_id },
        );
        Ok(new_media)
    }
}

impl super::Editor {
    /// Attach a screen file that started with the camera file (probed; offset 0).
    pub async fn attach_screen_media(&self, project_id: uuid::Uuid, file_path: &str) -> crate::error::Result<()> {
        let media = self.video().probe(file_path).await?;
        self.ensure_loaded(project_id)?;
        let mut state = self.inner.state.lock().unwrap();
        let entry = state
            .get_mut(&project_id)
            .and_then(|e| e.project.as_mut())
            .ok_or_else(|| crate::error::CoreError::NotFound(format!("project {project_id}")))?;
        entry.screen_media = Some(media);
        self.inner.store.save_project(entry)?;
        Ok(())
    }
}

/// Is `b` the same video as `a` (e.g. the file was moved)? Duration, frame
/// size and frame rate must match.
fn same_source(a: &Media, b: &Media) -> bool {
    (a.duration - b.duration).abs() < 0.05
        && a.width == b.width
        && a.height == b.height
        && (a.frame_rate - b.frame_rate).abs() < 0.01
}

/// Append `span` (an included clip over newly appended footage) to a clip list.
fn push_span(clips: &mut Vec<Clip>, span: &Clip) {
    let mut clip = span.clone();
    clip.order = clips.len() as u32;
    clips.push(clip);
}

/// Undo/redo entries and checkpoints hold full clip snapshots of the source
/// as it was. After an append the source is longer, so each snapshot gets the
/// same new span — otherwise an undo or restore would leave the appended
/// footage outside the timeline's clips.
fn extend_history(entry: &mut super::ProjectState, span: &Clip) {
    for journal in entry.undo.iter_mut().chain(entry.redo.iter_mut()) {
        for op in [&mut journal.inverse, &mut journal.redo] {
            if let EditOp::SetClips { clips, .. } = op {
                push_span(clips, span);
            }
        }
    }
    if let Some(project) = entry.project.as_mut() {
        for checkpoint in &mut project.checkpoints {
            push_span(&mut checkpoint.clips, span);
        }
    }
}

// ------------------------------------------------------------ import_url

/// Removes the import's temporary folder however the download ends.
struct TempFolder(std::path::PathBuf);

impl Drop for TempFolder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const TEMP_PREFIX: &str = ".roughcut-";

impl Editor {
    /// Download one video from a link with yt-dlp and create a project
    /// around it (`import_url`; docs/designs/url-import-and-cli.md,
    /// ADR-0009). The download runs in its own task, so a caller that stops
    /// waiting doesn't stop it: the project still appears.
    pub async fn import_url(&self, url: &str, name: Option<&str>, out_dir: Option<&str>) -> Result<UrlImport> {
        let url = url.trim();
        check_link(url)?;
        let fetcher = self.fetcher();
        fetcher.ready()?;
        let prefs = self.inner.store.load_preferences()?;
        let folder = download_folder(&prefs)?;
        let dir = match out_dir.map(str::trim).filter(|d| !d.is_empty()) {
            Some(d) => folder_inside(&folder, d)?,
            None => folder.clone(),
        };
        std::fs::create_dir_all(&dir)?;
        self.sweep_import_temps(&folder);
        if dir != folder {
            self.sweep_import_temps(&dir);
        }

        let host = host_of(url);
        let started = std::time::Instant::now();
        applog::write(
            applog::Level::Info,
            applog::Event::UrlImportStart { host: host.clone(), ytdlp: fetcher.version() },
        );
        // The log keeps the host only: links, the video id and its title are
        // cut out of yt-dlp's error text (filled in once the probe knows them).
        let known: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(vec![url.to_string()]);
        let failed = |e: CoreError| {
            let text = private_out(&e.to_string(), &known.lock().unwrap());
            applog::write(
                applog::Level::Error,
                applog::Event::UrlImportFailed {
                    host: host.clone(),
                    ms: started.elapsed().as_millis() as u64,
                    code: e.code(),
                    error: applog::ErrText::new(&text),
                },
            );
            e
        };

        send(&self.inner.sink, CoreEvent::progress(ProgressTask::UrlImport, None, 0.0, "reading the link"));
        let probe = fetcher.probe(url).await.map_err(&failed)?;
        known.lock().unwrap().extend([probe.webpage_url.clone(), probe.id.clone(), probe.title.clone()]);
        let size = probe.size_approx.map(|b| format!(" (about {})", human_size(b))).unwrap_or_default();
        send(
            &self.inner.sink,
            CoreEvent::progress(ProgressTask::UrlImport, None, 0.0, format!("downloading “{}”{size}", probe.title)),
        );

        let import_id = Uuid::new_v4();
        let temp = dir.join(format!("{TEMP_PREFIX}{}", import_id.simple()));
        let editor = self.clone();
        let name = name.map(str::trim).filter(|n| !n.is_empty()).map(str::to_owned);
        let task = tokio::spawn(async move {
            let _temp = TempFolder(temp.clone());
            editor.inner.imports.lock().unwrap().insert(import_id, 0);
            let on_pid = {
                let editor = editor.clone();
                move |pid: u32| {
                    editor.inner.imports.lock().unwrap().insert(import_id, pid);
                }
            };
            let sink = editor.sink();
            let fetched = fetcher.download(&probe, &dir, &temp, &sink, &on_pid).await;
            editor.inner.imports.lock().unwrap().remove(&import_id);
            let fetched = fetched?;
            editor.finish_url_import(probe, fetched, name).await
        });
        let result = task
            .await
            .map_err(|e| CoreError::Other(format!("the download task stopped: {e}")))
            .and_then(|r| r)
            .map_err(&failed)?;
        let bytes = std::fs::metadata(&result.file_path).map(|m| m.len()).unwrap_or(0);
        applog::write(
            applog::Level::Info,
            applog::Event::UrlImportEnd { host, ms: started.elapsed().as_millis() as u64, bytes },
        );
        send(&self.inner.sink, CoreEvent::progress(ProgressTask::UrlImport, None, 1.0, "downloaded"));
        Ok(result)
    }

    /// Create the project around the downloaded file and record its link.
    async fn finish_url_import(
        &self,
        probe: crate::adapters::fetch::Probe,
        fetched: crate::adapters::fetch::Fetched,
        name: Option<String>,
    ) -> Result<UrlImport> {
        let file_path = fetched.file.to_string_lossy().into_owned();
        let project_name = name.unwrap_or_else(|| probe.title.clone());
        let project = match self.create_project(&project_name, Some(&file_path)).await {
            Ok(p) => p,
            Err(e) => {
                if !fetched.reused {
                    let _ = std::fs::remove_file(&fetched.file);
                }
                return Err(e);
            }
        };
        let source_url = probe.webpage_url.clone();
        let project = {
            let mut state = self.inner.state.lock().unwrap();
            let entry = state
                .get_mut(&project.id)
                .and_then(|e| e.project.as_mut())
                .ok_or_else(|| CoreError::NotFound("project".into()))?;
            if let Some(media) = entry.media.as_mut() {
                media.source_url = Some(source_url.clone());
            }
            self.inner.store.save_project(entry)?;
            entry.clone()
        };
        send(&self.inner.sink, CoreEvent::ProjectsChanged {});
        let mut warnings = vec![];
        if let Some(media) = &project.media {
            let codec = media.codec.to_ascii_lowercase();
            if !(codec.starts_with("h264") || codec.starts_with("avc")) {
                warnings.push(format!(
                    "the video is {} rather than H.264: the preview gets a playable copy, but Premiere \
                     or Final Cut may not open the original file",
                    media.codec
                ));
            }
        }
        if fetched.reused {
            warnings.push("this video was already in the download folder, so that file was used".into());
        }
        Ok(UrlImport { project, file_path, title: probe.title, source_url, warnings })
    }

    /// Remove `.roughcut-*` folders in `dir` that no running import owns:
    /// what a quit or crash left behind.
    fn sweep_import_temps(&self, dir: &std::path::Path) {
        let running: Vec<String> = self
            .inner
            .imports
            .lock()
            .unwrap()
            .keys()
            .map(|id| format!("{TEMP_PREFIX}{}", id.simple()))
            .collect();
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(TEMP_PREFIX) && entry.path().is_dir() && !running.contains(&name) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
}

/// Only `http` / `https` links, and none naming this computer or a private
/// network (ADR-0009).
fn check_link(url: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|_| CoreError::InvalidArg(format!("not a link: {url}")))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(CoreError::InvalidArg("only http and https links can be imported".into()));
    }
    if super::projects::names_local_host(url) {
        return Err(CoreError::InvalidArg(
            "links to this computer or a private network can't be imported".into(),
        ));
    }
    Ok(())
}

/// `text` without links or any of `known` (the link, the video id, its
/// title), for the log.
fn private_out(text: &str, known: &[String]) -> String {
    let mut out: String = text
        .split_whitespace()
        .map(|w| if w.contains("://") { "<link>" } else { w })
        .collect::<Vec<_>>()
        .join(" ");
    let mut known: Vec<&String> = known.iter().filter(|k| k.chars().count() >= 3).collect();
    known.sort_by_key(|k| std::cmp::Reverse(k.len()));
    for k in known {
        out = out.replace(k.as_str(), "<video>");
    }
    out
}

fn host_of(url: &str) -> String {
    reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_default()
}

/// The download folder: the user's choice in Setup, else
/// `~/Movies/RoughCut/Downloads/` (a subfolder, so downloads don't mix with
/// recordings the removed in-app recorder left in `~/Movies/RoughCut/`).
pub fn download_folder_path(prefs: &Preferences) -> std::path::PathBuf {
    match prefs.download_dir.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        Some(d) => std::path::PathBuf::from(d),
        None => dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("Movies")
            .join("RoughCut")
            .join("Downloads"),
    }
}

/// [`download_folder_path`], created if missing.
fn download_folder(prefs: &Preferences) -> Result<std::path::PathBuf> {
    let dir = download_folder_path(prefs);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// `out_dir` must be the download folder or a folder inside it: a relative
/// path is taken inside it; an absolute one must start with it. `..` is
/// refused, so the path can't climb out.
fn folder_inside(folder: &std::path::Path, out_dir: &str) -> Result<std::path::PathBuf> {
    let expanded = match out_dir.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().map(|h| h.join(rest)).unwrap_or_else(|| out_dir.into()),
        None => std::path::PathBuf::from(out_dir),
    };
    let requested = expanded.as_path();
    let outside = || {
        CoreError::InvalidArg(format!(
            "out_dir must be the download folder ({}) or a folder inside it",
            folder.display()
        ))
    };
    if requested.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err(outside());
    }
    let path = if requested.is_absolute() { requested.to_path_buf() } else { folder.join(requested) };
    let base = folder.canonicalize().unwrap_or_else(|_| folder.to_path_buf());
    if path.starts_with(folder) || path.starts_with(&base) {
        Ok(path)
    } else {
        Err(outside())
    }
}

fn human_size(bytes: u64) -> String {
    let mb = bytes as f64 / 1_000_000.0;
    if mb >= 1000.0 {
        format!("{:.1} GB", mb / 1000.0)
    } else {
        format!("{:.0} MB", mb.max(1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::{extend_history, folder_inside, private_out, same_source};
    use crate::engine::{JournalEntry, ProjectState};
    use crate::model::*;

    #[test]
    fn the_log_never_sees_the_link_the_video_id_or_the_title() {
        let known = vec![
            "https://youtu.be/abcdefghijk".to_string(),
            "abcdefghijk".to_string(),
            "My Secret Talk".to_string(),
        ];
        let text = private_out(
            "[youtube] abcdefghijk: Private video \"My Secret Talk\" at https://www.youtube.com/watch?v=abcdefghijk",
            &known,
        );
        assert!(
            !text.contains("abcdefghijk") && !text.contains("My Secret Talk") && !text.contains("youtube.com"),
            "{text}"
        );
        assert!(text.contains("Private video"), "{text}");
    }

    #[test]
    fn out_dir_stays_inside_the_download_folder() {
        let folder = std::path::Path::new("/Users/me/Movies/RoughCut/Downloads");
        assert_eq!(folder_inside(folder, "talks").unwrap(), folder.join("talks"));
        assert_eq!(
            folder_inside(folder, "/Users/me/Movies/RoughCut/Downloads/a/b").unwrap(),
            folder.join("a/b")
        );
        assert_eq!(folder_inside(folder, folder.to_str().unwrap()).unwrap(), folder);
        for bad in ["/tmp", "../x", "talks/../../x", "/Users/me/Movies"] {
            assert!(folder_inside(folder, bad).is_err(), "{bad}");
        }
    }

    fn media(duration: f64, width: u32) -> Media {
        Media {
            id: uuid::Uuid::new_v4(),
            file_path: "/x.mp4".into(),
            duration,
            frame_rate: 30.0,
            width,
            height: 1080,
            audio_sample_rate: 48000,
            codec: "h264".into(),
            imported_at: chrono::Utc::now(),
            source_url: None,
        }
    }

    #[test]
    fn same_source_needs_matching_duration_size_and_rate() {
        assert!(same_source(&media(120.0, 1920), &media(120.02, 1920)));
        assert!(!same_source(&media(120.0, 1920), &media(95.0, 1920)));
        assert!(!same_source(&media(120.0, 1920), &media(120.0, 3840)));
    }

    #[test]
    fn append_extends_every_snapshot_to_cover_the_new_footage() {
        let mut timeline = Timeline::new(100.0);
        timeline.set_range_included(10.0, 20.0, false, ClipOrigin::Manual);
        let before = timeline.clips.clone();
        let mut project = Project::new("p");
        project.timeline = timeline.clone();
        project.checkpoints.push(Checkpoint {
            id: uuid::Uuid::new_v4(),
            name: "cp".into(),
            created_at: chrono::Utc::now(),
            clips: before.clone(),
            global_padding: Padding::default(),
            cut_count: 1,
            auto: false,
        });
        let snapshot = |clips: Vec<Clip>| EditOp::SetClips { clips, global_padding: Padding::default() };
        let mut entry = ProjectState {
            project: Some(project),
            transcript: None,
            undo: vec![JournalEntry {
                action: EditAction::new(ActionSource::Ui, EditOp::CutRange { start: 10.0, end: 20.0 }),
                inverse: snapshot(Timeline::new(100.0).clips),
                redo: snapshot(before),
            }],
            redo: vec![],
        };
        let span = Clip {
            id: uuid::Uuid::new_v4(),
            source_in: 100.0,
            source_out: 160.0,
            included: true,
            order: 0,
            origin: ClipOrigin::Manual,
            linked_segment_ids: vec![],
        };
        extend_history(&mut entry, &span);

        // Restoring any snapshot over the 160s source keeps the appended
        // minute and still partitions the whole source.
        let covered = |clips: &[Clip]| {
            let mut t = Timeline::new(160.0);
            t.clips = clips.to_vec();
            t.normalize();
            assert_eq!(t.clips.last().unwrap().source_out, 160.0);
            assert!(t.clips.windows(2).all(|w| (w[0].source_out - w[1].source_in).abs() < 1e-9));
            t.included_duration()
        };
        let EditOp::SetClips { clips, .. } = &entry.undo[0].inverse else { unreachable!() };
        assert_eq!(covered(clips), 160.0, "undo to the original keeps the new footage");
        let EditOp::SetClips { clips, .. } = &entry.undo[0].redo else { unreachable!() };
        assert_eq!(covered(clips), 150.0, "the cut survives, plus the new footage");
        let cp = &entry.project.as_ref().unwrap().checkpoints[0];
        assert_eq!(covered(&cp.clips), 150.0);
    }
}
