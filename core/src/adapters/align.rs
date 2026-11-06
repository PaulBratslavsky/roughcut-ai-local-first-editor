//! Forced alignment: put each transcript word where it is actually spoken.
//!
//! Whisper writes the right words but only estimates their times (a stream's
//! first words landed inside its silent first 13 s; "but" sat half a second
//! early, in silence). A wav2vec2 model reads the audio every 20 ms and says
//! which letter is being spoken; a Viterbi pass lines the known words up with
//! that, word by word. Runs on this Mac (model: `facebook/wav2vec2-base-960h`,
//! English, ~380 MB, downloaded once from Setup).
//!
//! The pure parts (text normalization, chunking, mapping aligned words back
//! onto the transcript) build without the model; running the model needs the
//! `word-align` feature.

use crate::error::{CoreError, Result};
use crate::model::{Transcript, Word};
use std::path::PathBuf;

/// The model's files, pinned to one revision of the Hugging Face repo so the
/// checksums stay valid.
pub const ALIGNER_REPO: &str = "facebook/wav2vec2-base-960h";
pub const ALIGNER_REVISION: &str = "22aad52d435eb6dbaf354bdad9b0da84ce7d6156";
pub const ALIGNER_FILES: &[(&str, &str, u64)] = &[
    ("config.json", "d3ec255c063d9f95057b553b19c20135b259875834a4fe9deb218a6be25b4cf3", 1_596),
    ("vocab.json", "19727f8944fe6459fc3f240ae2c198395b740f6a029bd23e06656266b83bcf64", 291),
    ("model.safetensors", "8aa76ab2243c81747a1f832954586bc566090c83a0ac167df6f31f0fa917d74a", 377_607_901),
];

/// `<data dir>/models/wav2vec2-base-960h/`, or `ROUGHCUT_ALIGNER_DIR`
/// (development: a copy of the model elsewhere).
pub fn aligner_dir() -> PathBuf {
    std::env::var_os("ROUGHCUT_ALIGNER_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::adapters::transcribe::models_dir().join("wav2vec2-base-960h"))
}

/// The model files are all present.
pub fn aligner_downloaded() -> bool {
    ALIGNER_FILES.iter().all(|(f, _, _)| aligner_dir().join(f).is_file())
}

/// This build can run the aligner.
pub fn aligner_compiled() -> bool {
    cfg!(feature = "word-align")
}

/// The model is English: other languages keep whisper's times.
pub fn language_supported(language: &str) -> bool {
    let l = language.to_ascii_lowercase();
    l == "en" || l.starts_with("en-") || l == "english"
}

/// What the aligner can read: upper-case letters and apostrophes, words
/// split on anything else ("self-driving" is two words, "24" is none).
pub fn normalize(text: &str) -> Vec<String> {
    text.to_uppercase()
        .chars()
        .map(|c| if c.is_ascii_alphabetic() || c == '\'' { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .filter(|t| t.chars().any(|c| c.is_ascii_alphabetic()))
        .map(str::to_owned)
        .collect()
}

/// A word the model placed, in seconds from the start of the video.
#[derive(Debug, Clone, PartialEq)]
pub struct AlignedWord {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

/// A stretch of the transcript aligned in one model run: whole segments,
/// about `CHUNK_S` long. Chunks don't overlap: each split sits at the
/// quietest moment within `MARGIN_S` of whisper's segment boundary, so a
/// chunk's audio holds its own words and none of its neighbours' (with
/// overlapping margins the model squeezed "but today" into the previous
/// phrase's "try this again").
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    /// Indices into `transcript.segments`.
    pub segments: Vec<usize>,
    pub start: f64,
    pub end: f64,
}

pub const CHUNK_S: f64 = 30.0;
pub const MARGIN_S: f64 = 1.0;

/// Loudness per 20 ms frame (mean absolute sample), for choosing quiet
/// split points.
pub fn frame_energy(samples: &[f32]) -> Vec<f32> {
    samples.chunks(320).map(|c| c.iter().map(|v| v.abs()).sum::<f32>() / c.len().max(1) as f32).collect()
}

pub fn chunks(transcript: &Transcript, duration: f64, energy: Option<&[f32]>) -> Vec<Chunk> {
    let mut out: Vec<Chunk> = vec![];
    let mut current: Option<Chunk> = None;
    for (i, seg) in transcript.segments.iter().enumerate() {
        if seg.is_silence || seg.words.is_empty() {
            continue;
        }
        match current.as_mut() {
            Some(c) if seg.end - c.start <= CHUNK_S => {
                c.segments.push(i);
                c.end = seg.end;
            }
            _ => {
                if let Some(c) = current.take() {
                    out.push(c);
                }
                current = Some(Chunk { segments: vec![i], start: seg.start, end: seg.end });
            }
        }
    }
    out.extend(current);
    let end_of_video = if duration > 0.0 { duration } else { out.last().map(|c| c.end + MARGIN_S).unwrap_or(0.0) };
    // Split points between neighbouring chunks: the quietest 20 ms frame
    // within MARGIN_S of the midpoint of whisper's boundary.
    let mut splits: Vec<f64> = vec![];
    for pair in out.windows(2) {
        let mid = (pair[0].end + pair[1].start) / 2.0;
        let split = energy
            .and_then(|e| {
                let a = ((mid - MARGIN_S).max(0.0) * 50.0) as usize;
                let b = (((mid + MARGIN_S) * 50.0) as usize).min(e.len());
                (a < b).then(|| (a..b).min_by(|&i, &j| e[i].total_cmp(&e[j])).unwrap())
            })
            .map(|i| i as f64 / 50.0)
            .unwrap_or(mid);
        splits.push(split);
    }
    let n = out.len();
    for (i, c) in out.iter_mut().enumerate() {
        c.start = if i == 0 { 0.0f64.max(c.start - MARGIN_S) } else { splits[i - 1] };
        c.end = if i + 1 == n { end_of_video.min(c.end + MARGIN_S).max(c.end) } else { splits[i] };
    }
    out
}

/// The text a chunk sends to the model: every word's normalized tokens.
pub fn chunk_text(transcript: &Transcript, chunk: &Chunk) -> Vec<String> {
    chunk
        .segments
        .iter()
        .flat_map(|&i| transcript.segments[i].words.iter())
        .flat_map(|w| normalize(&w.text))
        .collect()
}

/// Put the model's times on the chunk's words, in order. A word made of
/// several tokens spans them; a word with none ("24", "%") sits evenly in
/// the gap between its placed neighbours. If the model's words don't match
/// the text it was given, nothing changes (returns false).
pub fn apply_alignment(transcript: &mut Transcript, chunk: &Chunk, aligned: &[AlignedWord]) -> bool {
    let expected = chunk_text(transcript, chunk);
    if expected.len() != aligned.len()
        || expected.iter().zip(aligned).any(|(e, a)| !e.eq_ignore_ascii_case(&a.text))
    {
        return false;
    }
    // Every word in the chunk, in order, with its tokens' times if any.
    let mut placed: Vec<(usize, usize, Option<(f64, f64)>)> = vec![];
    let mut k = 0;
    for &si in &chunk.segments {
        for (wi, w) in transcript.segments[si].words.iter().enumerate() {
            let n = normalize(&w.text).len();
            let span = (n > 0).then(|| (aligned[k].start, aligned[k + n - 1].end));
            k += n;
            placed.push((si, wi, span));
        }
    }
    // Words without tokens: evenly between the neighbours that have times.
    let mut i = 0;
    while i < placed.len() {
        if placed[i].2.is_some() {
            i += 1;
            continue;
        }
        let first = i;
        while i < placed.len() && placed[i].2.is_none() {
            i += 1;
        }
        let from = first.checked_sub(1).and_then(|j| placed[j].2).map(|s| s.1).unwrap_or(chunk.start);
        let to = placed.get(i).and_then(|p| p.2).map(|s| s.0).unwrap_or(from.max(chunk.end));
        let n = (i - first) as f64;
        let step = ((to - from) / n).max(0.0);
        for (j, p) in placed[first..i].iter_mut().enumerate() {
            let s = from + step * j as f64;
            p.2 = Some((s, s + step));
        }
    }
    for (si, wi, span) in placed {
        if let Some((start, end)) = span {
            let w: &mut Word = &mut transcript.segments[si].words[wi];
            w.start = start;
            w.end = end.max(start);
        }
    }
    // Segments follow their words.
    for &si in &chunk.segments {
        let seg = &mut transcript.segments[si];
        if let (Some(f), Some(l)) = (seg.words.first(), seg.words.last()) {
            seg.start = f.start;
            seg.end = l.end.max(f.start);
        }
    }
    true
}

/// What an alignment run did.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AlignStats {
    pub chunks: usize,
    pub aligned: usize,
    /// Chunks whose words kept whisper's times (the model's words didn't
    /// match, or the run failed).
    pub kept: usize,
}

/// Align the whole transcript against 16 kHz mono samples of the media.
/// `on_progress(fraction)` after each chunk. CPU-heavy: call from a
/// blocking thread.
#[cfg(feature = "word-align")]
pub fn align_transcript(
    transcript: &mut Transcript,
    samples: &[f32],
    duration: f64,
    on_progress: &dyn Fn(f64),
) -> Result<AlignStats> {
    use wav2vec2_rs::{AlignmentInput, ForcedAlignerBuilder, Wav2Vec2Config};
    if !aligner_downloaded() {
        return Err(CoreError::Unavailable(
            "the word alignment model isn't downloaded (Setup → Word alignment)".into(),
        ));
    }
    let dir = aligner_dir();
    let aligner = ForcedAlignerBuilder::new(Wav2Vec2Config {
        model_path: dir.join("model.safetensors").to_string_lossy().into_owned(),
        config_path: dir.join("config.json").to_string_lossy().into_owned(),
        vocab_path: dir.join("vocab.json").to_string_lossy().into_owned(),
        device: "cpu".into(),
        expected_sample_rate_hz: 16_000,
    })
    .build()
    .map_err(|e| CoreError::Unavailable(format!("word alignment model: {e}")))?;
    let energy = frame_energy(samples);
    let list = chunks(transcript, duration, Some(&energy));
    let mut stats = AlignStats { chunks: list.len(), ..Default::default() };
    for (n, chunk) in list.iter().enumerate() {
        let text = chunk_text(transcript, chunk);
        let a = ((chunk.start * 16_000.0) as usize).min(samples.len());
        let b = ((chunk.end * 16_000.0) as usize).min(samples.len());
        let ok = !text.is_empty()
            && b > a
            && aligner
                .align(&AlignmentInput {
                    sample_rate_hz: 16_000,
                    samples: samples[a..b].to_vec(),
                    transcript: text.join(" "),
                    normalized: None,
                })
                .ok()
                .map(|out| {
                    let words: Vec<AlignedWord> = out
                        .words
                        .iter()
                        .map(|w| AlignedWord {
                            text: w.word.clone(),
                            start: chunk.start + w.start_ms as f64 / 1000.0,
                            end: chunk.start + w.end_ms as f64 / 1000.0,
                        })
                        .collect();
                    apply_alignment(transcript, chunk, &words)
                })
                .unwrap_or(false);
        if ok {
            stats.aligned += 1;
        } else {
            stats.kept += 1;
        }
        on_progress((n + 1) as f64 / stats.chunks.max(1) as f64);
    }
    Ok(stats)
}

/// Without the `word-align` feature: always unavailable.
#[cfg(not(feature = "word-align"))]
pub fn align_transcript(
    _transcript: &mut Transcript,
    _samples: &[f32],
    _duration: f64,
    _on_progress: &dyn Fn(f64),
) -> Result<AlignStats> {
    Err(CoreError::Unavailable("this build has no word aligner".into()))
}

/// 16 kHz mono f32 samples of a media file's first audio track, via ffmpeg.
pub async fn read_audio_16k(file_path: &str) -> Result<Vec<f32>> {
    let ffmpeg = crate::adapters::video::ffmpeg_path()
        .ok_or_else(|| CoreError::Unavailable("aligning words reads the audio, which needs ffmpeg".into()))?;
    let out = tokio::process::Command::new(ffmpeg)
        .args(["-v", "error", "-i", file_path, "-map", "0:a:0", "-vn", "-ac", "1", "-ar", "16000", "-f", "s16le", "pipe:1"])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| CoreError::Unavailable(format!("ffmpeg: {e}")))?;
    if !out.status.success() {
        return Err(CoreError::Unavailable(format!(
            "ffmpeg couldn't read the audio: {}",
            String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("")
        )));
    }
    Ok(out.stdout.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TranscriptSegment;
    use uuid::Uuid;

    fn w(t: &str, a: f64, b: f64) -> Word {
        Word { text: t.into(), start: a, end: b, confidence: 0.9 }
    }
    fn seg(words: Vec<Word>) -> TranscriptSegment {
        TranscriptSegment {
            id: Uuid::new_v4(),
            start: words[0].start,
            end: words.last().unwrap().end,
            text: words.iter().map(|w| w.text.clone()).collect::<Vec<_>>().join(" "),
            words,
            is_filler: false,
            is_silence: false,
            take_group_id: None,
            is_best_take: false,
        }
    }
    fn transcript(segments: Vec<TranscriptSegment>) -> Transcript {
        Transcript {
            id: Uuid::new_v4(),
            media_id: Uuid::new_v4(),
            language: "en".into(),
            segments,
            model_used: "test".into(),
            words_trimmed: false,
            words_aligned: false,
        }
    }
    fn aw(t: &str, a: f64, b: f64) -> AlignedWord {
        AlignedWord { text: t.into(), start: a, end: b }
    }

    #[test]
    fn text_is_what_the_model_can_read() {
        assert_eq!(normalize("Charm, maybe."), ["CHARM", "MAYBE"]);
        assert_eq!(normalize("it's"), ["IT'S"]);
        assert_eq!(normalize("self-driving"), ["SELF", "DRIVING"]);
        assert!(normalize("24").is_empty());
        assert!(normalize("—").is_empty());
        assert_eq!(normalize("'s"), ["'S"]);
    }

    #[test]
    fn chunks_hold_whole_segments_with_a_margin() {
        let t = transcript(vec![
            seg(vec![w("a", 0.0, 10.0)]),
            seg(vec![w("b", 10.0, 25.0)]),
            seg(vec![w("c", 25.0, 40.0)]),
        ]);
        let c = chunks(&t, 41.0, None);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].segments, [0, 1]);
        assert_eq!(c[1].segments, [2]);
        assert_eq!(c[0].start, 0.0);
        assert_eq!(c[0].end, c[1].start, "no overlap, no gap");
        assert_eq!(c[0].end, 25.0, "without audio: whisper's boundary");
        assert_eq!(c[1].end, 41.0, "the last chunk runs to the end, clamped to the video");
    }

    #[test]
    fn chunks_split_at_the_quiet_moment_near_whisper_s_boundary() {
        // Whisper's boundary between the phrases is 28.5 s, but the pause is
        // at 28.6-28.9 s; the split goes there.
        let t = transcript(vec![
            seg(vec![w("again", 1.0, 28.5)]),
            seg(vec![w("but", 28.5, 40.0)]),
        ]);
        let mut energy = vec![0.3f32; 50 * 41];
        for f in energy.iter_mut().take((28.9 * 50.0) as usize).skip((28.6 * 50.0) as usize) {
            *f = 0.001;
        }
        let c = chunks(&t, 41.0, Some(&energy));
        assert_eq!(c.len(), 2);
        assert!(c[0].end >= 28.6 && c[0].end < 28.9, "split {}", c[0].end);
        assert_eq!(c[0].end, c[1].start);
    }

    #[test]
    fn aligned_times_land_on_the_words_and_gaps_fill_numbers() {
        // The stream's "but": whisper had it in silence at 28.5.
        let mut t = transcript(vec![seg(vec![
            w("again", 28.1, 28.5),
            w("but", 28.5, 28.74),
            w("24", 28.8, 28.9),
            w("today", 29.02, 29.14),
        ])]);
        let c = chunks(&t, 60.0, None).remove(0);
        assert_eq!(chunk_text(&t, &c), ["AGAIN", "BUT", "TODAY"]);
        let ok = apply_alignment(
            &mut t,
            &c,
            &[aw("AGAIN", 28.22, 28.72), aw("BUT", 29.0, 29.44), aw("TODAY", 29.58, 29.92)],
        );
        assert!(ok);
        let ws = &t.segments[0].words;
        assert_eq!((ws[1].start, ws[1].end), (29.0, 29.44));
        assert!(ws[2].start >= 29.44 && ws[2].end <= 29.58, "\"24\" between its neighbours: {:?}", ws[2]);
        assert_eq!((t.segments[0].start, t.segments[0].end), (28.22, 29.92), "the segment follows its words");
    }

    #[test]
    fn a_mismatched_alignment_changes_nothing() {
        let mut t = transcript(vec![seg(vec![w("hello", 1.0, 1.5), w("world", 1.5, 2.0)])]);
        let before = t.clone();
        let c = chunks(&t, 10.0, None).remove(0);
        assert!(!apply_alignment(&mut t, &c, &[aw("HELLO", 1.1, 1.4)]), "one word short");
        assert!(!apply_alignment(&mut t, &c, &[aw("HELLO", 1.1, 1.4), aw("WORD", 1.5, 1.9)]), "wrong word");
        assert_eq!(t.segments[0].words[0].start, before.segments[0].words[0].start);
    }

    #[test]
    fn a_word_of_several_tokens_spans_them() {
        let mut t = transcript(vec![seg(vec![w("self-driving", 3.0, 4.0), w("cars", 4.0, 4.5)])]);
        let c = chunks(&t, 10.0, None).remove(0);
        let ok = apply_alignment(&mut t, &c, &[aw("SELF", 3.2, 3.4), aw("DRIVING", 3.45, 3.8), aw("CARS", 3.9, 4.3)]);
        assert!(ok);
        assert_eq!((t.segments[0].words[0].start, t.segments[0].words[0].end), (3.2, 3.8));
    }
}
