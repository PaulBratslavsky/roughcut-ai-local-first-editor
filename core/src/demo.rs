//! Fixture data for demo mode and tests: a believable talking-head recording
//! with filler words, silence gaps, and a repeated take — enough to exercise
//! the whole edit loop without ffmpeg/whisper installed.

use crate::model::{Media, Transcript, TranscriptSegment, Word};
use uuid::Uuid;

const SCRIPT: &[&str] = &[
    "Hey everyone, welcome back to the channel.",
    "um",
    "Today I want to talk about something that completely changed how I edit videos.",
    "So the problem is that editing takes forever, right?",
    "uh",
    "You record twenty minutes of footage and then you spend three hours cutting silences and bad takes.",
    "[silence]",
    "Let me show you what I mean with a real example.",
    "Let me show you what I mean with an actual example from last week.",
    "First, you import your footage, and the app transcribes it entirely on your own machine.",
    "um",
    "Nothing gets uploaded anywhere, which matters if you're under NDA or just value your privacy.",
    "[silence]",
    "Then the AI does a first pass: silences gone, filler words gone, best takes kept.",
    "And here's the key part, you stay in control.",
    "Every cut is just a clip with handles you can drag, frame by frame.",
    "uh",
    "You can also just edit the transcript like a document, delete a sentence and the video cuts to match.",
    "Or you can literally chat with your footage.",
    "Tell it to cut the tangent about your weekend and it finds it and cuts it.",
    "[silence]",
    "Speaking of weekends, I went hiking on Saturday and the weather was honestly perfect the whole time.",
    "Anyway, back to the editor.",
    "When you're done you export straight to Premiere, Final Cut, or Resolve and finish there.",
    "um",
    "If this kind of local-first software matters to you, the whole thing is open source.",
    "Links are in the description, and I'll see you in the next one.",
];

/// Build the fixture transcript across roughly the media's duration.
pub fn fixture_transcript(media: &Media) -> Transcript {
    let total = if media.duration > 0.0 { media.duration } else { 300.0 };
    // Weight lines by word count; silences get a fixed weight.
    let weights: Vec<f64> = SCRIPT
        .iter()
        .map(|l| {
            if *l == "[silence]" {
                4.0
            } else {
                l.split_whitespace().count().max(1) as f64
            }
        })
        .collect();
    let weight_sum: f64 = weights.iter().sum();
    let scale = (total * 0.985) / weight_sum;

    let mut t = 0.4_f64;
    let mut segments = vec![];
    let mut take_pair: Vec<usize> = vec![];

    for (i, line) in SCRIPT.iter().enumerate() {
        let dur = weights[i] * scale;
        if *line == "[silence]" {
            segments.push(TranscriptSegment {
                id: Uuid::new_v4(),
                start: t,
                end: t + dur,
                text: String::new(),
                words: vec![],
                is_filler: false,
                is_silence: true,
                take_group_id: None,
                is_best_take: false,
            });
        } else {
            let words_text: Vec<&str> = line.split_whitespace().collect();
            let per = dur / words_text.len() as f64;
            let words: Vec<Word> = words_text
                .iter()
                .enumerate()
                .map(|(wi, w)| Word {
                    text: (*w).to_string(),
                    start: t + wi as f64 * per,
                    end: t + (wi as f64 + 0.92) * per,
                    confidence: 0.97,
                })
                .collect();
            let is_filler = matches!(*line, "um" | "uh");
            if line.starts_with("Let me show you what I mean") {
                take_pair.push(segments.len());
            }
            segments.push(TranscriptSegment {
                id: Uuid::new_v4(),
                start: t,
                end: t + dur,
                text: (*line).to_string(),
                words,
                is_filler,
                is_silence: false,
                take_group_id: None,
                is_best_take: false,
            });
        }
        t += dur;
    }

    // Mark the repeated-take pair; the later take wins (creators redo until good).
    if take_pair.len() == 2 {
        let group = Uuid::new_v4();
        for (n, idx) in take_pair.iter().enumerate() {
            segments[*idx].take_group_id = Some(group);
            segments[*idx].is_best_take = n == take_pair.len() - 1;
        }
    }

    Transcript {
        id: Uuid::new_v4(),
        media_id: media.id,
        language: "en".into(),
        segments,
        model_used: "demo-fixture".into(),
    }
}
