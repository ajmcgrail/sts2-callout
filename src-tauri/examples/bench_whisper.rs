//! Times one recognition pass, e.g.
//! `cargo run --release --example bench_whisper -- ggml-base.en.bin 5`
use std::time::Instant;

use sts2_callout_lib::stt::{thread_count, whisper_params, RATE};
use whisper_rs::{WhisperContext, WhisperContextParameters};

fn main() {
    let mut args = std::env::args().skip(1);
    let model = args.next().expect("usage: bench_whisper <model.bin> [seconds]");
    let secs: f32 = args.next().map_or(5.0, |s| s.parse().unwrap());
    whisper_rs::install_logging_hooks();
    let ctx = WhisperContext::new_with_params(&model, WhisperContextParameters::default()).unwrap();
    let mut state = ctx.create_state().unwrap();
    // Noise stands in for speech; the encoder cost doesn't depend on content.
    let audio: Vec<f32> = (0..(secs * RATE as f32) as usize).map(|i| ((i * 7919 % 1000) as f32 / 1000.0 - 0.5) * 0.2).collect();
    println!("threads: {}", thread_count());
    for mode in ["best", "balanced", "fast"] {
        for run in 0..2 {
            let t = Instant::now();
            state.full(whisper_params(thread_count(), audio.len(), mode, false), &audio).unwrap();
            println!("{mode} run {run}: {} ms", t.elapsed().as_millis());
        }
    }
}
