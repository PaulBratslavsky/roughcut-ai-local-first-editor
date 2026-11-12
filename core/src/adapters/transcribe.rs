//! Transcriber: on-device speech-to-text behind a trait.
//! Production = in-process whisper.cpp via whisper-rs (feature `whisper-native`),
//! falling back to the `whisper-cli` binary; demo/tests = fixture.
//!
//! Word times come from whisper.cpp's DTW token timestamps when the model file
//! is one we know the alignment heads for (`dtw_preset_for_model`), else from
//! its plain token timestamps; `assemble_segments` turns either into Words.

use crate::error::{CoreError, Result};
use crate::events::SharedSink;
use crate::model::{Media, Transcript, TranscriptSegment, Word};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
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

// ------------------------------------------------------------ DTW presets

/// whisper.cpp's alignment-head preset for DTW token timestamps. Each Whisper
/// size has its own cross-attention heads that follow the audio, so the preset
/// must match the model file. Unknown models get no DTW and keep plain token
/// timestamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DtwPreset {
    TinyEn,
    Tiny,
    BaseEn,
    Base,
    SmallEn,
    Small,
    MediumEn,
    Medium,
    LargeV1,
    LargeV2,
    LargeV3,
    LargeV3Turbo,
}

impl DtwPreset {
    /// The value `whisper-cli --dtw` takes.
    pub fn cli_name(self) -> &'static str {
        match self {
            DtwPreset::TinyEn => "tiny.en",
            DtwPreset::Tiny => "tiny",
            DtwPreset::BaseEn => "base.en",
            DtwPreset::Base => "base",
            DtwPreset::SmallEn => "small.en",
            DtwPreset::Small => "small",
            DtwPreset::MediumEn => "medium.en",
            DtwPreset::Medium => "medium",
            DtwPreset::LargeV1 => "large.v1",
            DtwPreset::LargeV2 => "large.v2",
            DtwPreset::LargeV3 => "large.v3",
            DtwPreset::LargeV3Turbo => "large.v3.turbo",
        }
    }

    #[cfg(feature = "whisper-native")]
    fn native(self) -> whisper_rs::DtwModelPreset {
        use whisper_rs::DtwModelPreset as P;
        match self {
            DtwPreset::TinyEn => P::TinyEn,
            DtwPreset::Tiny => P::Tiny,
            DtwPreset::BaseEn => P::BaseEn,
            DtwPreset::Base => P::Base,
            DtwPreset::SmallEn => P::SmallEn,
            DtwPreset::Small => P::Small,
            DtwPreset::MediumEn => P::MediumEn,
            DtwPreset::Medium => P::Medium,
            DtwPreset::LargeV1 => P::LargeV1,
            DtwPreset::LargeV2 => P::LargeV2,
            DtwPreset::LargeV3 => P::LargeV3,
            DtwPreset::LargeV3Turbo => P::LargeV3Turbo,
        }
    }
}

/// The DTW preset for a model file, from its name `ggml-<model>[-<weights>].bin`:
/// `ggml-large-v3-turbo-q5_0.bin` → `LargeV3Turbo`, `ggml-small.en.bin` →
/// `SmallEn`. A quantized file (`-q5_0`, `-q8_0`, `-f16`, …) has its base
/// model's heads. Anything else (`distil-*`, fine-tunes, a bare
/// `ggml-large.bin`) → `None`.
pub fn dtw_preset_for_model(model: &Path) -> Option<DtwPreset> {
    let name = model.file_name()?.to_str()?.to_ascii_lowercase();
    let name = name.strip_suffix(".bin").unwrap_or(&name);
    let name = name.strip_prefix("ggml-").unwrap_or(name);
    let base = match name.rsplit_once('-') {
        Some((model, weights)) if is_weight_type(weights) => model,
        _ => name,
    };
    Some(match base {
        "tiny.en" => DtwPreset::TinyEn,
        "tiny" => DtwPreset::Tiny,
        "base.en" => DtwPreset::BaseEn,
        "base" => DtwPreset::Base,
        "small.en" => DtwPreset::SmallEn,
        "small" => DtwPreset::Small,
        "medium.en" => DtwPreset::MediumEn,
        "medium" => DtwPreset::Medium,
        "large-v1" => DtwPreset::LargeV1,
        "large-v2" => DtwPreset::LargeV2,
        "large-v3" => DtwPreset::LargeV3,
        "large-v3-turbo" => DtwPreset::LargeV3Turbo,
        _ => return None,
    })
}

/// A weight-type suffix (`q5_0`, `q8_0`, `q4_k`, `f16`, `f32`), not part of
/// the model's name.
fn is_weight_type(s: &str) -> bool {
    let quant = s.strip_prefix('q').is_some_and(|rest| {
        rest.starts_with(|c: char| c.is_ascii_digit())
            && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    });
    quant || s == "f16" || s == "f32"
}

// ------------------------------------------------------------ word times

/// One token as whisper decoded it, in seconds.
#[derive(Debug, Clone, PartialEq)]
struct RawToken {
    text: String,
    /// whisper's token-timestamp estimate (the fallback).
    t0: f64,
    t1: f64,
    /// DTW time: when the model emitted the token, which is about where the
    /// token ends in the audio. `None` when DTW was off or failed.
    dtw: Option<f64>,
    p: f64,
}

/// One segment as whisper decoded it, in seconds; tokens include specials.
#[derive(Debug, Clone, PartialEq)]
struct RawSegment {
    start: f64,
    end: f64,
    text: String,
    tokens: Vec<RawToken>,
}

/// whisper.cpp centiseconds → seconds.
fn centis(t: i64) -> f64 {
    t as f64 / 100.0
}

/// A whisper.cpp DTW time (centiseconds; -1 = not computed) in seconds.
fn dtw_secs(t_dtw: i64) -> Option<f64> {
    (t_dtw >= 0).then(|| centis(t_dtw))
}

// DTW tuning, checked against the audio energy of 3 minutes of a real stream
// with large-v3-turbo and small: speech outside every word fell from 12-24%
// of speech frames with token timestamps to 2-8% with these, and more pauses
// stayed gaps between words.
/// A DTW span longer than this many median spans holds a pause (the model
/// waits on the next token while nobody speaks): keep only its tail, as
/// openai-whisper does, so the pause stays a gap between words.
const DTW_MAX_SPAN_MEDIANS: f64 = 2.0;
/// The median span used for that cap is at most this (openai-whisper's 0.7 s).
const DTW_MEDIAN_CEIL_S: f64 = 0.7;
/// Before a pause the model often emits a word a little before its sound
/// ends: extend a DTW word's end this far into a following gap.
const DTW_END_PAD_S: f64 = 0.1;
/// A DTW time further than this outside its segment is noise: that token
/// keeps its token timestamps.
const DTW_SLACK_S: f64 = 2.0;

/// Punctuation (or nothing): DTW gives such a token the pause after a word.
fn is_mark(text: &str) -> bool {
    text.trim().chars().all(|c| !c.is_alphanumeric() && c != char::REPLACEMENT_CHARACTER)
}

/// Build `TranscriptSegment`s with Word times from whisper's raw output.
/// Returns the segments and whether any word was timed by DTW.
///
/// With DTW, a token runs from the previous token's DTW time to its own. A
/// span that swallowed a pause is cut to its tail, a word's end may move up
/// to `DTW_END_PAD_S` into a following gap, and punctuation sits at the end of
/// the word before it (or the start of the word after, at a segment's start).
/// A DTW segment is resized to its words: whisper's own segment edges come
/// from the same token-timestamp guesses DTW replaces.
///
/// A token without a usable DTW time (DTW off, unknown model, -1, or far
/// outside its segment) keeps whisper's token timestamps, as before DTW.
///
/// Either way words come out in order (`start <= end <= next start`), inside
/// their segment, segments don't overlap, and nothing passes `audio_end`.
fn assemble_segments(raw: Vec<RawSegment>, audio_end: Option<f64>) -> (Vec<TranscriptSegment>, bool) {
    struct Slot {
        seg: usize,
        text: String,
        p: f64,
        start: f64,
        end: f64,
        dtw: bool,
        mark: bool,
    }
    impl Slot {
        /// DTW punctuation: placed next to a word after the rest are timed.
        fn floating(&self) -> bool {
            self.dtw && self.mark
        }
    }
    let limit = audio_end.filter(|e| e.is_finite() && *e > 0.0).unwrap_or(f64::INFINITY);
    // Each slot starts at or after the previous one ends, inside [0, limit].
    let keep_in_order = |slots: &mut [Slot], skip_floating: bool| {
        let mut prev_end = 0.0_f64;
        for s in slots.iter_mut().filter(|s| !(skip_floating && s.floating())) {
            s.start = s.start.max(prev_end).min(limit);
            s.end = s.end.max(s.start).min(limit);
            prev_end = s.end;
        }
    };

    let raw: Vec<RawSegment> = raw.into_iter().filter(|s| !s.text.trim().is_empty()).collect();

    // 1. Raw spans. `boundary` is where the previous text token ended.
    let mut slots: Vec<Slot> = vec![];
    let mut boundary: Option<f64> = None;
    for (si, seg) in raw.iter().enumerate() {
        for tok in seg.tokens.iter().filter(|t| !t.text.starts_with("[_")) {
            let window = (seg.start - DTW_SLACK_S)..=(seg.end + DTW_SLACK_S);
            let dtw = tok.dtw.filter(|b| b.is_finite() && window.contains(b));
            let (start, end) = match dtw {
                Some(b) => (boundary.unwrap_or(seg.start).min(b), b),
                None => (tok.t0, tok.t1),
            };
            boundary = Some(dtw.unwrap_or(tok.t1));
            slots.push(Slot {
                seg: si,
                text: tok.text.trim().to_string(),
                p: tok.p,
                start,
                end,
                dtw: dtw.is_some(),
                mark: is_mark(&tok.text),
            });
        }
    }
    let used_dtw = slots.iter().any(|s| s.dtw);

    // 2. Cut spans that swallowed a pause down to their tail.
    let mut spans: Vec<f64> = slots
        .iter()
        .filter(|s| s.dtw && !s.mark && s.end > s.start)
        .map(|s| s.end - s.start)
        .collect();
    if !spans.is_empty() {
        spans.sort_by(f64::total_cmp);
        let cap = DTW_MAX_SPAN_MEDIANS * spans[spans.len() / 2].min(DTW_MEDIAN_CEIL_S);
        for s in slots.iter_mut().filter(|s| s.dtw && !s.mark) {
            s.start = s.start.max(s.end - cap);
        }
    }
    keep_in_order(&mut slots, true);

    // 3. DTW word ends move up to DTW_END_PAD_S into a following gap.
    for i in 0..slots.len() {
        if !slots[i].dtw || slots[i].mark {
            continue;
        }
        let next = slots[i + 1..].iter().find(|s| !s.floating()).map_or(limit, |s| s.start);
        slots[i].end = slots[i].end.max((slots[i].end + DTW_END_PAD_S).min(next));
    }

    // 4. Punctuation at the end of the word before it in its segment, or at
    // the start of the word after it.
    for i in 0..slots.len() {
        if !slots[i].floating() {
            continue;
        }
        let seg = slots[i].seg;
        let at = if i > 0 && slots[i - 1].seg == seg {
            Some(slots[i - 1].end)
        } else {
            slots[i + 1..].iter().take_while(|s| s.seg == seg).find(|s| !s.floating()).map(|s| s.start)
        };
        if let Some(t) = at {
            slots[i].start = t;
            slots[i].end = t;
        }
    }
    keep_in_order(&mut slots, false);

    // 5. Group into segments.
    let mut out: Vec<TranscriptSegment> = Vec::with_capacity(raw.len());
    let mut slots = slots.into_iter().peekable();
    for (si, seg) in raw.into_iter().enumerate() {
        let mut words = vec![];
        let mut seg_dtw = false;
        while let Some(s) = slots.next_if(|s| s.seg == si) {
            seg_dtw |= s.dtw;
            words.push(Word { text: s.text, start: s.start, end: s.end, confidence: s.p });
        }
        let (start, end) = match (words.first(), words.last()) {
            (Some(first), Some(last)) if seg_dtw => (first.start, last.end),
            (Some(first), Some(last)) => (seg.start.min(first.start), seg.end.max(last.end)),
            _ => (seg.start, seg.end),
        };
        out.push(TranscriptSegment {
            id: Uuid::new_v4(),
            start: start.max(0.0).min(limit),
            end: end.max(start).min(limit),
            text: seg.text.trim().to_string(),
            words,
            is_filler: false,
            is_silence: false,
            take_group_id: None,
            is_best_take: false,
        });
    }

    // 6. Segments don't overlap; each still holds all its words.
    for i in 1..out.len() {
        let prev_content_end = out[i - 1].words.last().map_or(out[i - 1].start, |w| w.end);
        let first_word = out[i].words.first().map_or(f64::INFINITY, |w| w.start);
        out[i].start = out[i].start.max(prev_content_end).min(first_word);
        out[i].end = out[i].end.max(out[i].start);
        out[i - 1].end = out[i - 1].end.min(out[i].start);
    }
    (out, used_dtw)
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
        let model_path = require_model()?;
        let model = model_path.to_string_lossy().into_owned();
        let out_base = format!("{wav_path}.transcript");
        let mut args: Vec<String> =
            ["-m", model.as_str(), "-f", wav_path, "-ojf", "-of", out_base.as_str()].map(String::from).into();
        if language != "auto" && !language.is_empty() {
            args.extend(["-l".to_string(), language.to_string()]);
        }
        let dtw = match dtw_preset_for_model(&model_path) {
            Some(preset) => cli_dtw_args(preset).await,
            None => vec![],
        };
        let mut out = run_whisper_cli(&args, &dtw).await?;
        if !out.status.success() && !dtw.is_empty() {
            // e.g. alignment heads this model doesn't have: plain timestamps.
            out = run_whisper_cli(&args, &[]).await?;
        }
        if !out.status.success() {
            return Err(CoreError::Other(format!(
                "whisper-cli failed: {}",
                String::from_utf8_lossy(&out.stderr).chars().take(800).collect::<String>()
            )));
        }
        let json = std::fs::read_to_string(format!("{out_base}.json"))?;
        parse_whisper_json(&json, media.id, Some(media.duration))
    }
}

async fn run_whisper_cli(args: &[String], extra: &[String]) -> Result<std::process::Output> {
    Command::new("whisper-cli")
        .args(args)
        .args(extra)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| CoreError::Unavailable(format!("whisper-cli: {e}")))
}

/// The `whisper-cli` flags for DTW word times, if this build has them.
async fn cli_dtw_args(preset: DtwPreset) -> Vec<String> {
    let help = Command::new("whisper-cli")
        .arg("-h")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await;
    match help {
        Ok(h) => {
            let text = format!("{}{}", String::from_utf8_lossy(&h.stdout), String::from_utf8_lossy(&h.stderr));
            cli_dtw_args_from_help(&text, preset)
        }
        Err(_) => vec![],
    }
}

/// `-dtw <preset>`, plus `-nfa` where the build has it: whisper.cpp 1.8 turns
/// flash attention on by default, and flash attention silently turns DTW off.
fn cli_dtw_args_from_help(help: &str, preset: DtwPreset) -> Vec<String> {
    if !help.contains("--dtw") {
        return vec![];
    }
    let mut args = vec!["-dtw".to_string(), preset.cli_name().to_string()];
    if help.contains("--no-flash-attn") {
        args.push("-nfa".to_string());
    }
    args
}

/// Parse whisper.cpp `--output-json-full` output into our model. Token
/// `t_dtw` (centiseconds) is present when whisper-cli ran with `-dtw`.
fn parse_whisper_json(json: &str, media_id: Uuid, audio_end: Option<f64>) -> Result<Transcript> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    let language = v["result"]["language"].as_str().unwrap_or("en").to_string();
    let model_type = v["model"]["type"].as_str().unwrap_or("whisper").to_string();
    let ms = |x: &serde_json::Value| x.as_f64().unwrap_or(0.0) / 1000.0;
    let raw = v["transcription"]
        .as_array()
        .map(|segs| {
            segs.iter()
                .map(|seg| RawSegment {
                    start: ms(&seg["offsets"]["from"]),
                    end: ms(&seg["offsets"]["to"]),
                    text: seg["text"].as_str().unwrap_or("").to_string(),
                    tokens: seg["tokens"]
                        .as_array()
                        .map(|toks| {
                            toks.iter()
                                .map(|tok| RawToken {
                                    text: tok["text"].as_str().unwrap_or("").to_string(),
                                    t0: ms(&tok["offsets"]["from"]),
                                    t1: ms(&tok["offsets"]["to"]),
                                    dtw: tok["t_dtw"].as_i64().and_then(dtw_secs),
                                    p: tok["p"].as_f64().unwrap_or(1.0),
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();
    let (segments, used_dtw) = assemble_segments(raw, audio_end);
    let model_used = if used_dtw { format!("{model_type}+dtw") } else { model_type };
    Ok(Transcript { id: Uuid::new_v4(), media_id, language, segments, model_used })
}

/// In-process whisper.cpp via the `whisper-rs` bindings (feature
/// `whisper-native`). No PATH dependency, no temp JSON, and real progress
/// events through the model's progress callback. Metal/GPU is enabled by the
/// underlying whisper.cpp build where available. Word times use DTW when the
/// model file has a known preset (`model_used` then ends in `+dtw`).
#[cfg(feature = "whisper-native")]
pub struct WhisperRs;

/// A whisper state for `model`, with DTW token timestamps when `preset` is
/// given and the model accepts it. If it doesn't (alignment heads outside the
/// model's layers fail `create_state`), load again without DTW.
#[cfg(feature = "whisper-native")]
fn whisper_state(model: &Path, preset: Option<DtwPreset>) -> Result<whisper_rs::WhisperState> {
    use whisper_rs::{DtwMode, DtwParameters, WhisperContext, WhisperContextParameters};
    if let Some(preset) = preset {
        let mut params = WhisperContextParameters::default();
        params
            .dtw_parameters(DtwParameters {
                mode: DtwMode::ModelPreset { model_preset: preset.native() },
                ..DtwParameters::default()
            })
            // Flash attention silently turns DTW off in whisper.cpp.
            .flash_attn(false);
        if let Ok(state) = WhisperContext::new_with_params(model, params).and_then(|ctx| ctx.create_state()) {
            return Ok(state);
        }
    }
    let ctx = WhisperContext::new_with_params(model, WhisperContextParameters::default())
        .map_err(|e| CoreError::Unavailable(format!("whisper model load: {e}")))?;
    ctx.create_state().map_err(|e| CoreError::Other(format!("whisper state: {e}")))
}

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
            use whisper_rs::{FullParams, SamplingStrategy};

            let mut state = whisper_state(&model, dtw_preset_for_model(&model))?;

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

            let mut raw = vec![];
            for seg in state.as_iter() {
                let text = seg.to_str_lossy().map(|s| s.into_owned()).unwrap_or_default();
                let mut tokens = vec![];
                for i in 0..seg.n_tokens() {
                    let Some(tok) = seg.get_token(i) else { continue };
                    let data = tok.token_data();
                    tokens.push(RawToken {
                        text: tok.to_str_lossy().map(|s| s.into_owned()).unwrap_or_default(),
                        t0: centis(data.t0),
                        t1: centis(data.t1),
                        dtw: dtw_secs(data.t_dtw),
                        p: f64::from(tok.token_probability()),
                    });
                }
                raw.push(RawSegment {
                    start: centis(seg.start_timestamp()),
                    end: centis(seg.end_timestamp()),
                    text,
                    tokens,
                });
            }
            // whisper reads 16 kHz audio.
            let (segments, used_dtw) = assemble_segments(raw, Some(samples.len() as f64 / 16_000.0));

            let detected = whisper_rs::get_lang_str(state.full_lang_id_from_state())
                .map(str::to_string)
                .unwrap_or_else(|| if lang.is_empty() || lang == "auto" { "en".into() } else { lang });
            Ok(Transcript {
                id: Uuid::new_v4(),
                media_id,
                language: detected,
                segments,
                model_used: if used_dtw { format!("{model_name}+dtw") } else { model_name },
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
mod tests {
    use super::*;

    fn tok(text: &str, t0: f64, t1: f64, dtw: Option<f64>) -> RawToken {
        RawToken { text: text.into(), t0, t1, dtw, p: 0.9 }
    }

    /// A token that only has a DTW time (its token timestamps are junk).
    fn dtw(text: &str, at: f64) -> RawToken {
        tok(text, 0.0, 0.0, Some(at))
    }

    fn seg(start: f64, end: f64, tokens: Vec<RawToken>) -> RawSegment {
        let text = tokens.iter().filter(|t| !t.text.starts_with("[_")).map(|t| t.text.as_str()).collect();
        RawSegment { start, end, text, tokens }
    }

    fn words(segs: &[TranscriptSegment]) -> Vec<(String, f64, f64)> {
        segs.iter().flat_map(|s| &s.words).map(|w| (w.text.clone(), w.start, w.end)).collect()
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[track_caller]
    fn assert_word(w: &(String, f64, f64), text: &str, start: f64, end: f64) {
        assert!(w.0 == text && close(w.1, start) && close(w.2, end), "got {w:?}, want ({text:?}, {start}, {end})");
    }

    /// Words in order and inside their segment; segments don't overlap.
    #[track_caller]
    fn assert_well_formed(segs: &[TranscriptSegment]) {
        let mut prev_word_end = f64::NEG_INFINITY;
        let mut prev_seg_end = f64::NEG_INFINITY;
        for s in segs {
            assert!(s.start <= s.end, "segment [{}, {}]", s.start, s.end);
            assert!(s.start >= prev_seg_end, "segment at {} overlaps one ending {prev_seg_end}", s.start);
            for w in &s.words {
                assert!(w.start <= w.end, "word {w:?}");
                assert!(w.start >= prev_word_end, "word {w:?} starts before {prev_word_end}");
                assert!(s.start <= w.start && w.end <= s.end, "word {w:?} outside [{}, {}]", s.start, s.end);
                prev_word_end = w.end;
            }
            prev_seg_end = s.end;
        }
    }

    #[test]
    fn dtw_preset_from_model_file_name() {
        use DtwPreset::*;
        let cases = [
            ("ggml-large-v3-turbo-q5_0.bin", Some(LargeV3Turbo)),
            ("ggml-large-v3-turbo-q8_0.bin", Some(LargeV3Turbo)),
            ("ggml-large-v3-turbo.bin", Some(LargeV3Turbo)),
            ("ggml-large-v3.bin", Some(LargeV3)),
            ("ggml-large-v3-q5_0.bin", Some(LargeV3)),
            ("ggml-large-v2.bin", Some(LargeV2)),
            ("ggml-large-v2-q8_0.bin", Some(LargeV2)),
            ("ggml-large-v1.bin", Some(LargeV1)),
            ("ggml-medium.bin", Some(Medium)),
            ("ggml-medium-q5_0.bin", Some(Medium)),
            ("ggml-medium.en.bin", Some(MediumEn)),
            ("ggml-medium.en-q5_0.bin", Some(MediumEn)),
            ("ggml-small-q5_1.bin", Some(Small)),
            ("ggml-small.bin", Some(Small)),
            ("ggml-small.en.bin", Some(SmallEn)),
            ("ggml-small.en-q8_0.bin", Some(SmallEn)),
            ("ggml-base.bin", Some(Base)),
            ("ggml-base-q5_1.bin", Some(Base)),
            ("ggml-base.en.bin", Some(BaseEn)),
            ("ggml-tiny.bin", Some(Tiny)),
            ("ggml-tiny.en-q5_1.bin", Some(TinyEn)),
            ("ggml-small-q4_k.bin", Some(Small)),
            ("ggml-base-f16.bin", Some(Base)),
            ("GGML-Small.EN.bin", Some(SmallEn)),
            ("/Users/x/Library/Application Support/roughcut/models/ggml-large-v3-turbo-q5_0.bin", Some(LargeV3Turbo)),
            // Not a stock model, or not one name says which: no DTW.
            ("ggml-distil-large-v3.bin", None),
            ("ggml-large.bin", None),
            ("ggml-large-v3-turbo-mine.bin", None),
            ("ggml-small-finetuned-q5_0.bin", None),
            ("ggml-model-whisper-small.bin", None),
            ("model.bin", None),
            ("", None),
        ];
        for (name, want) in cases {
            assert_eq!(dtw_preset_for_model(Path::new(name)), want, "{name}");
        }
        assert_eq!(LargeV3Turbo.cli_name(), "large.v3.turbo");
        assert_eq!(SmallEn.cli_name(), "small.en");
    }

    #[test]
    fn dtw_secs_reads_minus_one_as_missing() {
        assert_eq!(dtw_secs(-1), None);
        assert_eq!(dtw_secs(0), Some(0.0));
        assert_eq!(dtw_secs(523), Some(5.23));
    }

    #[test]
    fn without_dtw_words_keep_token_timestamps() {
        let raw = vec![
            seg(0.0, 1.2, vec![
                tok("[_BEG_]", 0.0, 0.0, None),
                tok(" Hello", 0.1, 0.5, None),
                tok(" there", 0.5, 1.0, None),
                tok(".", 1.0, 1.1, None),
                tok("[_TT_60]", 1.2, 1.2, None),
            ]),
            seg(1.2, 1.4, vec![tok("[_TT_60]", 1.2, 1.2, None)]), // no text: dropped
            seg(1.5, 2.0, vec![tok(" Bye", 1.6, 1.9, None)]),
        ];
        let (segs, used_dtw) = assemble_segments(raw, Some(10.0));
        assert!(!used_dtw);
        assert_eq!(segs.len(), 2);
        let w = words(&segs);
        assert_eq!(w.len(), 4);
        assert_word(&w[0], "Hello", 0.1, 0.5);
        assert_word(&w[1], "there", 0.5, 1.0);
        assert_word(&w[2], ".", 1.0, 1.1);
        assert_word(&w[3], "Bye", 1.6, 1.9);
        assert!(close(segs[0].start, 0.0) && close(segs[0].end, 1.2), "whisper's segment edges stay");
        assert!(close(segs[1].start, 1.5) && close(segs[1].end, 2.0));
        assert_eq!(segs[0].text, "Hello there.");
        assert_well_formed(&segs);
    }

    #[test]
    fn dtw_times_set_word_edges() {
        // Each token ends at its DTW time and starts where the one before ended.
        let raw = vec![seg(0.3, 1.2, vec![
            tok("[_BEG_]", 0.3, 0.3, None),
            tok(" how", 0.3, 0.4, Some(0.60)),
            tok(" are", 0.4, 0.5, Some(0.85)),
            tok(" you", 0.5, 0.6, Some(1.10)),
            tok("[_TT_60]", 1.2, 1.2, None),
        ])];
        let (segs, used_dtw) = assemble_segments(raw, Some(1.15));
        assert!(used_dtw);
        let w = words(&segs);
        assert_word(&w[0], "how", 0.3, 0.6);
        assert_word(&w[1], "are", 0.6, 0.85);
        // The last word gets the end pad, up to the end of the audio.
        assert_word(&w[2], "you", 0.85, 1.15);
        assert!(close(segs[0].start, 0.3) && close(segs[0].end, 1.15));
        assert_well_formed(&segs);
    }

    #[test]
    fn missing_or_bad_dtw_falls_back_per_token() {
        let raw = vec![seg(0.0, 2.0, vec![
            tok(" one", 0.0, 0.4, Some(0.5)),
            tok(" two", 0.5, 0.9, dtw_secs(-1)), // DTW not computed
            tok(" three", 1.0, 1.3, Some(40.0)), // nowhere near its segment
            tok(" four", 1.3, 1.6, Some(1.8)),
        ])];
        let (segs, used_dtw) = assemble_segments(raw, Some(1.85));
        assert!(used_dtw);
        let w = words(&segs);
        assert_word(&w[0], "one", 0.0, 0.5);
        assert_word(&w[1], "two", 0.5, 0.9);
        assert_word(&w[2], "three", 1.0, 1.3);
        // Starts where the token-timestamp word before it ended.
        assert_word(&w[3], "four", 1.3, 1.85);
        assert_well_formed(&segs);
    }

    #[test]
    fn word_times_are_clamped_into_order() {
        // Token timestamps with end < start and overlaps (seen from
        // large-v3-turbo), and a DTW time that runs backwards.
        let raw = vec![
            seg(5.4, 6.6, vec![
                tok(" don", 5.40, 5.19, None),
                tok("'t", 5.40, 5.32, None),
                tok(" make", 5.40, 5.55, None),
                tok(" this", 5.50, 5.80, None),
            ]),
            seg(6.6, 8.0, vec![dtw(" a", 7.0), dtw(" b", 6.8), dtw(" c", 7.4)]),
        ];
        let (segs, _) = assemble_segments(raw, Some(10.0));
        let w = words(&segs);
        assert_word(&w[0], "don", 5.40, 5.40);
        assert_word(&w[1], "'t", 5.40, 5.40);
        assert_word(&w[2], "make", 5.40, 5.55);
        assert_word(&w[3], "this", 5.55, 5.80);
        assert_word(&w[5], "b", 7.0, 7.0);
        assert_well_formed(&segs);
    }

    #[test]
    fn dtw_word_can_end_after_whispers_segment_end() {
        // large-v3-turbo on "...because I didn't use AI. Don't make...":
        // whisper's segment and "AI" token both end at 5.00 s, but "AI" is
        // heard until about 5.3 s. DTW says 5.24; the end pad takes it to
        // 5.34 and the segment grows to hold it, instead of cutting it off.
        let raw = vec![
            seg(0.0, 5.0, vec![
                tok(" didn", 4.00, 4.36, Some(4.44)),
                tok("'t", 4.36, 4.54, Some(4.66)),
                tok(" use", 4.54, 4.81, Some(4.98)),
                tok(" AI", 4.81, 5.00, Some(5.24)),
                tok(".", 5.00, 5.00, Some(5.54)),
                tok("[_TT_250]", 5.0, 5.0, None),
            ]),
            seg(5.42, 6.56, vec![
                tok(" Don", 5.42, 5.23, Some(5.56)),
                tok("'t", 5.42, 5.38, Some(5.62)),
                tok(" make", 5.65, 5.69, Some(5.80)),
            ]),
        ];
        let (segs, _) = assemble_segments(raw, Some(8.0));
        let w = words(&segs);
        assert_word(&w[2], "use", 4.66, 4.98);
        assert_word(&w[3], "AI", 4.98, 5.34);
        // The period sits at the end of "AI", not across the pause.
        assert_word(&w[4], ".", 5.34, 5.34);
        assert_word(&w[5], "Don", 5.54, 5.56);
        assert!(close(segs[0].end, 5.34), "segment end {}", segs[0].end);
        assert!(close(segs[1].start, 5.54), "segment start {}", segs[1].start);
        // "didn" ran from the segment start (0 s): cut to 2 x the median span.
        assert_word(&w[0], "didn", 4.0, 4.44);
        assert_well_formed(&segs);
    }

    #[test]
    fn a_pause_inside_a_dtw_span_stays_a_gap() {
        // The model waits on " later" through 2.25 s of silence; the cap keeps
        // only the last 2 x median (0.5 s) of that span.
        let raw = vec![seg(0.75, 4.3, vec![
            dtw(" one", 1.0),
            dtw(" two", 1.25),
            dtw(" three", 1.5),
            dtw(" four", 1.75),
            dtw(" later", 4.0),
            dtw(" on", 4.25),
        ])];
        let (segs, _) = assemble_segments(raw, None);
        let w = words(&segs);
        assert_word(&w[0], "one", 0.75, 1.0);
        assert_word(&w[3], "four", 1.5, 1.85); // end pad into the pause
        assert_word(&w[4], "later", 3.5, 4.0);
        assert_word(&w[5], "on", 4.0, 4.35);
        assert_well_formed(&segs);
    }

    #[test]
    fn dtw_punctuation_takes_no_time() {
        let raw = vec![
            seg(0.0, 1.0, vec![dtw(" hi", 0.5)]),
            seg(1.0, 2.0, vec![dtw(" \"", 1.2), dtw("Yes", 1.5), dtw(".", 1.6), dtw("\"", 1.7)]),
        ];
        let (segs, _) = assemble_segments(raw, Some(2.0));
        let w = words(&segs);
        assert_word(&w[0], "hi", 0.0, 0.6);
        // A leading quote sits at the start of the word after it.
        assert_word(&w[1], "\"", 1.2, 1.2);
        assert_word(&w[2], "Yes", 1.2, 1.6);
        assert_word(&w[3], ".", 1.6, 1.6);
        assert_word(&w[4], "\"", 1.6, 1.6);
        assert!(close(segs[1].start, 1.2) && close(segs[1].end, 1.6));
        assert_well_formed(&segs);
    }

    #[test]
    fn segments_never_overlap() {
        // A token-timestamp segment whose edge runs past where the next
        // (DTW) segment's first word starts.
        let raw = vec![
            seg(0.0, 2.0, vec![tok(" first", 0.0, 1.0, None)]),
            seg(2.0, 3.0, vec![tok(" second", 2.0, 2.5, Some(1.6))]),
        ];
        let (segs, _) = assemble_segments(raw, Some(3.0));
        let w = words(&segs);
        assert_word(&w[1], "second", 1.0, 1.7);
        assert!(close(segs[0].end, 1.0) && close(segs[1].start, 1.0));
        assert_well_formed(&segs);
    }

    #[test]
    fn cli_json_uses_t_dtw_when_present() {
        let json = r#"{
            "model": {"type": "small"},
            "result": {"language": "en"},
            "transcription": [{
                "offsets": {"from": 0, "to": 1000},
                "text": " Hi there",
                "tokens": [
                    {"text": "[_BEG_]", "offsets": {"from": 0, "to": 0}, "p": 0.9, "t_dtw": -1},
                    {"text": " Hi", "offsets": {"from": 100, "to": 300}, "p": 0.8, "t_dtw": 40},
                    {"text": " there", "offsets": {"from": 300, "to": 600}, "p": 0.7, "t_dtw": 70}
                ]
            }]
        }"#;
        let t = parse_whisper_json(json, Uuid::nil(), Some(10.0)).unwrap();
        assert_eq!(t.model_used, "small+dtw");
        let w = words(&t.segments);
        assert_word(&w[0], "Hi", 0.0, 0.4);
        assert_word(&w[1], "there", 0.4, 0.8);
        assert!((w.len(), t.segments[0].words[0].confidence) == (2, 0.8));

        // Older whisper-cli, or no -dtw: no t_dtw, token timestamps as before.
        let plain = json.replace(r#", "t_dtw": 40"#, "").replace(r#", "t_dtw": 70"#, "");
        let t = parse_whisper_json(&plain, Uuid::nil(), Some(10.0)).unwrap();
        assert_eq!(t.model_used, "small");
        let w = words(&t.segments);
        assert_word(&w[0], "Hi", 0.1, 0.3);
        assert_word(&w[1], "there", 0.3, 0.6);
    }

    #[test]
    fn cli_dtw_flags_follow_the_help_text() {
        let v18 = "  -dtw MODEL --dtw MODEL [] compute token-level timestamps\n  -nfa, --no-flash-attn [false] disable flash attention";
        assert_eq!(cli_dtw_args_from_help(v18, DtwPreset::LargeV3Turbo), ["-dtw", "large.v3.turbo", "-nfa"]);
        let v17 = "  -dtw MODEL --dtw MODEL [] compute token-level timestamps\n  -fa, --flash-attn [false] flash attention";
        assert_eq!(cli_dtw_args_from_help(v17, DtwPreset::Small), ["-dtw", "small"]);
        assert!(cli_dtw_args_from_help("usage: whisper-cli [options] file0.wav", DtwPreset::Small).is_empty());
    }
}
