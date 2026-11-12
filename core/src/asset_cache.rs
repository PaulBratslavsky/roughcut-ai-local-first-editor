//! The asset cache, `<data_dir>/cache/`: files derived from source media
//! (waveform peaks, the thumbnail filmstrip, the preview copy) that can
//! always be rebuilt, and the sweep that removes what no live project uses.
//!
//! What lives there:
//! - `<key>/`, one per source file, where `key` is
//!   [`asset_cache_key`](crate::adapters::video::asset_cache_key) (16 hex
//!   chars over path + size + mtime). It holds `peaks.bin`, `peaks_raw.bin`,
//!   `thumb_%04d.jpg`, `play.mp4` (`play.mp4.part` while it builds) and
//!   `source.json`, the source's path. The key needs the file's size and
//!   mtime, so while a source is offline (an unplugged drive) the path is
//!   the only way to tell which folder is its.
//! - `faces/<uuid>/`: frames for one face-crop pass, removed when the pass
//!   ends. A pass cut short (the app quit mid-pass) leaves its folder.
//! - `<uuid>/` (legacy): the first asset cache, keyed by media id, written
//!   for a few hours on 2026-06-10 until the file-identity key replaced it.
//!   Nothing reads these.

use crate::adapters::video::asset_cache_key;
use crate::model::{Media, Project};
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// The face-crop pass's scratch folder, under the cache root.
pub const FACES: &str = "faces";
/// Written into each `<key>/` folder: `{"file_path": "<source>"}`.
const SOURCE_FILE: &str = "source.json";
/// A folder with anything modified this recently is left alone: something
/// may be writing it (a new import's peaks, a preview copy being built).
pub const BUSY_WINDOW: Duration = Duration::from_secs(15 * 60);

pub fn root() -> PathBuf {
    crate::store::data_dir().join("cache")
}

/// The cache folder for `media` (not created).
pub fn dir_for(media: &Media) -> PathBuf {
    root().join(asset_cache_key(media))
}

/// The cache folder for `media`, created with its `source.json` if missing.
pub fn ensure_dir_for(media: &Media) -> std::io::Result<PathBuf> {
    ensure_dir_in(&root(), media)
}

fn ensure_dir_in(root: &Path, media: &Media) -> std::io::Result<PathBuf> {
    let dir = root.join(asset_cache_key(media));
    std::fs::create_dir_all(&dir)?;
    let marker = dir.join(SOURCE_FILE);
    if !marker.exists() {
        // Best effort: without it the folder is still matched by its key.
        let _ = std::fs::write(&marker, serde_json::json!({ "file_path": media.file_path }).to_string());
    }
    Ok(dir)
}

/// The source path a `<key>/` folder was made for, if it recorded one.
fn source_of(dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(dir.join(SOURCE_FILE)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v["file_path"].as_str().map(str::to_string)
}

/// The media that live projects use: their cache keys, and the paths of
/// sources that are offline right now (their key can't be computed).
#[derive(Debug, Default)]
pub struct InUse {
    keys: HashSet<String>,
    offline: HashSet<String>,
}

impl InUse {
    pub fn add_media(&mut self, media: &Media) {
        self.keys.insert(asset_cache_key(media));
        if std::fs::metadata(&media.file_path).is_err() {
            self.offline.insert(media.file_path.clone());
        }
    }

    pub fn add_project(&mut self, project: &Project) {
        for media in project.media.iter().chain(project.screen_media.iter()) {
            self.add_media(media);
        }
    }

    fn keeps(&self, key: &str, dir: &Path) -> bool {
        self.keys.contains(key) || source_of(dir).is_some_and(|p| self.offline.contains(&p))
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct SweepReport {
    /// Folders removed: unused `<key>/`, legacy `<uuid>/`, leftover face passes.
    pub removed: u32,
    /// Bytes freed, leftover `play.mp4.part` files in kept folders included.
    pub freed_bytes: u64,
    /// `<key>/` folders kept: in use, or touched within the busy window.
    pub kept: u32,
}

/// Remove what nothing uses from the cache at `root`. Anything modified
/// after `busy_after` counts as in use. Names the cache doesn't make (other
/// files and folders, symlinks) are left alone. Errors are skipped: a folder
/// that can't be removed now is tried again next sweep.
pub fn sweep(root: &Path, in_use: &InUse, busy_after: SystemTime) -> SweepReport {
    let mut report = SweepReport::default();
    let Ok(entries) = std::fs::read_dir(root) else {
        return report;
    };
    for entry in entries.flatten() {
        // DirEntry::file_type doesn't follow symlinks: a link is never ours.
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == FACES {
            for pass in std::fs::read_dir(&path).into_iter().flatten().flatten() {
                if pass.file_type().is_ok_and(|t| t.is_dir()) && !busy(&pass.path(), busy_after) {
                    remove(&pass.path(), &mut report);
                }
            }
        } else if is_asset_key(&name) {
            if busy(&path, busy_after) {
                report.kept += 1;
            } else if in_use.keeps(&name, &path) {
                report.kept += 1;
                // Not busy, so no build is writing it: a crash left it.
                let part = path.join("play.mp4.part");
                if let Ok(meta) = std::fs::metadata(&part) {
                    if std::fs::remove_file(&part).is_ok() {
                        report.freed_bytes += meta.len();
                    }
                }
            } else {
                remove(&path, &mut report);
            }
        } else if Uuid::parse_str(&name).is_ok() && !busy(&path, busy_after) {
            remove(&path, &mut report);
        }
    }
    report
}

/// What `asset_cache_key` produces: 16 lowercase hex chars.
fn is_asset_key(name: &str) -> bool {
    name.len() == 16 && name.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The folder, or anything directly in it, was modified after `after`.
/// A time that can't be read doesn't count; one in the future does.
fn busy(dir: &Path, after: SystemTime) -> bool {
    let recent = |meta: std::io::Result<std::fs::Metadata>| {
        meta.and_then(|m| m.modified()).is_ok_and(|t| t > after)
    };
    recent(std::fs::metadata(dir))
        || std::fs::read_dir(dir).into_iter().flatten().flatten().any(|e| recent(e.metadata()))
}

fn remove(dir: &Path, report: &mut SweepReport) {
    let bytes = size_of(dir);
    if std::fs::remove_dir_all(dir).is_ok() {
        report.removed += 1;
        report.freed_bytes += bytes;
    }
}

fn size_of(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path).into_iter().flatten().flatten().map(|e| size_of(&e.path())).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media(path: &Path) -> Media {
        Media {
            id: Uuid::new_v4(),
            file_path: path.to_string_lossy().into_owned(),
            duration: 60.0,
            frame_rate: 30.0,
            width: 1920,
            height: 1080,
            audio_sample_rate: 48000,
            codec: "h264".into(),
            imported_at: chrono::Utc::now(),
        }
    }

    fn write(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![0u8; bytes]).unwrap();
    }

    /// Everything on disk is older than this.
    fn later() -> SystemTime {
        SystemTime::now() + Duration::from_secs(3600)
    }

    #[test]
    fn removes_what_no_live_media_uses_and_keeps_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cache");
        let used = tmp.path().join("used.mp4");
        let gone = tmp.path().join("gone.mp4");
        write(&used, 10);
        write(&gone, 10);
        let used_dir = ensure_dir_in(&root, &media(&used)).unwrap();
        write(&used_dir.join("peaks.bin"), 100);
        let gone_dir = ensure_dir_in(&root, &media(&gone)).unwrap();
        write(&gone_dir.join("play.mp4"), 1000);
        // Marker-less folders from before `source.json`, and the legacy and
        // face-pass folders.
        write(&root.join("0123456789abcdef/peaks.bin"), 50);
        let legacy = root.join(Uuid::new_v4().to_string());
        write(&legacy.join("thumb_0001.jpg"), 20);
        let face_pass = root.join(FACES).join(Uuid::new_v4().to_string());
        write(&face_pass.join("frame_00.png"), 30);
        // Not the cache's: left alone.
        write(&root.join("notes/readme.txt"), 5);
        write(&root.join(".DS_Store"), 5);
        write(&root.join("ABCDEF0123456789/peaks.bin"), 5);

        let mut in_use = InUse::default();
        in_use.add_media(&media(&used));
        let report = sweep(&root, &in_use, later());

        assert!(used_dir.join("peaks.bin").is_file());
        assert!(!gone_dir.exists());
        assert!(!root.join("0123456789abcdef").exists());
        assert!(!legacy.exists());
        assert!(!face_pass.exists());
        assert!(root.join(FACES).is_dir(), "only the passes go, not faces/ itself");
        assert!(root.join("notes/readme.txt").is_file());
        assert!(root.join(".DS_Store").is_file());
        assert!(root.join("ABCDEF0123456789").is_dir());
        assert_eq!(report.removed, 4);
        assert_eq!(report.kept, 1);
        // Plus `gone`'s source.json.
        assert!(report.freed_bytes > 1000 + 50 + 20 + 30);
    }

    #[test]
    fn keeps_the_folder_of_a_live_source_that_is_offline() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cache");
        let src = tmp.path().join("on-the-external-drive.mp4");
        write(&src, 10);
        let dir = ensure_dir_in(&root, &media(&src)).unwrap();
        write(&dir.join("play.mp4"), 1000);
        // The drive is unplugged: the key no longer matches the folder.
        std::fs::remove_file(&src).unwrap();
        assert_ne!(dir, root.join(asset_cache_key(&media(&src))));

        let mut in_use = InUse::default();
        in_use.add_media(&media(&src));
        let report = sweep(&root, &in_use, later());
        assert!(dir.join("play.mp4").is_file());
        assert_eq!(report, SweepReport { removed: 0, freed_bytes: 0, kept: 1 });

        // No longer used by a live project: it goes.
        let report = sweep(&root, &InUse::default(), later());
        assert!(!dir.exists());
        assert_eq!(report.removed, 1);
    }

    #[test]
    fn a_changed_source_drops_its_old_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cache");
        let src = tmp.path().join("re-exported.mp4");
        write(&src, 10);
        let old = ensure_dir_in(&root, &media(&src)).unwrap();
        write(&src, 20); // new size, new key
        let new = ensure_dir_in(&root, &media(&src)).unwrap();
        assert_ne!(old, new);

        let mut in_use = InUse::default();
        in_use.add_media(&media(&src));
        sweep(&root, &in_use, later());
        assert!(!old.exists(), "the path still exists, so its old folder is stale");
        assert!(new.is_dir());
    }

    #[test]
    fn leaves_folders_touched_within_the_busy_window() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cache");
        write(&root.join("0123456789abcdef/play.mp4.part"), 100);
        let pass = root.join(FACES).join(Uuid::new_v4().to_string());
        write(&pass.join("frame_00.png"), 10);
        let report = sweep(&root, &InUse::default(), SystemTime::now() - BUSY_WINDOW);
        assert!(root.join("0123456789abcdef/play.mp4.part").is_file());
        assert!(pass.is_dir());
        assert_eq!(report, SweepReport { removed: 0, freed_bytes: 0, kept: 1 });
    }

    #[test]
    fn drops_a_leftover_part_file_from_a_kept_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cache");
        let src = tmp.path().join("a.mkv");
        write(&src, 10);
        let dir = ensure_dir_in(&root, &media(&src)).unwrap();
        write(&dir.join("peaks.bin"), 10);
        write(&dir.join("play.mp4.part"), 500);
        let mut in_use = InUse::default();
        in_use.add_media(&media(&src));
        let report = sweep(&root, &in_use, later());
        assert!(dir.join("peaks.bin").is_file());
        assert!(!dir.join("play.mp4.part").exists());
        assert_eq!(report, SweepReport { removed: 0, freed_bytes: 500, kept: 1 });
    }

    #[test]
    fn a_missing_cache_is_an_empty_sweep() {
        let tmp = tempfile::tempdir().unwrap();
        let report = sweep(&tmp.path().join("nope"), &InUse::default(), later());
        assert_eq!(report, SweepReport::default());
    }

    #[test]
    fn asset_keys_are_sixteen_lowercase_hex() {
        assert!(is_asset_key("0123456789abcdef"));
        assert!(!is_asset_key("0123456789ABCDEF"));
        assert!(!is_asset_key("0123456789abcde"));
        assert!(!is_asset_key("faces"));
        assert!(!is_asset_key(&Uuid::new_v4().to_string()));
    }
}
