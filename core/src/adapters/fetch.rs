//! Downloading one video from a link with yt-dlp (the `import_url` tool).
//!
//! yt-dlp and Deno (the JavaScript runtime YouTube needs) are the app's own
//! copies in `<data dir>/bin/`, installed by the user in Setup. A copy on
//! PATH or from Homebrew is never used, so link import is on only after that
//! install, and Remove turns it off (ADR-0009).
//!
//! Two yt-dlp runs per import: a metadata pass that refuses playlists and
//! live streams before anything is downloaded, then the download itself.

use crate::error::{CoreError, Result};
use crate::events::{ProgressTask, SharedSink};
use crate::progress::{stream_progress, Delimiter};
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::process::Command;

/// What the metadata pass learned about the link.
#[derive(Debug, Clone)]
pub struct Probe {
    /// The video's own page (a playlist entry's link resolves to it).
    pub webpage_url: String,
    pub id: String,
    pub title: String,
    pub duration: Option<f64>,
    pub size_approx: Option<u64>,
    /// The streams the download will fetch, in order: `(format_id, kind)`,
    /// kind "video" or "audio" (one entry when the site serves one file).
    pub streams: Vec<(String, &'static str)>,
}

#[derive(Debug, Clone)]
pub struct Fetched {
    pub file: PathBuf,
    /// The file was already in the folder (same video id): nothing was
    /// downloaded, and the file isn't ours to delete.
    pub reused: bool,
}

/// The download seam. `YtDlpFetcher` is the real one; tests use
/// [`MockFetcher`].
#[async_trait]
pub trait UrlFetcher: Send + Sync {
    /// Link import is on and its tools are present.
    fn ready(&self) -> Result<()>;
    /// The yt-dlp version, for the log ("" when unknown).
    fn version(&self) -> String;
    async fn probe(&self, url: &str) -> Result<Probe>;
    /// Download into `temp` and move the finished file into `home`.
    /// `on_pid` receives the child's process id so a quit can stop it.
    async fn download(
        &self,
        probe: &Probe,
        home: &Path,
        temp: &Path,
        sink: &SharedSink,
        on_pid: &(dyn Fn(u32) + Send + Sync),
    ) -> Result<Fetched>;
}

// ------------------------------------------------------------- the binaries

pub fn bin_dir() -> PathBuf {
    crate::store::data_dir().join("bin")
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// The app's own yt-dlp (`YTDLP_PATH` overrides it, for development).
pub fn ytdlp_path() -> PathBuf {
    std::env::var_os("YTDLP_PATH").map(PathBuf::from).unwrap_or_else(|| bin_dir().join(exe("yt-dlp")))
}

/// The app's own Deno (`DENO_PATH` overrides it).
pub fn deno_path() -> PathBuf {
    std::env::var_os("DENO_PATH").map(PathBuf::from).unwrap_or_else(|| bin_dir().join(exe("deno")))
}

/// Versions recorded at install time; status reads them and never runs the
/// binaries (`<data dir>/bin/link-import.json`).
pub fn versions_file() -> PathBuf {
    bin_dir().join("link-import.json")
}

pub fn recorded_versions() -> (Option<String>, Option<String>) {
    let Ok(text) = std::fs::read_to_string(versions_file()) else { return (None, None) };
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return (None, None) };
    let get = |k: &str| v[k].as_str().map(str::to_owned);
    (get("ytdlp"), get("deno"))
}

pub const OFF_MESSAGE: &str =
    "link import is off: install yt-dlp in RoughCut's Setup (only you can turn it on)";

// ------------------------------------------------------------- yt-dlp args

/// Format choice: H.264 video and AAC audio up to 1080p, else the best up to
/// 1080p, remuxed to MP4 where the codecs allow.
const FORMAT_ARGS: &[&str] = &[
    "-f",
    "bv*+ba/b",
    "-S",
    "res:1080,vcodec:h264,acodec:aac",
    "--merge-output-format",
    "mp4",
    "--remux-video",
    "mp4",
];

/// Arguments every run shares. The user's own yt-dlp config is ignored, no
/// cookies are passed, and the link always comes last, after `--`.
fn common_args(deno: Option<&Path>, ffmpeg: Option<&Path>) -> Vec<String> {
    let mut a: Vec<String> = ["--ignore-config", "--no-playlist", "--color", "no_color"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if let Some(d) = deno {
        a.push("--js-runtimes".into());
        a.push(format!("deno:{}", d.display()));
    }
    if let Some(f) = ffmpeg {
        a.push("--ffmpeg-location".into());
        a.push(f.display().to_string());
    }
    a.extend(FORMAT_ARGS.iter().map(|s| s.to_string()));
    a
}

pub fn probe_args(url: &str, deno: Option<&Path>, ffmpeg: Option<&Path>) -> Vec<String> {
    let mut a = common_args(deno, ffmpeg);
    a.extend(["-J", "--flat-playlist", "--"].iter().map(|s| s.to_string()));
    a.push(url.to_string());
    a
}

pub fn download_args(url: &str, home: &Path, temp: &Path, deno: Option<&Path>, ffmpeg: Option<&Path>) -> Vec<String> {
    let mut a = common_args(deno, ffmpeg);
    for s in [
        "-P".to_string(),
        format!("home:{}", home.display()),
        "-P".to_string(),
        format!("temp:{}", temp.display()),
        "-o".to_string(),
        "%(title).150B [%(id)s].%(ext)s".to_string(),
        "--windows-filenames".to_string(),
        "--no-overwrites".to_string(),
        "--newline".to_string(),
        "--progress".to_string(),
        "--progress-template".to_string(),
        "download:RCPROG %(info.format_id)s %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s".to_string(),
        "-O".to_string(),
        "after_move:RCFILE %(filepath)s".to_string(),
        "--".to_string(),
        url.to_string(),
    ] {
        a.push(s);
    }
    a
}

// ------------------------------------------------------------- parsing

/// Read the metadata pass's JSON: refuse playlists and live streams.
pub fn parse_probe(json: &Value, url: &str) -> Result<Probe> {
    let kind = json["_type"].as_str().unwrap_or("video");
    if kind == "playlist" || kind == "multi_video" {
        return Err(CoreError::InvalidArg("this link is a playlist; import one video at a time".into()));
    }
    match json["live_status"].as_str() {
        Some("is_live") => {
            return Err(CoreError::InvalidArg("this is a live stream; import it after it ends".into()))
        }
        Some("is_upcoming") => {
            return Err(CoreError::InvalidArg(
                "this live stream hasn't started; import it after it ends".into(),
            ))
        }
        _ if json["is_live"].as_bool() == Some(true) => {
            return Err(CoreError::InvalidArg("this is a live stream; import it after it ends".into()))
        }
        _ => {}
    }
    let id = json["id"].as_str().unwrap_or("").to_string();
    let title = json["title"].as_str().filter(|t| !t.trim().is_empty()).unwrap_or("Untitled video").to_string();
    let webpage_url = json["webpage_url"].as_str().unwrap_or(url).to_string();
    let duration = json["duration"].as_f64();
    let size_of = |f: &Value| f["filesize"].as_u64().or_else(|| f["filesize_approx"].as_u64());
    let streams: Vec<(String, &'static str)> = match json["requested_formats"].as_array() {
        Some(fs) if !fs.is_empty() => fs
            .iter()
            .map(|f| {
                let kind = if f["vcodec"].as_str().map(|v| v != "none").unwrap_or(true) { "video" } else { "audio" };
                (f["format_id"].as_str().unwrap_or("").to_string(), kind)
            })
            .collect(),
        _ => vec![(json["format_id"].as_str().unwrap_or("").to_string(), "video")],
    };
    let size_approx = match json["requested_formats"].as_array() {
        Some(fs) if !fs.is_empty() => fs.iter().map(size_of).sum::<Option<u64>>(),
        _ => size_of(json),
    }
    .or_else(|| json["filesize_approx"].as_u64());
    Ok(Probe { webpage_url, id, title, duration, size_approx, streams })
}

/// One `RCPROG <format_id> <downloaded> <total> <estimate>` line →
/// `(fraction, message)` across all streams. `NA` fields are skipped.
pub fn parse_progress(line: &str, streams: &[(String, &'static str)]) -> Option<(f64, String)> {
    let rest = line.trim().strip_prefix("RCPROG ")?;
    let mut parts = rest.split_whitespace();
    let format_id = parts.next()?;
    let done: f64 = parts.next()?.parse().ok()?;
    let total = parts
        .next()
        .and_then(|t| t.parse::<f64>().ok())
        .or_else(|| parts.next().and_then(|t| t.parse::<f64>().ok()))
        .filter(|t| *t > 0.0)?;
    let part = (done / total).clamp(0.0, 1.0);
    let n = streams.len().max(1);
    let index = streams.iter().position(|(id, _)| id == format_id).unwrap_or(0);
    let fraction = (index as f64 + part) / n as f64;
    let percent = (part * 100.0).round() as i64;
    let message = if n > 1 {
        let kind = streams.get(index).map(|s| s.1).unwrap_or("video");
        format!("downloading {kind} {}/{n}: {percent}%", index + 1)
    } else {
        format!("downloading: {percent}%")
    };
    Some((fraction.min(0.99), message))
}

/// The reason to show for a failed run: yt-dlp's last `ERROR:` line, without
/// the prefix and the `[site] id:` tag.
pub fn error_reason(stderr: &str) -> Option<String> {
    let line = stderr.lines().rev().find(|l| l.trim_start().starts_with("ERROR:"))?;
    let mut text = line.trim_start().trim_start_matches("ERROR:").trim().to_string();
    if text.starts_with('[') {
        if let Some(close) = text.find("] ") {
            text = text[close + 2..].to_string();
            if let Some(colon) = text.find(": ") {
                if !text[..colon].contains(' ') {
                    text = text[colon + 2..].to_string();
                }
            }
        }
    }
    Some(text)
}

/// A file in `home` whose name carries `[id]` (yt-dlp's file name), newest
/// first: the fallback when `after_move` printed nothing.
pub fn find_by_id(home: &Path, id: &str) -> Option<PathBuf> {
    if id.is_empty() {
        return None;
    }
    let tag = format!("[{id}].");
    std::fs::read_dir(home)
        .ok()?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains(&tag))
        .filter(|e| e.path().is_file())
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

// ------------------------------------------------------------- yt-dlp

pub struct YtDlpFetcher;

impl YtDlpFetcher {
    fn deno() -> Option<PathBuf> {
        let d = deno_path();
        d.is_file().then_some(d)
    }

    /// yt-dlp's standalone build is a launcher that runs the real yt-dlp
    /// as a child, which runs ffmpeg in turn. Its own process group lets a
    /// quit stop all of them at once ([`crate::engine::Editor::cancel_imports`]).
    fn command(args: &[String]) -> Command {
        let mut cmd = Command::new(ytdlp_path());
        cmd.args(args).stdin(Stdio::null()).kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        cmd
    }
}

#[async_trait]
impl UrlFetcher for YtDlpFetcher {
    fn ready(&self) -> Result<()> {
        if !ytdlp_path().is_file() {
            return Err(CoreError::Unavailable(OFF_MESSAGE.into()));
        }
        if crate::adapters::video::ffmpeg_path().is_none() {
            return Err(CoreError::Unavailable(
                "link import needs ffmpeg to join video and audio; install it (Setup shows how)".into(),
            ));
        }
        Ok(())
    }

    fn version(&self) -> String {
        recorded_versions().0.unwrap_or_default()
    }

    async fn probe(&self, url: &str) -> Result<Probe> {
        let ffmpeg = crate::adapters::video::ffmpeg_path();
        let args = probe_args(url, Self::deno().as_deref(), ffmpeg.as_deref());
        let out = Self::command(&args)
            .output()
            .await
            .map_err(|e| CoreError::Unavailable(format!("couldn't run yt-dlp: {e}")))?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() {
            return Err(CoreError::Unavailable(
                error_reason(&stderr).unwrap_or_else(|| format!("yt-dlp couldn't read this link ({})", out.status)),
            ));
        }
        let json: Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| CoreError::Unavailable(format!("yt-dlp gave an unreadable answer: {e}")))?;
        parse_probe(&json, url)
    }

    async fn download(
        &self,
        probe: &Probe,
        home: &Path,
        temp: &Path,
        sink: &SharedSink,
        on_pid: &(dyn Fn(u32) + Send + Sync),
    ) -> Result<Fetched> {
        let existing = find_by_id(home, &probe.id);
        let ffmpeg = crate::adapters::video::ffmpeg_path();
        let args = download_args(&probe.webpage_url, home, temp, Self::deno().as_deref(), ffmpeg.as_deref());
        let mut child = Self::command(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| CoreError::Unavailable(format!("couldn't run yt-dlp: {e}")))?;
        if let Some(pid) = child.id() {
            on_pid(pid);
        }

        // Progress can arrive on either stream (yt-dlp is quiet because of
        // `-O`); both go through the one progress seam. The file path and
        // the errors are collected on the side.
        let file: Arc<Mutex<Option<String>>> = Arc::default();
        let stderr_text: Arc<Mutex<String>> = Arc::default();
        let reader = |stream_file: Arc<Mutex<Option<String>>>, keep: Option<Arc<Mutex<String>>>| {
            let streams = probe.streams.clone();
            move |line: &str| {
                if let Some(path) = line.trim().strip_prefix("RCFILE ") {
                    *stream_file.lock().unwrap() = Some(path.to_string());
                    return None;
                }
                if let Some(keep) = &keep {
                    if !line.starts_with("RCPROG ") {
                        let mut t = keep.lock().unwrap();
                        t.push_str(line);
                        t.push('\n');
                    }
                }
                parse_progress(line, &streams)
            }
        };
        let out_task = child.stdout.take().map(|s| {
            stream_progress(s, sink.clone(), ProgressTask::UrlImport, None, Delimiter::Newline, reader(file.clone(), None))
        });
        let err_task = child.stderr.take().map(|s| {
            stream_progress(
                s,
                sink.clone(),
                ProgressTask::UrlImport,
                None,
                Delimiter::Newline,
                reader(file.clone(), Some(stderr_text.clone())),
            )
        });
        let status = child.wait().await.map_err(|e| CoreError::Unavailable(format!("yt-dlp: {e}")))?;
        for t in [out_task, err_task].into_iter().flatten() {
            let _ = t.await;
        }
        if !status.success() {
            let stderr = stderr_text.lock().unwrap().clone();
            return Err(CoreError::Unavailable(
                error_reason(&stderr).unwrap_or_else(|| format!("yt-dlp stopped ({status})")),
            ));
        }
        let path = file
            .lock()
            .unwrap()
            .clone()
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .or_else(|| find_by_id(home, &probe.id))
            .ok_or_else(|| CoreError::Unavailable("yt-dlp finished without a video file".into()))?;
        let reused = existing.as_deref() == Some(path.as_path());
        Ok(Fetched { file: path, reused })
    }
}

// ------------------------------------------------------------- tests' fetcher

/// Offline stand-in. Links containing `playlist`, `live` or `fail` behave
/// like those cases; anything else "downloads" a small file.
pub struct MockFetcher;

#[async_trait]
impl UrlFetcher for MockFetcher {
    fn ready(&self) -> Result<()> {
        Ok(())
    }

    fn version(&self) -> String {
        "mock".into()
    }

    async fn probe(&self, url: &str) -> Result<Probe> {
        let json = if url.contains("playlist") {
            serde_json::json!({ "_type": "playlist", "id": "PL1", "title": "A playlist" })
        } else if url.contains("live") {
            serde_json::json!({ "id": "live1", "title": "A stream", "live_status": "is_live" })
        } else {
            serde_json::json!({
                "id": "mock123",
                "title": "Mock video",
                "webpage_url": url,
                "duration": 300.0,
                "format_id": "137+140",
                "requested_formats": [
                    { "format_id": "137", "vcodec": "avc1.640028", "filesize": 1000 },
                    { "format_id": "140", "vcodec": "none", "filesize": 200 }
                ]
            })
        };
        parse_probe(&json, url)
    }

    async fn download(
        &self,
        probe: &Probe,
        home: &Path,
        temp: &Path,
        _sink: &SharedSink,
        _on_pid: &(dyn Fn(u32) + Send + Sync),
    ) -> Result<Fetched> {
        std::fs::create_dir_all(temp)?;
        std::fs::write(temp.join("partial.part"), b"partial")?;
        if probe.webpage_url.contains("fail") {
            return Err(CoreError::Unavailable("Video unavailable. This video is private".into()));
        }
        let existing = find_by_id(home, &probe.id);
        let file = home.join(format!("{} [{}].mp4", probe.title, probe.id));
        if existing.is_none() {
            std::fs::write(&file, b"mock video")?;
        }
        Ok(Fetched { reused: existing.is_some(), file })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_run_ignores_config_and_puts_the_link_last_after_a_double_dash() {
        let home = Path::new("/m");
        let temp = Path::new("/m/.roughcut-x");
        let deno = Path::new("/bin/deno");
        for args in [
            probe_args("-o/tmp/x", Some(deno), None),
            download_args("-o/tmp/x", home, temp, Some(deno), Some(Path::new("/bin/ffmpeg"))),
        ] {
            assert!(args.contains(&"--ignore-config".to_string()), "{args:?}");
            assert!(args.contains(&"--no-playlist".to_string()));
            assert_eq!(&args[args.len() - 2..], ["--", "-o/tmp/x"], "{args:?}");
            let js = args.iter().position(|a| a == "--js-runtimes").unwrap();
            assert_eq!(args[js + 1], "deno:/bin/deno");
        }
        let d = download_args("https://x", home, temp, None, None);
        assert!(!d.contains(&"--js-runtimes".to_string()), "no runtime, no flag");
        assert!(d.contains(&"home:/m".to_string()) && d.contains(&"temp:/m/.roughcut-x".to_string()));
        assert!(d.contains(&"--no-overwrites".to_string()));
    }

    #[test]
    fn the_metadata_pass_refuses_playlists_and_live_streams() {
        let err = |v: Value| parse_probe(&v, "https://x").unwrap_err().to_string();
        assert!(err(json!({"_type": "playlist"})).contains("playlist"));
        assert!(err(json!({"_type": "multi_video"})).contains("playlist"));
        assert!(err(json!({"live_status": "is_live"})).contains("live stream"));
        assert!(err(json!({"live_status": "is_upcoming"})).contains("hasn't started"));
        assert!(err(json!({"is_live": true})).contains("live stream"));
        let ok = parse_probe(&json!({"live_status": "was_live", "id": "a", "title": "T"}), "https://x").unwrap();
        assert_eq!((ok.id.as_str(), ok.title.as_str(), ok.webpage_url.as_str()), ("a", "T", "https://x"));
    }

    #[test]
    fn the_probe_reads_the_streams_in_download_order() {
        let p = parse_probe(
            &json!({
                "id": "v", "title": "T", "webpage_url": "https://site/v",
                "requested_formats": [
                    {"format_id": "137", "vcodec": "avc1", "filesize": 10},
                    {"format_id": "140", "vcodec": "none", "filesize_approx": 5}
                ]
            }),
            "https://short/v",
        )
        .unwrap();
        assert_eq!(p.streams, vec![("137".to_string(), "video"), ("140".to_string(), "audio")]);
        assert_eq!(p.size_approx, Some(15));
        assert_eq!(p.webpage_url, "https://site/v");
        let single = parse_probe(&json!({"id": "v", "title": "T", "format_id": "18"}), "https://x").unwrap();
        assert_eq!(single.streams, vec![("18".to_string(), "video")]);
    }

    #[test]
    fn progress_spans_the_video_then_the_audio_download() {
        let streams = vec![("137".to_string(), "video"), ("140".to_string(), "audio")];
        let (f, m) = parse_progress("RCPROG 137 50 100 NA", &streams).unwrap();
        assert!((f - 0.25).abs() < 1e-9, "{f}");
        assert_eq!(m, "downloading video 1/2: 50%");
        let (f, m) = parse_progress("RCPROG 140 NA NA NA", &streams).map_or((0.0, String::new()), |x| x);
        assert_eq!((f, m.as_str()), (0.0, ""), "no numbers, no event");
        let (f, m) = parse_progress("RCPROG 140 30 NA 60", &streams).unwrap();
        assert!((f - 0.75).abs() < 1e-9, "estimate used when the total is NA: {f}");
        assert_eq!(m, "downloading audio 2/2: 50%");
        let one = vec![("18".to_string(), "video")];
        assert_eq!(parse_progress("RCPROG 18 1 4 NA", &one).unwrap().1, "downloading: 25%");
        assert!(parse_progress("[download] 10%", &one).is_none());
    }

    #[test]
    fn the_error_shown_is_ytdlps_last_error_line_without_its_tags() {
        let stderr = "WARNING: something\nERROR: [youtube] abc123: Private video. Sign in if you've been granted access\n";
        assert_eq!(error_reason(stderr).unwrap(), "Private video. Sign in if you've been granted access");
        assert_eq!(error_reason("ERROR: Unsupported URL: https://x").unwrap(), "Unsupported URL: https://x");
        assert!(error_reason("all fine").is_none());
    }

    #[test]
    fn a_file_is_found_by_the_video_id_in_its_name() {
        let dir = std::env::temp_dir().join(format!("roughcut-fetch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Talk [abc].mp4"), b"x").unwrap();
        std::fs::write(dir.join("Other [zzz].mp4"), b"x").unwrap();
        assert_eq!(find_by_id(&dir, "abc"), Some(dir.join("Talk [abc].mp4")));
        assert_eq!(find_by_id(&dir, "nope"), None);
        assert_eq!(find_by_id(&dir, ""), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
