//! Transcribe a 16 kHz mono WAV with the in-process engine (`WhisperRs`) and
//! print each segment and word with its times, to check word timing against
//! the audio. The model is the app's (`WHISPER_MODEL` overrides it).
//! Build with `--features whisper-native`.
//! Usage: transcribe_words <file.wav> [from_s] [to_s] [language]

#[cfg(feature = "whisper-native")]
#[tokio::main]
async fn main() {
    use roughcut_core::adapters::transcribe::{resolve_whisper_model, Transcriber, WhisperRs};
    use roughcut_core::events::{NullSink, SharedSink};
    use roughcut_core::model::Media;
    use std::sync::Arc;

    let args: Vec<String> = std::env::args().collect();
    let wav = args.get(1).expect("usage: transcribe_words <file.wav> [from_s] [to_s] [language]");
    let from: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let to: f64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(f64::INFINITY);
    let language = args.get(4).map(String::as_str).unwrap_or("en");
    println!("model: {:?}", resolve_whisper_model());

    let media = Media {
        id: uuid::Uuid::new_v4(),
        file_path: wav.clone(),
        duration: 0.0,
        frame_rate: 0.0,
        width: 0,
        height: 0,
        audio_sample_rate: 16_000,
        codec: "pcm_s16le".into(),
        imported_at: chrono::Utc::now(),
    };
    let sink: SharedSink = Arc::new(NullSink);
    let t0 = std::time::Instant::now();
    let t = WhisperRs.transcribe(&media, wav, language, None, &sink).await.expect("transcribe");
    println!("model_used: {}  ({:.1}s)", t.model_used, t0.elapsed().as_secs_f64());
    for seg in t.segments.iter().filter(|s| s.end >= from && s.start <= to) {
        println!("{:8.2} {:8.2}  {}", seg.start, seg.end, seg.text);
        for w in &seg.words {
            println!("    {:8.2} {:8.2}  {}", w.start, w.end, w.text);
        }
    }
}

#[cfg(not(feature = "whisper-native"))]
fn main() {
    eprintln!("transcribe_words needs --features whisper-native");
    std::process::exit(2);
}
