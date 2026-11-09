//! Checks the per-piece seeking MP4 render against real ffmpeg.
//!
//! Generates a 45-minute test video (testsrc + a tone), then renders:
//! a short from minute 40, two pieces in reverse order, an audio offset
//! either way, and a 9:16 crop. For each it checks the output length and that
//! the right source frames land at the right output times (PSNR against
//! frames grabbed from the source). It also times the minute-40 short against
//! the old way (one un-seeked input), to show what seeking saves.
//!
//! Run: `cargo run -p roughcut-core --example render_seek_check`

use roughcut_core::adapters::video::RenderJob;
use roughcut_core::adapters::{FfmpegCli, VideoEngine};
use roughcut_core::events::{NullSink, SharedSink};
use roughcut_core::model::PlaySpan;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

fn span(a: f64, b: f64) -> PlaySpan {
    PlaySpan { source_in: a, source_out: b }
}

fn ffmpeg(args: &[&str]) -> std::process::Output {
    Command::new("ffmpeg").args(args).output().expect("run ffmpeg")
}

fn duration(path: &Path) -> f64 {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0.0)
}

fn size(path: &Path) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// PSNR (dB) between the frame of `a` at `ta` and the frame of `b` at `tb`.
fn psnr(dir: &Path, a: &Path, ta: f64, b: &Path, tb: f64) -> f64 {
    psnr_vf(dir, a, ta, b, tb, "scale=320:240")
}

/// The same over a filtered region (e.g. a crop away from a camera bubble).
fn psnr_vf(dir: &Path, a: &Path, ta: f64, b: &Path, tb: f64, vf: &str) -> f64 {
    let grab = |file: &Path, t: f64, name: &str| {
        let png = dir.join(name);
        let out = ffmpeg(&[
            "-y", "-v", "error", "-ss", &format!("{t:.3}"), "-i", &file.to_string_lossy(),
            "-frames:v", "1", "-vf", vf, &png.to_string_lossy(),
        ]);
        assert!(out.status.success(), "grab failed: {}", String::from_utf8_lossy(&out.stderr));
        png
    };
    let (pa, pb) = (grab(a, ta, "a.png"), grab(b, tb, "b.png"));
    let out = ffmpeg(&["-i", &pa.to_string_lossy(), "-i", &pb.to_string_lossy(), "-lavfi", "psnr", "-f", "null", "-"]);
    let err = String::from_utf8_lossy(&out.stderr);
    let avg = err.split("average:").nth(1).and_then(|r| r.split_whitespace().next()).unwrap_or("0");
    if avg == "inf" { 99.0 } else { avg.parse().unwrap_or(0.0) }
}

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join("roughcut-render-seek-check");
    std::fs::create_dir_all(&dir).unwrap();
    let src: PathBuf = dir.join("source-45min.mp4");
    if duration(&src) < 2699.0 {
        println!("generating a 45-minute test source (once)…");
        let out = ffmpeg(&[
            "-y", "-v", "error", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=25:duration=2700",
            "-f", "lavfi", "-i", "sine=frequency=440:duration=2700",
            "-c:v", "libx264", "-preset", "ultrafast", "-g", "50", "-c:a", "aac", &src.to_string_lossy(),
        ]);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    let engine = FfmpegCli;
    let media = engine.probe(&src.to_string_lossy()).await.expect("probe");
    let sink: SharedSink = Arc::new(NullSink);
    let mut failed = 0;
    let mut check = |ok: bool, what: &str| {
        println!("{} {what}", if ok { "PASS" } else { "FAIL" });
        if !ok {
            failed += 1;
        }
    };

    // 1. A short from minute 40 with a cut inside it.
    let late = vec![vec![span(2400.0, 2410.0), span(2412.0, 2430.0)]];
    let out1 = dir.join("late.mp4");
    let t = Instant::now();
    let job = RenderJob {
        media: &media, pieces: &late, out_path: &out1.to_string_lossy(), audio_offset_s: 0.0,
        screen: None, vertical_crop: None,
    };
    engine.render_mp4(&job, &sink).await.expect("render late");
    let seeked = t.elapsed().as_secs_f64();
    check((duration(&out1) - 28.0).abs() < 0.15, &format!("minute-40 short is 28 s (got {:.2})", duration(&out1)));
    let p = psnr(&dir, &src, 2400.5, &out1, 0.5);
    check(p > 30.0, &format!("output 0:00.5 is source 40:00.5 (PSNR {p:.1})"));
    let p = psnr(&dir, &src, 2412.5, &out1, 10.5);
    check(p > 30.0, &format!("output 0:10.5 is source 40:12.5, after the cut (PSNR {p:.1})"));
    let wrong = psnr(&dir, &src, 2405.5, &out1, 10.5);
    check(wrong < p - 5.0, &format!("the frame check tells frames apart (wrong frame PSNR {wrong:.1})"));

    // The old way: one input from 0:00, trimmed.
    let old = dir.join("late-old.mp4");
    let t = Instant::now();
    let out = ffmpeg(&[
        "-y", "-v", "error", "-i", &src.to_string_lossy(), "-filter_complex",
        "[0:v]trim=start=2400:end=2410,setpts=PTS-STARTPTS[v0];[0:a]atrim=start=2400:end=2410,asetpts=PTS-STARTPTS[a0];\
         [0:v]trim=start=2412:end=2430,setpts=PTS-STARTPTS[v1];[0:a]atrim=start=2412:end=2430,asetpts=PTS-STARTPTS[a1];\
         [v0][a0][v1][a1]concat=n=2:v=1:a=1[v][a]",
        "-map", "[v]", "-map", "[a]", "-c:v", "libx264", "-preset", "fast", "-crf", "18", "-c:a", "aac",
        &old.to_string_lossy(),
    ]);
    assert!(out.status.success());
    let unseeked = t.elapsed().as_secs_f64();
    println!("INFO minute-40 short: {seeked:.2} s seeked vs {unseeked:.2} s from 0:00");
    check(seeked < unseeked, "seeking is faster than decoding from 0:00");

    // 2. Two pieces in reverse order: the payoff (1:45) before the setup (0:30).
    let reversed = vec![vec![span(105.0, 125.0)], vec![span(30.0, 50.0)]];
    let out2 = dir.join("reversed.mp4");
    let job = RenderJob {
        media: &media, pieces: &reversed, out_path: &out2.to_string_lossy(), audio_offset_s: 0.0,
        screen: None, vertical_crop: None,
    };
    engine.render_mp4(&job, &sink).await.expect("render reversed");
    check((duration(&out2) - 40.0).abs() < 0.15, &format!("reversed pieces are 40 s (got {:.2})", duration(&out2)));
    let p = psnr(&dir, &src, 105.5, &out2, 0.5);
    check(p > 30.0, &format!("output 0:00.5 is source 1:45.5 (PSNR {p:.1})"));
    let p = psnr(&dir, &src, 30.5, &out2, 20.5);
    check(p > 30.0, &format!("output 0:20.5 is source 0:30.5 (PSNR {p:.1})"));

    // 3. An A/V offset either way, and 4. a 9:16 crop.
    for (offset, crop, name) in [(0.4, None, "delay"), (-0.4, None, "advance"), (0.0, Some(0.5), "vertical")] {
        let out = dir.join(format!("{name}.mp4"));
        let job = RenderJob {
            media: &media, pieces: &reversed, out_path: &out.to_string_lossy(), audio_offset_s: offset,
            screen: None, vertical_crop: crop.map(|x| roughcut_core::model::ClipFraming { crop_x: x, ..roughcut_core::model::ClipFraming::CENTRED }),
        };
        let r = engine.render_mp4(&job, &sink).await;
        check(r.is_ok(), &format!("{name} renders ({:?})", r.err()));
        check((duration(&out) - 40.0).abs() < 0.15, &format!("{name} is 40 s (got {:.2})", duration(&out)));
        if crop.is_some() {
            check(size(&out) == "1080,1920", &format!("vertical is 1080x1920 (got {})", size(&out)));
        }
        if offset > 0.0 {
            // The preroll covers the delay: the track starts on the tone, not
            // on the silence `adelay` pads in front.
            let o = ffmpeg(&["-i", &out.to_string_lossy(), "-af", "silencedetect=n=-40dB:d=0.1", "-f", "null", "-"]);
            let log = String::from_utf8_lossy(&o.stderr);
            check(!log.contains("silence_start"), "a delayed track starts on real audio, no silence");
        }
    }

    // 5. Screen + camera bubble, one screen input per piece.
    let screen_src = dir.join("screen-3min.mp4");
    if duration(&screen_src) < 179.0 {
        let out = ffmpeg(&[
            "-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=25:duration=180",
            "-c:v", "libx264", "-preset", "ultrafast", &screen_src.to_string_lossy(),
        ]);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    let screen = engine.probe(&screen_src.to_string_lossy()).await.expect("probe screen");
    for mode in ["pip", "screen"] {
        let layout = roughcut_core::model::Layout { mode: mode.into(), ..Default::default() };
        let out = dir.join(format!("composite-{mode}.mp4"));
        let job = RenderJob {
            media: &media, pieces: &reversed, out_path: &out.to_string_lossy(), audio_offset_s: 0.0,
            screen: Some((&screen, &layout)), vertical_crop: None,
        };
        let r = engine.render_mp4(&job, &sink).await;
        check(r.is_ok(), &format!("composite {mode} renders ({:?})", r.err()));
        check((duration(&out) - 40.0).abs() < 0.15, &format!("composite {mode} is 40 s (got {:.2})", duration(&out)));
        // The top-left quarter: clear of the bubble in the bottom-right.
        let vf = "crop=iw/2:ih/2:0:0,scale=320:180";
        let p = psnr_vf(&dir, &screen_src, 105.5, &out, 0.5, vf);
        check(p > 30.0, &format!("composite {mode}: output 0:00.5 shows screen 1:45.5 (PSNR {p:.1})"));
        let p = psnr_vf(&dir, &screen_src, 30.5, &out, 20.5, vf);
        check(p > 30.0, &format!("composite {mode}: output 0:20.5 shows screen 0:30.5 (PSNR {p:.1})"));
        let wrong = psnr_vf(&dir, &screen_src, 60.5, &out, 0.5, vf);
        check(wrong < 25.0, &format!("composite {mode}: a wrong frame scores low (PSNR {wrong:.1})"));
    }

    // 6. Frame-exact edges at 30 fps: every frame is a different flat grey,
    // so landing one frame off is unmistakable. The piece starts on frame 2
    // of a second, where rounding the seek used to drop the first frame.
    let src30 = dir.join("greys-30fps.mp4");
    if duration(&src30) < 179.0 {
        let out = ffmpeg(&[
            "-y", "-v", "error", "-f", "lavfi",
            "-i", "color=c=black:s=320x240:r=30:d=180,format=yuv420p,geq=lum='mod(N*37,256)':cb=128:cr=128",
            "-f", "lavfi", "-i", "sine=frequency=440:duration=180",
            "-c:v", "libx264", "-preset", "ultrafast", "-g", "60", "-c:a", "aac", &src30.to_string_lossy(),
        ]);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    let media30 = engine.probe(&src30.to_string_lossy()).await.expect("probe 30fps");
    let f = 1.0 / 30.0;
    let start = 100.0 + 2.0 * f;
    let exact = vec![vec![span(start, start + 10.0)], vec![span(120.0 + 7.0 * f, 125.0 + 7.0 * f)]];
    let out6 = dir.join("frame-exact.mp4");
    let job = RenderJob {
        media: &media30, pieces: &exact, out_path: &out6.to_string_lossy(), audio_offset_s: 0.0,
        screen: None, vertical_crop: None,
    };
    engine.render_mp4(&job, &sink).await.expect("render frame-exact");
    // Grab a little before each frame's own timestamp: accurate seek takes
    // the first frame at or after the time given, which is then that frame.
    let at = |t: f64| (t - 0.3 * f).max(0.0);
    let first = psnr(&dir, &src30, at(start), &out6, at(0.0));
    let next = psnr(&dir, &src30, at(start + f), &out6, at(0.0));
    check(first > 35.0 && first > next + 10.0, &format!("first frame is the piece's first frame (PSNR {first:.1}, next frame {next:.1})"));
    let last = psnr(&dir, &src30, at(start + 10.0 - f), &out6, at(10.0 - f));
    check(last > 35.0, &format!("last frame of piece 1 is its last frame (PSNR {last:.1})"));
    let second = psnr(&dir, &src30, at(120.0 + 7.0 * f), &out6, at(10.0));
    check(second > 35.0, &format!("piece 2 starts on its first frame (PSNR {second:.1})"));
    check((duration(&out6) - 15.0).abs() < 0.1, &format!("frame-exact render is 15 s (got {:.3})", duration(&out6)));

    // 7. A short at the very end of the screen recording: the screen input
    // can't seek past its end, and its last frame holds.
    let layout = roughcut_core::model::Layout { mode: "screen".into(), ..Default::default() };
    let tail = vec![vec![span(179.5, 185.0)]];
    let out7 = dir.join("screen-tail.mp4");
    let job = RenderJob {
        media: &media, pieces: &tail, out_path: &out7.to_string_lossy(), audio_offset_s: 0.0,
        screen: Some((&screen, &layout)), vertical_crop: None,
    };
    let r = engine.render_mp4(&job, &sink).await;
    check(r.is_ok(), &format!("a short past the screen's end renders ({:?})", r.err()));
    check((duration(&out7) - 5.5).abs() < 0.15, &format!("it is 5.5 s (got {:.2})", duration(&out7)));
    let vf = "crop=iw/2:ih/2:0:0,scale=320:180";
    // Output 0:00.2 is screen 2:59.7, mid-frame at 25 fps: the frame on show
    // then is the one at 2:59.68. Grab exact frames on both sides.
    let g = 0.3 / 25.0;
    let p = psnr_vf(&dir, &screen_src, 179.68 - g, &out7, 0.2 - g, vf);
    check(p > 30.0, &format!("output 0:00.2 shows screen 2:59.7 (PSNR {p:.1})"));
    let p = psnr_vf(&dir, &screen_src, 179.95, &out7, 4.0, vf);
    check(p > 30.0, &format!("past the screen's end its last frame holds (PSNR {p:.1})"));

    if failed == 0 {
        println!("ALL PASS");
    } else {
        println!("{failed} FAILED");
        std::process::exit(1);
    }
}
