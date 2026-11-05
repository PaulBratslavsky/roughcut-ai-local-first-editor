//! `rcmedia://`: the preview's `<video>` and `<audio>` read local files
//! through this protocol instead of Tauri's asset protocol.
//!
//! Tauri's asset protocol answers every range request with at most 1000 KiB,
//! on the main thread. WebKit's player reads an MP4 the way it reads a file:
//! one exact byte range per box or sample, and a shorter answer is a failed
//! read. Some tables in a long recording are bigger than that: a 60 fps video
//! over ~35 minutes has a frame-timing table (`ctts`) over 1000 KiB. WebKit
//! then dropped the video track and played only the sound. A faststart copy
//! also has its chunk-offset table up front, and WebKit refused it outright.
//!
//! Here a range with an end is answered in full (up to `MAX_BOUNDED`), an
//! open-ended one in `OPEN_CHUNK` pieces (the player asks again for the
//! rest), on the blocking thread pool. Which paths may be read is the asset
//! protocol's scope (`tauri.conf.json`), so the two can't drift apart.
//! Thumbnails and waveform peaks still use the asset protocol: they're small
//! and read whole.

use std::fs::File;
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use tauri::http::{header, Method, Request, Response, StatusCode};

pub const SCHEME: &str = "rcmedia";

/// The most one bounded range is answered with. Far above anything a player
/// asks for in one read (a sample table, a keyframe); it only stops a
/// malformed request from loading gigabytes into memory.
const MAX_BOUNDED: u64 = 64 * 1024 * 1024;
/// How much of an open-ended range (`bytes=N-`) one answer carries.
const OPEN_CHUNK: u64 = 8 * 1024 * 1024;

/// The protocol handler: answers on the blocking pool, never the main thread.
pub fn handle<R: tauri::Runtime>(
    ctx: tauri::UriSchemeContext<'_, R>,
    request: Request<Vec<u8>>,
    responder: tauri::UriSchemeResponder,
) {
    use tauri::Manager;
    let scope = ctx.app_handle().asset_protocol_scope();
    tauri::async_runtime::spawn_blocking(move || {
        responder.respond(respond(&request, |p| scope.is_allowed(p)));
    });
}

/// The response to one request. `allowed` decides which files may be read.
pub fn respond(request: &Request<Vec<u8>>, allowed: impl Fn(&Path) -> bool) -> Response<Vec<u8>> {
    let Some(path) = file_path(request.uri().path()) else {
        return status(StatusCode::FORBIDDEN);
    };
    if !allowed(&path) {
        return status(StatusCode::FORBIDDEN);
    }
    let mut file = match File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return status(StatusCode::NOT_FOUND),
        Err(e) if e.kind() == ErrorKind::PermissionDenied => return status(StatusCode::FORBIDDEN),
        Err(_) => return status(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let len = match file.metadata() {
        Ok(m) if m.is_file() => m.len(),
        Ok(_) => return status(StatusCode::FORBIDDEN),
        Err(_) => return status(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let range_header = request.headers().get(header::RANGE).and_then(|v| v.to_str().ok());
    let (start, end, partial) = match range_header {
        Some(h) => match parse_range(h, len) {
            Some((start, end)) => (start, end, true),
            None => {
                return Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{len}"))
                    .body(vec![])
                    .unwrap();
            }
        },
        // No range: a small file whole; a big one starts like an open range.
        None if len <= MAX_BOUNDED => (0, len.saturating_sub(1), false),
        None => (0, OPEN_CHUNK - 1, true),
    };
    let n = if len == 0 { 0 } else { end - start + 1 };
    let mut builder = Response::builder()
        .header(header::CONTENT_TYPE, mime_type(&path))
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, n);
    builder = if partial {
        builder
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"))
    } else {
        builder.status(StatusCode::OK)
    };
    if request.method() == Method::HEAD || n == 0 {
        return builder.body(vec![]).unwrap();
    }
    let mut body = Vec::with_capacity(n as usize);
    let read = file
        .seek(SeekFrom::Start(start))
        .and_then(|_| (&mut file).take(n).read_to_end(&mut body));
    match read {
        Ok(_) if body.len() as u64 == n => builder.body(body).unwrap(),
        // The file shrank under us (still being written): say so, don't send
        // a short body that claims to be complete.
        _ => status(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// The file path in a request: the URL path minus its leading `/`,
/// percent-decoded (`convertFileSrc` encodes the whole path). None when it
/// isn't an absolute path or climbs with `..`.
fn file_path(url_path: &str) -> Option<PathBuf> {
    let encoded = url_path.strip_prefix('/').unwrap_or(url_path);
    let decoded = percent_encoding::percent_decode_str(encoded).decode_utf8().ok()?;
    let path = PathBuf::from(decoded.as_ref());
    let climbs = path.components().any(|c| c == Component::ParentDir);
    (path.is_absolute() && !climbs).then_some(path)
}

/// The byte range to answer, inclusive, from a `Range` header and the file
/// length. One range: of several, the first is answered (the response says
/// which). None = unsatisfiable or malformed.
fn parse_range(header: &str, len: u64) -> Option<(u64, u64)> {
    let spec = header.trim().strip_prefix("bytes=")?;
    let first = spec.split(',').next()?.trim();
    let (a, b) = first.split_once('-')?;
    let (a, b) = (a.trim(), b.trim());
    if len == 0 {
        return None;
    }
    let (start, end) = if a.is_empty() {
        // A suffix: the last N bytes.
        let n: u64 = b.parse().ok()?;
        if n == 0 {
            return None;
        }
        (len.saturating_sub(n), len - 1)
    } else {
        let start: u64 = a.parse().ok()?;
        if start >= len {
            return None;
        }
        if b.is_empty() {
            (start, (start + OPEN_CHUNK - 1).min(len - 1))
        } else {
            let end: u64 = b.parse().ok()?;
            if end < start {
                return None;
            }
            (start, end.min(len - 1).min(start + MAX_BOUNDED - 1))
        }
    };
    Some((start, end))
}

fn mime_type(path: &Path) -> &'static str {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "m4a" => "audio/mp4",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        "ogg" | "opus" => "audio/ogg",
        _ => "application/octet-stream",
    }
}

fn status(code: StatusCode) -> Response<Vec<u8>> {
    Response::builder().status(code).body(vec![]).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// The URL `convertFileSrc(path, "rcmedia")` makes on macOS.
    fn url(path: &Path) -> String {
        let p = path.to_string_lossy();
        format!("rcmedia://localhost/{}", percent_encoding::utf8_percent_encode(&p, percent_encoding::NON_ALPHANUMERIC))
    }

    fn get(path: &Path, range: Option<&str>) -> Response<Vec<u8>> {
        let mut req = Request::builder().uri(url(path));
        if let Some(r) = range {
            req = req.header(header::RANGE, r);
        }
        respond(&req.body(vec![]).unwrap(), |_| true)
    }

    /// A temp file of `n` bytes where byte i is `i % 251`.
    fn file(n: usize) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip with space.mp4");
        let bytes: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
        File::create(&path).unwrap().write_all(&bytes).unwrap();
        (dir, path)
    }

    fn content_range(r: &Response<Vec<u8>>) -> &str {
        r.headers()[header::CONTENT_RANGE].to_str().unwrap()
    }

    #[test]
    fn a_bounded_range_over_1000_kib_is_answered_in_full() {
        // The case that broke long recordings: a 1.8 MB table read in one go.
        let (_d, path) = file(3_000_000);
        let r = get(&path, Some("bytes=100000-1896623"));
        assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(r.body().len(), 1_796_624);
        assert_eq!(content_range(&r), "bytes 100000-1896623/3000000");
        assert_eq!(r.body()[0], (100_000 % 251) as u8);
        assert_eq!(r.headers()[header::CONTENT_TYPE], "video/mp4");
    }

    #[test]
    fn open_ended_and_suffix_ranges() {
        let (_d, path) = file(10_000_000);
        let open = get(&path, Some("bytes=1000-"));
        assert_eq!(open.body().len() as u64, OPEN_CHUNK);
        assert_eq!(content_range(&open), format!("bytes 1000-{}/10000000", 1000 + OPEN_CHUNK - 1));
        let tail = get(&path, Some("bytes=9999990-"));
        assert_eq!(content_range(&tail), "bytes 9999990-9999999/10000000");
        let suffix = get(&path, Some("bytes=-500"));
        assert_eq!(content_range(&suffix), "bytes 9999500-9999999/10000000");
        // An end past the file is clamped to it.
        let past = get(&path, Some("bytes=9999000-20000000"));
        assert_eq!(past.body().len(), 1000);
        // Of several ranges, the first is answered.
        assert_eq!(content_range(&get(&path, Some("bytes=0-1, 5-9"))), "bytes 0-1/10000000");
    }

    #[test]
    fn unsatisfiable_and_malformed_ranges() {
        let (_d, path) = file(100);
        for bad in ["bytes=100-", "bytes=50-10", "bytes=-0", "items=0-1", "bytes=x-y"] {
            let r = get(&path, Some(bad));
            assert_eq!(r.status(), StatusCode::RANGE_NOT_SATISFIABLE, "{bad}");
            assert_eq!(content_range(&r), "bytes */100", "{bad}");
        }
    }

    #[test]
    fn without_a_range_a_small_file_comes_whole() {
        let (_d, path) = file(4096);
        let r = get(&path, None);
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.body().len(), 4096);
        assert!(r.headers().get(header::CONTENT_RANGE).is_none());
    }

    #[test]
    fn head_has_headers_and_no_body() {
        let (_d, path) = file(4096);
        let req = Request::builder().method(Method::HEAD).uri(url(&path));
        let r = respond(&req.body(vec![]).unwrap(), |_| true);
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers()[header::CONTENT_LENGTH], "4096");
        assert!(r.body().is_empty());
    }

    #[test]
    fn scope_missing_files_and_bad_paths() {
        let (_d, path) = file(10);
        let req = || Request::builder().uri(url(&path)).body(vec![]).unwrap();
        assert_eq!(respond(&req(), |_| false).status(), StatusCode::FORBIDDEN);
        assert_eq!(get(&path.with_file_name("gone.mp4"), None).status(), StatusCode::NOT_FOUND);
        assert_eq!(get(path.parent().unwrap(), None).status(), StatusCode::FORBIDDEN, "a directory");
        assert_eq!(file_path("/%2Ftmp%2F..%2Fetc%2Fpasswd"), None, "climbs with ..");
        assert_eq!(file_path("/relative.mp4"), None);
        #[cfg(unix)]
        assert_eq!(file_path("/%2FUsers%2Fme%2Fa%20b.mp4"), Some(PathBuf::from("/Users/me/a b.mp4")));
    }

    #[test]
    fn mime_types_follow_the_extension() {
        assert_eq!(mime_type(Path::new("/a/b.MOV")), "video/quicktime");
        assert_eq!(mime_type(Path::new("/a/b.mkv")), "video/x-matroska");
        assert_eq!(mime_type(Path::new("/a/b")), "application/octet-stream");
    }
}
