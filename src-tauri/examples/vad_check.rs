//! Prints VAD speech segments for raw 16 kHz mono f32 audio:
//! `cargo run --example vad_check -- ggml-silero-v6.2.0.bin audio.f32`
use whisper_rs::{WhisperVadContext, WhisperVadContextParams, WhisperVadParams};

fn main() {
    let mut args = std::env::args().skip(1);
    let (model, file) = (args.next().unwrap(), args.next().unwrap());
    let bytes = std::fs::read(file).unwrap();
    let audio: Vec<f32> = bytes.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect();
    let mut ctx = WhisperVadContext::new(&model, WhisperVadContextParams::default()).unwrap();
    ctx.detect_speech(&audio).unwrap();
    let probs = ctx.probabilities();
    let per = audio.len() as f32 / probs.len() as f32 / 16000.0;
    let line: String = probs.iter().map(|p| if *p > 0.5 { '#' } else if *p > 0.2 { '+' } else { '.' }).collect();
    println!("{} probs, {:.0} ms each\n{line}", probs.len(), per * 1000.0);
    for s in ctx.segments_from_samples(WhisperVadParams::new(), &audio).unwrap() {
        println!("speech {:.2}s – {:.2}s", s.start / 100.0, s.end / 100.0);
    }
}
