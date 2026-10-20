//! Prints VAD speech segments for raw 16 kHz mono f32 audio, and how long
//! detection takes for different buffer lengths:
//! `cargo run --release --example vad_check -- ggml-silero-v6.2.0.bin audio.f32`
use std::time::Instant;

use whisper_rs::{WhisperVadContext, WhisperVadContextParams, WhisperVadParams};

fn main() {
    let mut args = std::env::args().skip(1);
    let (model, file) = (args.next().unwrap(), args.next().unwrap());
    let bytes = std::fs::read(file).unwrap();
    let audio: Vec<f32> = bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
    let mut params = WhisperVadContextParams::default();
    params.set_n_threads(1);
    params.set_use_gpu(false);
    let mut ctx = WhisperVadContext::new(&model, params).unwrap();
    for secs in [0.5f32, 2.0, 8.0] {
        let n = ((secs * 16000.0) as usize).min(audio.len());
        let t = Instant::now();
        for _ in 0..5 {
            ctx.detect_speech(&audio[..n]).unwrap();
        }
        println!("{secs} s of audio: {:.1} ms per detect_speech", t.elapsed().as_secs_f64() * 1000.0 / 5.0);
    }
    for s in ctx.segments_from_samples(WhisperVadParams::new(), &audio).unwrap() {
        println!("speech {:.2}s – {:.2}s", s.start / 100.0, s.end / 100.0);
    }
}
