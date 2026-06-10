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
    Ok(Transcript { id: Uuid::new_v4(), media_id, language, segments, model_used })
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

            let ctx = WhisperContext::new_with_params(&model, WhisperContextParameters::default())
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
                    crate::events::send(
                        &sink,
                        crate::events::CoreEvent::progress(
                            "transcribe",
                            Some(media_id),
                            f64::from(pct.clamp(0, 100)) / 100.0,
                            "transcribing",
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
                let mut words = vec![];
                for i in 0..seg.n_tokens() {
                    let Some(tok) = seg.get_token(i) else { continue };
                    let t = tok.to_str_lossy().map(|s| s.into_owned()).unwrap_or_default();
                    if t.starts_with("[_") {
                        continue; // special tokens
                    }
                    let data = tok.token_data();
                    words.push(Word {
                        text: t.trim().to_string(),
                        start: data.t0 as f64 / 100.0,
                        end: data.t1 as f64 / 100.0,
                        confidence: f64::from(tok.token_probability()),
                    });
                }
                segments.push(TranscriptSegment {
                    id: Uuid::new_v4(),
                    start: seg.start_timestamp() as f64 / 100.0,
                    end: seg.end_timestamp() as f64 / 100.0,
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
        sink: &SharedSink,
    ) -> Result<Transcript> {
        for i in 1..=4 {
            crate::events::send(
                sink,
                crate::events::CoreEvent::progress("transcribe", None, i as f64 * 0.25, "transcribing (demo)"),
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
        sink: &SharedSink,
    ) -> Result<Transcript> {
        let caps = crate::capabilities::probe();
        if caps.demo() {
            return MockTranscriber.transcribe(media, wav_path, language, sink).await;
        }
        #[cfg(feature = "whisper-native")]
        {
            return WhisperRs.transcribe(media, wav_path, language, sink).await;
        }
        #[cfg(not(feature = "whisper-native"))]
        {
            if caps.whisper_cli {
                WhisperCli.transcribe(media, wav_path, language, sink).await
            } else {
                Err(CoreError::Unavailable(
                    "no transcription engine: install whisper-cpp (whisper-cli) or build with the whisper-native feature"
                        .into(),
                ))
            }
        }
    }
}
