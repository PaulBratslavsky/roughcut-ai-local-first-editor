//! Checks OBS-style imports and the footage join against real ffmpeg.
//!
//! Makes 10-second test clips and checks that `ensure_playable_copy` turns an
//! OBS-style `.mkv` (H.264 + AAC, H.264 + Opus, HEVC + AAC, two audio tracks)
//! and an odd-sized VP9 `.webm` into a faststart MP4 with stereo AAC audio
//! (HEVC tagged `hvc1`), and that an MP4 gets no copy, its index at the end
//! or not, unless it is HEVC tagged `hev1`. Then joins
//! files with `concat_media`: MP4s into an MP4; `.mkv` files into an `.mkv`
//! whose preview copy plays; two-track OBS files twice in a row (as two
//! "add footage" steps); and refuses files that don't match. Runs with a
//! throwaway HOME, so the real data dir is untouched.
//!
//! Run: `cargo run -p roughcut-core --example playable_check`

use roughcut_core::adapters::video::{concat_media, ensure_playable_copy};
use roughcut_core::adapters::{FfmpegCli, VideoEngine};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

fn ffmpeg_bin() -> String {
    std::env::var("FFMPEG_PATH").unwrap_or_else(|_| "ffmpeg".into())
}

fn ffprobe_bin() -> String {
    std::env::var("FFPROBE_PATH").unwrap_or_else(|_| "ffprobe".into())
}

const H264: &[&str] = &["-c:v", "libx264", "-pix_fmt", "yuv420p"];

/// A 10-second test clip: moving test pattern plus a tone (and, with
/// `second_track`, a 6-channel second audio track, like an OBS file
/// recording track 2).
fn make_clip(out: &Path, size: &str, video: &[&str], audio: &[&str], second_track: bool) {
    let mut cmd = Command::new(ffmpeg_bin());
    cmd.args(["-y", "-v", "error"])
        .args(["-f", "lavfi", "-i", &format!("testsrc2=size={size}:rate=30:duration=10")])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=10"]);
    if second_track {
        cmd.args(["-f", "lavfi", "-i", "sine=frequency=660:sample_rate=48000:duration=10"])
            .args(["-map", "0:v", "-map", "1:a", "-map", "2:a", "-ac:a:1", "6"]);
    }
    // Track 1 stereo, as OBS writes it.
    cmd.args(video).args(audio).args(["-ac:a:0", "2"]).arg(out);
    let res = cmd.output().expect("run ffmpeg");
    assert!(res.status.success(), "making {out:?}: {}", String::from_utf8_lossy(&res.stderr));
}

fn probe(path: &str, entries: &str, select: &str) -> String {
    let out = Command::new(ffprobe_bin())
        .args(["-v", "error", "-select_streams", select, "-show_entries", entries, "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn duration(path: &str) -> f64 {
    let out = Command::new(ffprobe_bin())
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", path])
        .output()
        .expect("run ffprobe");
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0.0)
}

/// True when the MP4's `moov` box comes before `mdat` (plays while loading).
fn moov_first(path: &str) -> bool {
    let bytes = std::fs::read(path).expect("read copy");
    let find = |tag: &[u8]| bytes.windows(4).position(|w| w == tag);
    matches!((find(b"moov"), find(b"mdat")), (Some(m), Some(d)) if m < d)
}

fn name(p: &Path) -> String {
    p.file_name().unwrap().to_string_lossy().into_owned()
}

async fn check_copy(engine: &FfmpegCli, src: &Path, want_video: &str, want_tag: Option<&str>) -> String {
    let media = engine.probe(&src.to_string_lossy()).await.expect("probe");
    let t = Instant::now();
    let copy = ensure_playable_copy(&media)
        .await
        .expect("playable copy")
        .unwrap_or_else(|| panic!("{src:?}: expected a playable copy"));
    let took = t.elapsed();
    assert!(copy.ends_with("play.mp4"), "{copy}");
    assert_eq!(probe(&copy, "stream=codec_name", "v:0"), want_video, "{src:?} video");
    if let Some(tag) = want_tag {
        assert_eq!(probe(&copy, "stream=codec_tag_string", "v:0"), tag, "{src:?} tag");
    }
    let (w, h) = probe(&copy, "stream=width,height", "v:0").split_once(',').map(|(w, h)| {
        (w.parse::<u32>().unwrap(), h.parse::<u32>().unwrap())
    }).expect("size");
    assert!(w % 2 == 0 && h % 2 == 0, "{src:?}: odd copy size {w}x{h}");
    // Exactly one audio track, stereo AAC: the source's first track.
    assert_eq!(probe(&copy, "stream=codec_name,channels", "a"), "aac,2", "{src:?} audio");
    assert!(moov_first(&copy), "{src:?}: copy is not faststart");
    let (a, b) = (duration(&src.to_string_lossy()), duration(&copy));
    assert!((a - b).abs() < 0.15, "{src:?}: length {a} vs copy {b}");
    // A second call finds the cached copy.
    let again = ensure_playable_copy(&media).await.expect("cached").expect("cached copy");
    assert_eq!(again, copy);
    println!("  {:<26} → {want_video} {w}x{h} + aac stereo, faststart, {:.2}s", name(src), took.as_secs_f64());
    copy
}

async fn join(paths: &[&Path]) -> String {
    let paths: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    concat_media(&paths).await.expect("join")
}

#[tokio::main]
async fn main() {
    let home: PathBuf = std::env::temp_dir().join(format!("roughcut-playable-{}", std::process::id()));
    let clips = home.join("clips");
    std::fs::create_dir_all(&clips).unwrap();
    // The data dir lives under HOME; point it at the throwaway folder before
    // anything reads it.
    std::env::set_var("HOME", &home);
    let data_dir = roughcut_core::store::data_dir();
    assert!(data_dir.starts_with(&home), "data dir {data_dir:?} is not under the throwaway HOME");
    let engine = FfmpegCli;

    println!("playable copies:");
    let mkv_aac = clips.join("obs-aac.mkv");
    make_clip(&mkv_aac, "640x360", H264, &["-c:a", "aac"], false);
    check_copy(&engine, &mkv_aac, "h264", None).await;

    let mkv_opus = clips.join("obs-opus.mkv");
    make_clip(&mkv_opus, "640x360", H264, &["-c:a", "libopus"], false);
    check_copy(&engine, &mkv_opus, "h264", None).await;

    let hevc = &["-c:v", "libx265", "-pix_fmt", "yuv420p", "-x265-params", "log-level=error"];
    let mkv_hevc = clips.join("obs-hevc.mkv");
    make_clip(&mkv_hevc, "640x360", hevc, &["-c:a", "aac"], false);
    check_copy(&engine, &mkv_hevc, "hevc", Some("hvc1")).await;

    // OBS track 1 (stereo) + track 2 (6 channels): ffmpeg's own pick would
    // be the 6-channel track; the copy must take track 1.
    let mkv_tracks = clips.join("obs-two-tracks.mkv");
    make_clip(&mkv_tracks, "640x360", H264, &["-c:a", "aac"], true);
    assert_eq!(probe(&mkv_tracks.to_string_lossy(), "stream=channels", "a"), "2\n6");
    check_copy(&engine, &mkv_tracks, "h264", None).await;

    // Odd dimensions: the H.264 re-encode must round them to even.
    let webm = clips.join("browser-odd.webm");
    make_clip(&webm, "641x361", &["-c:v", "libvpx-vp9", "-deadline", "realtime", "-cpu-used", "8"], &["-c:a", "libopus"], false);
    check_copy(&engine, &webm, "h264", None).await;

    let mp4 = clips.join("plain.mp4");
    make_clip(&mp4, "640x360", &[H264, &["-movflags", "+faststart"]].concat(), &["-c:a", "aac"], false);
    let media = engine.probe(&mp4.to_string_lossy()).await.expect("probe mp4");
    assert_eq!(ensure_playable_copy(&media).await.expect("mp4"), None, "a faststart MP4 needs no copy");
    println!("  {:<26} → plays as-is (no copy)", "plain.mp4");

    // OBS writes its MP4 index at the end: the preview reads it as it is.
    let late = clips.join("obs-late-index.mp4");
    make_clip(&late, "640x360", H264, &["-c:a", "aac"], false);
    assert!(!moov_first(&late.to_string_lossy()), "the test clip should have its index at the end");
    let media = engine.probe(&late.to_string_lossy()).await.expect("probe late-index mp4");
    assert_eq!(ensure_playable_copy(&media).await.expect("late mp4"), None, "a late index needs no copy");
    println!("  {:<26} → plays as-is (no copy)", name(&late));

    // HEVC in MP4: ffmpeg tags it hev1, which Apple players refuse, so it
    // gets a copy tagged hvc1; one already tagged hvc1 plays as it is.
    let hev1 = clips.join("hevc-hev1.mp4");
    make_clip(&hev1, "640x360", hevc, &["-c:a", "aac"], false);
    assert_eq!(probe(&hev1.to_string_lossy(), "stream=codec_tag_string", "v:0"), "hev1");
    check_copy(&engine, &hev1, "hevc", Some("hvc1")).await;
    let hvc1 = clips.join("hevc-hvc1.mp4");
    make_clip(&hvc1, "640x360", &[&hevc[..], &["-tag:v", "hvc1"]].concat(), &["-c:a", "aac"], false);
    let media = engine.probe(&hvc1.to_string_lossy()).await.expect("probe hvc1 mp4");
    assert_eq!(ensure_playable_copy(&media).await.expect("hvc1 mp4"), None, "hvc1 HEVC needs no copy");
    println!("  {:<26} → plays as-is (no copy)", name(&hvc1));

    println!("join:");
    let mp4_2 = clips.join("plain-2.mp4");
    make_clip(&mp4_2, "640x360", &[H264, &["-movflags", "+faststart"]].concat(), &["-c:a", "aac"], false);
    let joined = join(&[&mp4, &mp4_2]).await;
    assert!(Path::new(&joined).starts_with(data_dir.join("media")), "joined into {joined}");
    assert!(joined.ends_with(".mp4") && moov_first(&joined), "{joined}");
    let len = duration(&joined);
    assert!((len - 20.0).abs() < 0.2, "joined length {len}");
    println!("  plain.mp4 + plain-2.mp4      → {len:.1}s .mp4 in <data dir>/media/");

    // HEVC .mkv files join into an .mkv, so the preview copy still adds hvc1.
    let joined = join(&[&mkv_hevc, &mkv_hevc]).await;
    assert!(joined.ends_with(".mkv"), "{joined}");
    let joined_media = engine.probe(&joined).await.expect("probe joined");
    let copy = ensure_playable_copy(&joined_media).await.expect("copy").expect("joined mkv needs a copy");
    assert_eq!(probe(&copy, "stream=codec_tag_string", "v:0"), "hvc1");
    println!("  obs-hevc.mkv × 2             → {:.1}s .mkv; its copy is tagged hvc1", duration(&joined));

    // Two "add footage" steps with two-track OBS files: the second compares
    // the first join's result (track 1 only) with a new two-track file.
    let first = join(&[&mkv_tracks, &mkv_tracks]).await;
    assert_eq!(probe(&first, "stream=channels", "a"), "2", "join keeps track 1 only");
    let second = join(&[Path::new(&first), &mkv_tracks]).await;
    let len = duration(&second);
    assert!((len - 30.0).abs() < 0.3, "second join length {len}");
    println!("  two-track .mkv, joined twice → {len:.1}s");

    // Mismatched files are refused, not joined into a broken file.
    let err = concat_media(&[mp4.to_string_lossy().into_owned(), mkv_hevc.to_string_lossy().into_owned()])
        .await
        .expect_err("h264 + hevc must not join");
    assert!(err.to_string().contains("don't match"), "{err}");
    println!("  plain.mp4 + obs-hevc.mkv     → refused ({err})");

    let _ = std::fs::remove_dir_all(&home);
    println!("PLAYABLE-OK");
}
