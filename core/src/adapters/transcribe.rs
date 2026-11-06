//! Transcriber: on-device speech-to-text behind a trait.
//! Production = in-process whisper.cpp via whisper-rs (feature `whisper-native`),
//! falling back to the `whisper-cli` binary; demo/tests = fixture.

use crate::error::{CoreError, Result};
use crate::events::SharedSink;
use crate::model::{Media, Transcript, TranscriptSegment, Word};
use async_trait::async_trait;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;
use uuid::Uuid;

/// Model files we know how to use, best first. The first one present in
/// `<data dir>/models/` wins; `WHISPER_MODEL` overrides everything.
pub const MODEL_CANDIDATES: &[&str] = &[
    "ggml-large-v3-turbo-q5_0.bin",
    "ggml-small-q5_1.bin",
    "ggml-base.bin",
];

pub fn models_dir() -> PathBuf {
    crate::store::data_dir().join("models")
}

/// Resolve the whisper model to load, or None if nothing is downloaded yet.
pub fn resolve_whisper_model() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("WHISPER_MODEL") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    MODEL_CANDIDATES.iter().map(|f| models_dir().join(f)).find(|p| p.is_file())
}

fn require_model() -> Result<PathBuf> {
    resolve_whisper_model().ok_or_else(|| {
        CoreError::Unavailable(format!(
            "no whisper model found in {} — download one from the setup screen or set WHISPER_MODEL",
            models_dir().display()
        ))
    })
}

/// True when the in-process whisper.cpp engine is compiled in.
pub fn whisper_native_enabled() -> bool {
    cfg!(feature = "whisper-native")
}

#[async_trait]
pub trait Transcriber: Send + Sync {
    /// Transcribe `wav_path` (16 kHz mono) for `media`. Implementations emit
    /// `progress` events on `sink` as they go.
    async fn transcribe(
        &self,
        media: &Media,
        wav_path: &str,
        language: &str,
        project_id: Option<uuid::Uuid>,
        sink: &SharedSink,
    ) -> Result<Transcript>;
}

pub fn whisper_available() -> bool {
    std::process::Command::new("which")
        .arg("whisper-cli")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// whisper.cpp CLI adapter. Expects `whisper-cli` on PATH and a GGML model at
/// `$WHISPER_MODEL` (path) or `<data dir>/models/ggml-base.bin`.
pub struct WhisperCli;

#[async_trait]
impl Transcriber for WhisperCli {
    async fn transcribe(
        &self,
        media: &Media,
        wav_path: &str,
        language: &str,
        _project_id: Option<uuid::Uuid>,
        _sink: &SharedSink,
    ) -> Result<Transcript> {
        let model = require_model()?.to_string_lossy().into_owned();
        let out_base = format!("{wav_path}.transcript");
        let mut cmd = Command::new("whisper-cli");
        cmd.args(["-m", &model, "-f", wav_path, "-ojf", "-of", &out_base]);
        if language != "auto" && !language.is_empty() {
            cmd.args(["-l", language]);
        }
        let out = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| CoreError::Unavailable(format!("whisper-cli: {e}")))?;
        if !out.status.success() {
            return Err(CoreError::Other(format!(
                "whisper-cli failed: {}",
                String::from_utf8_lossy(&out.stderr).chars().take(800).collect::<String>()
            )));
        }
        let json = std::fs::read_to_string(format!("{out_base}.json"))?;
        parse_whisper_json(&json, media.id)
    }
}

/// Parse whisper.cpp `--output-json-full` output into our model.
fn parse_whisper_json(json: &str, media_id: Uuid) -> Result<Transcript> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    let language = v["result"]["language"].as_str().unwrap_or("en").to_string();
    let model_used = v["model"]["type"].as_str().unwrap_or("whisper").to_string();
    let mut segments = vec![];
    for seg in v["transcription"].as_array().cloned().unwrap_or_default() {
        let start = seg["offsets"]["from"].as_f64().unwrap_or(0.0) / 1000.0;
        let end = seg["offsets"]["to"].as_f64().unwrap_or(0.0) / 1000.0;
        let text = seg["text"].as_str().unwrap_or("").trim().to_string();
        let mut words = vec![];
        for tok in seg["tokens"].as_array().cloned().unwrap_or_default() {
            let t = tok["text"].as_str().unwrap_or("");
            if t.starts_with("[_") {
                continue; // special tokens
            }
            words.push(Word {
                text: t.trim().to_string(),
                start: tok["offsets"]["from"].as_f64().unwrap_or(0.0) / 1000.0,
                end: tok["offsets"]["to"].as_f64().unwrap_or(0.0) / 1000.0,
                confidence: tok["p"].as_f64().unwrap_or(1.0),
            });
        }
        if text.is_empty() {
            continue;
        }
        segments.push(TranscriptSegment {
            id: Uuid::new_v4(),
            start,
            end,
            text,
            words,
            is_filler: false,
            is_silence: false,
            take_group_id: None,
            is_best_take: false,
        });
    }
    Ok(Transcript { id: Uuid::new_v4(), media_id, language, segments, model_used, words_trimmed: false })
}

/// whisper.cpp's alignment-heads preset for a model file, by name: DTW word
/// alignment needs to know which attention heads track time. None = an
/// unknown model; DTW stays off and raw token times are used.
#[cfg(feature = "whisper-native")]
fn dtw_preset(model_name: &str) -> Option<whisper_rs::DtwModelPreset> {
    use whisper_rs::DtwModelPreset as P;
    let n = model_name.to_ascii_lowercase();
    let english = n.contains(".en");
    Some(if n.contains("large-v3-turbo") {
        P::LargeV3Turbo
    } else if n.contains("large-v3") {
        P::LargeV3
    } else if n.contains("large-v2") {
        P::LargeV2
    } else if n.contains("large-v1") || n.ends_with("large") || n.contains("large-q") {
        P::LargeV1
    } else if n.contains("medium") {
        if english { P::MediumEn } else { P::Medium }
    } else if n.contains("small") {
        if english { P::SmallEn } else { P::Small }
    } else if n.contains("base") {
        if english { P::BaseEn } else { P::Base }
    } else if n.contains("tiny") {
        if english { P::TinyEn } else { P::Tiny }
    } else {
        return None;
    })
}

/// One whisper token with its times (seconds).
#[derive(Debug, Clone)]
pub struct RawToken {
    pub text: String,
    pub t0: f64,
    pub t1: f64,
    /// The DTW-aligned time, when alignment ran.
    pub t_dtw: Option<f64>,
    pub p: f64,
}

/// Join whisper's tokens into words. A token that starts with a space
/// begins a new word; one that doesn't (the rest of a long word, a
/// contraction, punctuation) joins the word before it. Special tokens
/// (`[_BEG_]`, `<|en|>`, …) are dropped.
///
/// A word starts at its first token's aligned time (DTW) when there is one,
/// else at the raw token time, and ends where the next word starts (the last
/// word at the segment's end). Times are kept in order and inside the
/// segment. The audio trim (`detect::trim_words_to_audio`) then pulls each
/// word's edges in to where there is actually sound.
pub fn tokens_to_words(tokens: &[RawToken], seg_start: f64, seg_end: f64) -> Vec<Word> {
    struct Building {
        text: String,
        start: f64,
        p_sum: f64,
        n: usize,
    }
    let mut built: Vec<Building> = vec![];
    for tok in tokens {
        let raw = tok.text.as_str();
        if raw.starts_with("[_") || raw.starts_with("<|") || raw.trim().is_empty() {
            continue;
        }
        let time = tok.t_dtw.unwrap_or(tok.t0);
        let starts_word = raw.starts_with(' ') || built.is_empty();
        if starts_word {
            built.push(Building { text: raw.trim().to_string(), start: time, p_sum: tok.p, n: 1 });
        } else if let Some(last) = built.last_mut() {
            last.text.push_str(raw.trim());
            last.p_sum += tok.p;
            last.n += 1;
        }
    }
    let lo = seg_start.min(seg_end);
    let hi = seg_end.max(seg_start);
    let mut words: Vec<Word> = Vec::with_capacity(built.len());
    let mut prev = lo;
    for b in &built {
        let start = b.start.clamp(prev, hi);
        words.push(Word { text: b.text.clone(), start, end: start, confidence: b.p_sum / b.n as f64 });
        prev = start;
    }
    for i in 0..words.len() {
        let next = if i + 1 < words.len() { words[i + 1].start } else { hi };
        words[i].end = next.max(words[i].start);
    }
    words
}

/// In-process whisper.cpp via the `whisper-rs` bindings (feature
/// `whisper-native`). No PATH dependency, no temp JSON, and real progress
/// events through the model's progress callback. Metal/GPU is enabled by the
/// underlying whisper.cpp build where available.
#[cfg(feature = "whisper-native")]
pub struct WhisperRs;

#[cfg(feature = "whisper-native")]
#[async_trait]
impl Transcriber for WhisperRs {
    async fn transcribe(
        &self,
        media: &Media,
        wav_path: &str,
        language: &str,
        project_id: Option<uuid::Uuid>,
        sink: &SharedSink,
    ) -> Result<Transcript> {
        let model = require_model()?;
        let samples = read_wav_mono_f32(wav_path)?;
        let media_id = media.id;
        let lang = language.to_string();
        let sink = sink.clone();
        let model_name = model
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "whisper".into());

        // whisper.cpp's full() is CPU/GPU-bound and blocking.
        let result = tokio::task::spawn_blocking(move || -> Result<Transcript> {
            use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

            // DTW alignment: whisper.cpp aligns each token to the audio using
            // the model's alignment heads, so word times stop stretching
            // across pauses (raw token times do). Known models only.
            let mut ctx_params = WhisperContextParameters::default();
            if let Some(preset) = dtw_preset(&model_name) {
                ctx_params.dtw_parameters(whisper_rs::DtwParameters {
                    mode: whisper_rs::DtwMode::ModelPreset { model_preset: preset },
                    ..Default::default()
                });
            }
            let ctx = WhisperContext::new_with_params(&model, ctx_params)
                .map_err(|e| CoreError::Unavailable(format!("whisper model load: {e}")))?;
            let mut state = ctx
                .create_state()
                .map_err(|e| CoreError::Other(format!("whisper state: {e}")))?;

            let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            if !lang.is_empty() {
                params.set_language(Some(&lang)); // "auto" triggers detection
            }
            params.set_token_timestamps(true);
            params.set_print_special(false);
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_timestamps(false);
            let threads =
                std::thread::available_parallelism().map(|n| n.get() as i32).unwrap_or(4);
            params.set_n_threads(threads.min(8));
            {
                let sink = sink.clone();
                params.set_progress_callback_safe(move |pct: i32| {
                    // The UI filters progress by PROJECT id; map whisper's
                    // 0-100% into the 25..98% band after the engine's
                    // extract/start pre-events so the bar never jumps back.
                    let frac = 0.25 + f64::from(pct.clamp(0, 100)) / 100.0 * 0.73;
                    crate::events::send(
                        &sink,
                        crate::events::CoreEvent::progress(crate::events::ProgressTask::Transcribe,
                            project_id,
                            frac,
                            format!("transcribing on-device — {}%", pct.clamp(0, 100)),
                        ),
                    );
                });
            }

            state
                .full(params, &samples)
                .map_err(|e| CoreError::Other(format!("whisper transcribe: {e}")))?;

            let mut segments = vec![];
            for seg in state.as_iter() {
                let text = seg.to_str_lossy().map(|s| s.trim().to_string()).unwrap_or_default();
                if text.is_empty() {
                    continue;
                }
                let seg_start = seg.start_timestamp() as f64 / 100.0;
                let seg_end = seg.end_timestamp() as f64 / 100.0;
                let mut tokens = vec![];
                for i in 0..seg.n_tokens() {
                    let Some(tok) = seg.get_token(i) else { continue };
                    let data = tok.token_data();
                    tokens.push(RawToken {
                        text: tok.to_str_lossy().map(|s| s.into_owned()).unwrap_or_default(),
                        t0: data.t0 as f64 / 100.0,
                        t1: data.t1 as f64 / 100.0,
                        t_dtw: (data.t_dtw >= 0).then(|| data.t_dtw as f64 / 100.0),
                        p: f64::from(tok.token_probability()),
                    });
                }
                let words = tokens_to_words(&tokens, seg_start, seg_end);
                segments.push(TranscriptSegment {
                    id: Uuid::new_v4(),
                    start: seg_start,
                    end: seg_end,
                    text,
                    words,
                    is_filler: false,
                    is_silence: false,
                    take_group_id: None,
                    is_best_take: false,
                });
            }

            let detected = whisper_rs::get_lang_str(state.full_lang_id_from_state())
                .map(str::to_string)
                .unwrap_or_else(|| if lang.is_empty() || lang == "auto" { "en".into() } else { lang });
            Ok(Transcript {
                id: Uuid::new_v4(),
                media_id,
                language: detected,
                segments,
                model_used: model_name,
                words_trimmed: false,
            })
        })
        .await
        .map_err(|e| CoreError::Other(format!("whisper task: {e}")))??;

        Ok(result)
    }
}

/// Read a (16 kHz mono) WAV into f32 samples for whisper.
#[cfg(feature = "whisper-native")]
fn read_wav_mono_f32(path: &str) -> Result<Vec<f32>> {
    let mut reader =
        hound::WavReader::open(path).map_err(|e| CoreError::Other(format!("wav open: {e}")))?;
    let spec = reader.spec();
    let to_err = |e: hound::Error| CoreError::Other(format!("wav read: {e}"));
    let mut samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => reader
            .samples::<i16>()
            .map(|s| s.map(|v| f32::from(v) / 32768.0))
            .collect::<std::result::Result<_, _>>()
            .map_err(to_err)?,
        hound::SampleFormat::Float => {
            reader.samples::<f32>().collect::<std::result::Result<_, _>>().map_err(to_err)?
        }
    };
    if spec.channels > 1 {
        // Average interleaved channels down to mono.
        let ch = spec.channels as usize;
        samples = samples.chunks(ch).map(|c| c.iter().sum::<f32>() / c.len() as f32).collect();
    }
    Ok(samples)
}

/// Demo/test transcriber: returns the bundled fixture transcript scaled to the
/// media duration.
pub struct MockTranscriber;

#[async_trait]
impl Transcriber for MockTranscriber {
    async fn transcribe(
        &self,
        media: &Media,
        _wav_path: &str,
        _language: &str,
        project_id: Option<uuid::Uuid>,
        sink: &SharedSink,
    ) -> Result<Transcript> {
        for i in 1..=4 {
            crate::events::send(
                sink,
                crate::events::CoreEvent::progress(crate::events::ProgressTask::Transcribe, project_id, i as f64 * 0.25, "transcribing (demo)"),
            );
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        }
        Ok(crate::demo::fixture_transcript(media))
    }
}

/// Resolves the best available transcriber PER CALL: fixture in demo mode,
/// the in-process engine when compiled in, the CLI binary otherwise — so a
/// model downloaded mid-session is picked up without restarting.
pub struct AutoTranscriber;

#[async_trait]
impl Transcriber for AutoTranscriber {
    async fn transcribe(
        &self,
        media: &Media,
        wav_path: &str,
        language: &str,
        project_id: Option<uuid::Uuid>,
        sink: &SharedSink,
    ) -> Result<Transcript> {
        let caps = crate::capabilities::probe();
        if caps.demo() {
            return MockTranscriber.transcribe(media, wav_path, language, project_id, sink).await;
        }
        #[cfg(feature = "whisper-native")]
        {
            return WhisperRs.transcribe(media, wav_path, language, project_id, sink).await;
        }
        #[cfg(not(feature = "whisper-native"))]
        {
            if caps.whisper_cli {
                WhisperCli.transcribe(media, wav_path, language, project_id, sink).await
            } else {
                Err(CoreError::Unavailable(
                    "no transcription engine: install whisper-cpp (whisper-cli) or build with the whisper-native feature"
                        .into(),
                ))
            }
        }
    }
}

#[cfg(test)]
mod word_tests {
    use super::*;

    fn tok(text: &str, t0: f64, t_dtw: Option<f64>) -> RawToken {
        RawToken { text: text.into(), t0, t1: t0 + 0.1, t_dtw, p: 0.9 }
    }

    #[test]
    fn tokens_join_into_words_and_specials_are_dropped() {
        let tokens = vec![
            tok("[_BEG_]", 0.0, None),
            tok(" Hello", 0.5, Some(0.6)),
            tok(",", 0.9, Some(0.95)),
            tok(" super", 1.0, Some(1.1)),
            tok("cut", 1.2, Some(1.3)),
            tok(" isn", 1.6, Some(1.7)),
            tok("'t", 1.8, Some(1.9)),
            tok(" um", 2.5, Some(3.4)),
            tok("<|endoftext|>", 4.0, None),
        ];
        let w = tokens_to_words(&tokens, 0.4, 4.0);
        let texts: Vec<&str> = w.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["Hello,", "supercut", "isn't", "um"]);
        // Starts come from DTW; each word ends where the next begins.
        assert_eq!((w[0].start, w[0].end), (0.6, 1.1));
        assert_eq!((w[3].start, w[3].end), (3.4, 4.0), "the last word ends at the segment's end");
    }

    #[test]
    fn times_stay_in_order_and_inside_the_segment() {
        // DTW can put a token before the segment or out of order.
        let tokens = vec![tok(" a", 1.0, Some(0.2)), tok(" b", 2.0, Some(1.5)), tok(" c", 3.0, Some(1.2))];
        let w = tokens_to_words(&tokens, 1.0, 5.0);
        assert_eq!(w[0].start, 1.0, "clamped to the segment start");
        assert!(w[1].start <= w[2].start, "never earlier than the word before");
        assert!(w.iter().all(|w| w.end >= w.start && w.end <= 5.0));
    }

    #[test]
    fn without_dtw_the_raw_token_time_is_used() {
        let w = tokens_to_words(&[tok(" hi", 2.0, None), tok(" there", 2.5, None)], 2.0, 3.0);
        assert_eq!((w[0].start, w[1].start), (2.0, 2.5));
    }
}
