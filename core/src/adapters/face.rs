//! Face finding for 9:16 reframing. Apple's Vision framework does the
//! detection on-device: nothing leaves the machine and there's no model to
//! ship. Off macOS detection is unavailable and a clip keeps its centred (or
//! hand-set) crop.
//!
//! The rest is plain math: which source moments to sample, and where a
//! full-height 9:16 window should sit so the speaker is in it.

use crate::error::{CoreError, Result};
use std::path::Path;

/// One face in a frame, normalised to that frame (0..1, origin top-left).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FaceBox {
    pub cx: f64,
    pub cy: f64,
    pub w: f64,
    pub h: f64,
    pub confidence: f32,
}

/// Faces below this confidence, or shorter than this share of the frame,
/// don't count (posters, reflections, a crowd far behind the speaker).
const MIN_CONFIDENCE: f32 = 0.6;
const MIN_FACE_H: f64 = 0.04;

/// Faces in a still image on disk (PNG/JPEG). Blocking: call it from
/// `spawn_blocking`.
pub fn detect_faces(image: &Path) -> Result<Vec<FaceBox>> {
    imp::detect_faces(image)
}

#[cfg(target_os = "macos")]
mod imp {
    use super::FaceBox;
    use crate::error::{CoreError, Result};
    use objc2::rc::{autoreleasepool, Retained};
    use objc2::runtime::AnyObject;
    use objc2::AnyThread;
    use objc2_foundation::{NSArray, NSDictionary, NSString, NSURL};
    use objc2_vision::{VNDetectFaceRectanglesRequest, VNImageRequestHandler, VNRequest};

    pub fn detect_faces(image: &std::path::Path) -> Result<Vec<FaceBox>> {
        let path = image
            .to_str()
            .ok_or_else(|| CoreError::InvalidArg(format!("frame path is not UTF-8: {}", image.display())))?;
        autoreleasepool(|_| {
            let url = NSURL::fileURLWithPath(&NSString::from_str(path));
            let options: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::new();
            // SAFETY: an empty options dictionary is valid for initWithURL:options:.
            let handler = unsafe {
                VNImageRequestHandler::initWithURL_options(VNImageRequestHandler::alloc(), &url, &options)
            };
            // SAFETY: plain init of a request object.
            let request = unsafe { VNDetectFaceRectanglesRequest::new() };
            let as_request: Retained<VNRequest> =
                Retained::into_super(Retained::into_super(request.clone()));
            handler
                .performRequests_error(&NSArray::from_retained_slice(&[as_request]))
                .map_err(|e| CoreError::Other(format!("face detection failed: {}", e.localizedDescription())))?;
            let mut faces = vec![];
            // SAFETY: results are read after the handler performed the request.
            if let Some(results) = unsafe { request.results() } {
                for obs in results.iter() {
                    // Vision's box is normalised with the origin bottom-left.
                    let (bb, confidence) = unsafe { (obs.boundingBox(), obs.confidence()) };
                    faces.push(FaceBox {
                        cx: bb.origin.x + bb.size.width / 2.0,
                        cy: 1.0 - (bb.origin.y + bb.size.height / 2.0),
                        w: bb.size.width,
                        h: bb.size.height,
                        confidence,
                    });
                }
            }
            Ok(faces)
        })
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::FaceBox;
    use crate::error::{CoreError, Result};

    pub fn detect_faces(_image: &std::path::Path) -> Result<Vec<FaceBox>> {
        Err(CoreError::Unavailable("face detection needs macOS (Apple Vision)".into()))
    }
}

/// The speaker in one frame: the biggest face that clears the confidence and
/// size bars.
pub fn subject(faces: &[FaceBox]) -> Option<&FaceBox> {
    faces
        .iter()
        .filter(|f| f.confidence >= MIN_CONFIDENCE && f.h >= MIN_FACE_H)
        .max_by(|a, b| (a.w * a.h).total_cmp(&(b.w * b.h)))
}

/// Where a full-height 9:16 window sits across a `frame_w`×`frame_h` frame
/// (0 = left edge, 1 = right edge: the render's `crop_x`) so it's centred on
/// the speaker. Takes the median of each frame's subject, so a look-away or
/// a stray background face can't yank the crop. None when under a quarter of
/// the frames show a face: too little to trust.
pub fn crop_x_for_faces(frames: &[Vec<FaceBox>], frame_w: u32, frame_h: u32) -> Option<f64> {
    let mut centres: Vec<f64> = frames.iter().filter_map(|f| subject(f)).map(|f| f.cx).collect();
    if centres.is_empty() || centres.len() * 4 < frames.len() {
        return None;
    }
    centres.sort_by(f64::total_cmp);
    let mid = centres.len() / 2;
    let median = if centres.len() % 2 == 0 { (centres[mid - 1] + centres[mid]) / 2.0 } else { centres[mid] };
    let window = (frame_h as f64 * 9.0 / 16.0) / frame_w.max(1) as f64;
    if window >= 1.0 {
        // Already 9:16 or narrower: the crop spans the whole width.
        return Some(0.5);
    }
    Some(((median - window / 2.0) / (1.0 - window)).clamp(0.0, 1.0))
}

/// Source times to look at: evenly spaced through the kept footage
/// (`(source_in, source_out)` spans in play order), one per ~2 s of playable
/// time, at least 6 and at most `max`.
pub fn sample_times(kept: &[(f64, f64)], max: usize) -> Vec<f64> {
    let total: f64 = kept.iter().map(|(a, b)| (b - a).max(0.0)).sum();
    if total <= 0.0 || max == 0 {
        return vec![];
    }
    let n = ((total / 2.0).round() as usize).clamp(6.min(max), max);
    (0..n)
        .filter_map(|i| {
            let mut at = (i as f64 + 0.5) * total / n as f64;
            for &(a, b) in kept {
                let len = (b - a).max(0.0);
                if at < len {
                    return Some(a + at);
                }
                at -= len;
            }
            None
        })
        .collect()
}

/// A PNG's pixel size, from its IHDR chunk (ffmpeg writes the autorotated
/// frame, so this is the frame the render crops).
pub fn png_size(path: &Path) -> Result<(u32, u32)> {
    let bytes = std::fs::read(path)?;
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return Err(CoreError::Other(format!("not a PNG frame: {}", path.display())));
    }
    let be = |i: usize| u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    Ok((be(16), be(20)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(cx: f64, size: f64) -> FaceBox {
        FaceBox { cx, cy: 0.4, w: size, h: size, confidence: 0.99 }
    }

    #[test]
    fn centred_face_gives_centred_crop() {
        let frames = vec![vec![face(0.5, 0.2)]; 6];
        let x = crop_x_for_faces(&frames, 1920, 1080).unwrap();
        assert!((x - 0.5).abs() < 1e-9, "{x}");
    }

    #[test]
    fn crop_window_is_centred_on_the_face() {
        // 16:9: the 9:16 window is 0.3164 of the width. A face at 0.7 should
        // put the window's centre at 0.7.
        let frames = vec![vec![face(0.7, 0.2)]; 8];
        let x = crop_x_for_faces(&frames, 1920, 1080).unwrap();
        let window = (1080.0 * 9.0 / 16.0) / 1920.0;
        let centre = x * (1.0 - window) + window / 2.0;
        assert!((centre - 0.7).abs() < 1e-9, "centre {centre}");
    }

    #[test]
    fn faces_at_the_edge_clamp() {
        let left = crop_x_for_faces(&vec![vec![face(0.02, 0.1)]; 6], 1920, 1080).unwrap();
        let right = crop_x_for_faces(&vec![vec![face(0.98, 0.1)]; 6], 1920, 1080).unwrap();
        assert_eq!(left, 0.0);
        assert_eq!(right, 1.0);
    }

    #[test]
    fn the_biggest_face_is_the_speaker_and_outliers_dont_move_it() {
        let mut frames = vec![vec![face(0.3, 0.25), face(0.8, 0.05)]; 7];
        // One look-away frame where only a background face shows.
        frames.push(vec![face(0.9, 0.06)]);
        let x = crop_x_for_faces(&frames, 1920, 1080).unwrap();
        let window = (1080.0 * 9.0 / 16.0) / 1920.0;
        assert!((x * (1.0 - window) + window / 2.0 - 0.3).abs() < 1e-9);
    }

    #[test]
    fn too_few_faces_is_no_answer() {
        let mut frames = vec![vec![]; 9];
        frames.push(vec![face(0.6, 0.2)]);
        assert_eq!(crop_x_for_faces(&frames, 1920, 1080), None);
        assert_eq!(crop_x_for_faces(&[], 1920, 1080), None);
    }

    #[test]
    fn low_confidence_and_tiny_faces_dont_count() {
        let weak = FaceBox { confidence: 0.3, ..face(0.6, 0.2) };
        let tiny = face(0.6, 0.01);
        assert!(subject(&[weak, tiny]).is_none());
    }

    #[test]
    fn vertical_source_needs_no_pan() {
        let frames = vec![vec![face(0.2, 0.2)]; 6];
        assert_eq!(crop_x_for_faces(&frames, 1080, 1920), Some(0.5));
    }

    #[test]
    fn samples_spread_over_kept_spans_only() {
        // Two kept spans: 10-20s and 50-60s (20s playable) → 10 samples, all
        // inside the spans, in order.
        let t = sample_times(&[(10.0, 20.0), (50.0, 60.0)], 12);
        assert_eq!(t.len(), 10);
        assert!(t.iter().all(|&s| (10.0..20.0).contains(&s) || (50.0..60.0).contains(&s)), "{t:?}");
        assert!(t.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(t.iter().filter(|&&s| s < 30.0).count(), 5);
    }

    #[test]
    fn sample_count_is_bounded() {
        assert_eq!(sample_times(&[(0.0, 3.0)], 12).len(), 6);
        assert_eq!(sample_times(&[(0.0, 600.0)], 12).len(), 12);
        assert!(sample_times(&[], 12).is_empty());
    }

    #[test]
    fn reads_png_size() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.png");
        let mut png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&360u32.to_be_bytes());
        std::fs::write(&p, png).unwrap();
        assert_eq!(png_size(&p).unwrap(), (640, 360));
        std::fs::write(&p, b"nope").unwrap();
        assert!(png_size(&p).is_err());
    }
}
