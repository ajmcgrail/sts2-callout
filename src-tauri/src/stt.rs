//! Speech recognition loop: audio -> 16 kHz -> phrases (VAD) -> whisper -> matcher -> overlay.
//!
//! Whisper is far more accurate on whole phrases than on fixed slices that cut
//! words in half, so a Silero voice-activity model splits the audio at pauses.
//! Without the VAD model it falls back to fixed overlapping chunks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
    WhisperVadContext, WhisperVadContextParams, WhisperVadParams,
};

use crate::matcher::{normalize, ItemRef};
use crate::settings::Settings;
use crate::{audio, AppState};

pub const RATE: usize = 16_000;
const MS: usize = RATE / 1000;
/// Overlap kept between fixed chunks / forced phrase cuts so split names are still heard.
const OVERLAP: usize = RATE;
/// Silence that ends a phrase.
const PAUSE: usize = 500 * MS;
/// Audio kept around detected speech.
const PAD: usize = 200 * MS;
/// Run the VAD after this much new audio.
const VAD_STEP: usize = 300 * MS;

#[derive(Serialize, Clone, Copy)]
pub struct ModelInfo {
    pub name: &'static str,
    pub label: &'static str,
}

pub const MODELS: &[ModelInfo] = &[
    ModelInfo { name: "tiny.en", label: "tiny.en (75 MB, fastest, rough)" },
    ModelInfo { name: "base.en", label: "base.en (142 MB, fast)" },
    ModelInfo { name: "small.en-q5_1", label: "small.en-q5_1 (181 MB, recommended)" },
    ModelInfo { name: "small.en", label: "small.en (466 MB)" },
    ModelInfo { name: "medium.en-q5_0", label: "medium.en-q5_0 (514 MB, best, needs a fast CPU)" },
    ModelInfo { name: "large-v3-turbo-q5_0", label: "large-v3-turbo-q5_0 (547 MB, best, needs a fast CPU)" },
];

/// `STS2_MODEL_BASE` overrides where models are downloaded from (testing / mirrors).
fn model_base(default: &str) -> String {
    std::env::var("STS2_MODEL_BASE").unwrap_or_else(|_| default.to_string())
}

pub fn model_url(name: &str) -> String {
    format!("{}/ggml-{name}.bin", model_base("https://huggingface.co/ggerganov/whisper.cpp/resolve/main"))
}

pub fn model_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("ggml-{name}.bin"))
}

const VAD_MODEL: &str = "silero-v6.2.0";

pub fn vad_url() -> String {
    format!("{}/ggml-{VAD_MODEL}.bin", model_base("https://huggingface.co/ggml-org/whisper-vad/resolve/main"))
}

pub fn vad_path(dir: &Path) -> PathBuf {
    dir.join(format!("ggml-{VAD_MODEL}.bin"))
}

pub struct Listener {
    stop: Arc<AtomicBool>,
    pub device_label: String,
}

impl Listener {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Clone, Serialize)]
struct Transcript {
    text: String,
    matches: Vec<String>,
    took_ms: u128,
    audio_ms: usize,
    /// Decoded twice because the first pass was unsure.
    retried: bool,
}

/// Mean word probability below which a phrase gets a second decode.
const RETRY_BELOW: f32 = 0.6;

/// Run whisper; returns the text (annotations removed) and its mean token probability.
fn decode(
    wstate: &mut WhisperState,
    audio: &[f32],
    accuracy: &str,
    beam: bool,
    stop: &Arc<AtomicBool>,
) -> Option<(String, f32)> {
    let mut params = whisper_params(thread_count(), audio.len(), accuracy, beam);
    // Lets Stop interrupt a decode in progress (seconds long with big models).
    let abort = stop.clone();
    params.set_abort_callback_safe(move || abort.load(Ordering::Relaxed));
    if let Err(e) = wstate.full(params, audio) {
        if !stop.load(Ordering::Relaxed) {
            log::warn!("whisper failed on phrase: {e}");
        }
        return None;
    }
    let (mut text, mut prob_sum, mut n) = (String::new(), 0.0f32, 0usize);
    for seg in wstate.as_iter() {
        if seg.no_speech_probability() > 0.8 {
            continue;
        }
        if let Ok(t) = seg.to_str_lossy() {
            text.push_str(&t);
            text.push(' ');
        }
        for i in 0..seg.n_tokens() {
            let Some(tok) = seg.get_token(i) else { continue };
            let special = tok.to_str_lossy().is_ok_and(|t| t.starts_with("[_") || t.starts_with("<|"));
            if !special {
                prob_sum += tok.token_probability();
                n += 1;
            }
        }
    }
    let text = strip_annotations(&text).trim().to_string();
    Some((text, if n == 0 { 1.0 } else { prob_sum / n as f32 }))
}

/// Loads the models and starts capture + recognition. Blocking (model load).
pub fn spawn(app: AppHandle, gen: u64, model: &Path, vad_model: Option<&Path>, device: Option<String>) -> Result<Listener> {
    whisper_rs::install_logging_hooks();
    let ctx = WhisperContext::new_with_params(model, WhisperContextParameters::default())
        .map_err(|e| anyhow!("loading whisper model {}: {e}", model.display()))?;
    let vad = vad_model.and_then(|p| {
        let mut params = WhisperVadContextParams::default();
        params.set_n_threads(1);
        params.set_use_gpu(false);
        match WhisperVadContext::new(&p.to_string_lossy(), params) {
            Ok(v) => Some(v),
            Err(e) => {
                log::warn!("VAD model failed to load ({e}); using fixed chunks");
                None
            }
        }
    });
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<Vec<f32>>();
    let cap = audio::start(device, tx, stop.clone())?;
    let label = cap.label.clone();

    let stop2 = stop.clone();
    std::thread::Builder::new().name("speech".into()).spawn(move || {
        let result = run(&app, ctx, vad, rx, cap.sample_rate, &stop2);
        let error = match result {
            Err(e) => Some(format!("Speech recognition stopped: {e:#}")),
            // The audio thread ended without being asked to (device unplugged / failed).
            Ok(()) if !stop2.load(Ordering::Relaxed) => Some("The audio device was disconnected or changed. Click Start listening to continue.".to_string()),
            Ok(()) => None,
        };
        if let Some(e) = &error {
            log::error!("{e}");
        }
        stop2.store(true, Ordering::Relaxed);
        crate::listener_exited(&app, gen, error);
    })?;
    Ok(Listener { stop, device_label: label })
}

fn run(
    app: &AppHandle,
    ctx: WhisperContext,
    mut vad: Option<WhisperVadContext>,
    rx: mpsc::Receiver<Vec<f32>>,
    in_rate: u32,
    stop: &Arc<AtomicBool>,
) -> Result<()> {
    let mut wstate = ctx.create_state().map_err(|e| anyhow!("whisper state: {e}"))?;
    let mut resampler = Resampler::new(in_rate as f64 / RATE as f64);
    let mut segmenter = Segmenter::default();
    let mut since_vad = 0usize;
    let mut last_shown: HashMap<ItemRef, Instant> = HashMap::new();
    let mut last_level = Instant::now();
    let state = app.state::<AppState>();
    let mut last_real = Instant::now();
    let mut filled = 0usize;
    let _ = app.emit(
        "status",
        if vad.is_some() { "Splitting speech into phrases (voice activity detection)" } else { "Voice activity model unavailable, using fixed chunks" },
    );

    while !stop.load(Ordering::Relaxed) {
        let mut fresh = Vec::new();
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(buf) => {
                resampler.process(&buf, &mut fresh);
                last_real = Instant::now();
                filled = 0;
            }
            Err(RecvTimeoutError::Timeout) => {
                // WASAPI loopback sends nothing while nothing plays. Feed the
                // silence ourselves so a phrase followed by quiet still ends.
                let due = (last_real.elapsed().as_secs_f32() * RATE as f32) as usize;
                if due < 300 * MS || filled >= 2 * RATE {
                    continue;
                }
                fresh = vec![0.0; due - filled];
                filled = due;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
        while let Ok(buf) = rx.try_recv() {
            resampler.process(&buf, &mut fresh);
        }
        since_vad += fresh.len();
        segmenter.buf.extend_from_slice(&fresh);

        if last_level.elapsed() > Duration::from_millis(250) {
            let _ = app.emit("level", rms(&fresh));
            last_level = Instant::now();
        }

        let settings = state.settings.lock().clone();
        let max_len = (settings.max_phrase_secs.clamp(2.0, 20.0) * RATE as f32) as usize;

        let phrase = if let Some(vad) = vad.as_mut() {
            if since_vad < VAD_STEP {
                continue;
            }
            since_vad = 0;
            let speech = detect_speech(vad, &segmenter.buf);
            segmenter.next_phrase(&speech, max_len)
        } else {
            segmenter.next_chunk(max_len)
        };
        let Some(mut phrase) = phrase else { continue };

        // Fell behind (slow CPU / big model): drop the backlog instead of lagging further.
        if segmenter.buf.len() > max_len * 2 {
            let drop = segmenter.buf.len() - max_len;
            segmenter.buf.drain(..drop);
        }
        if rms(&phrase) < settings.silence_threshold {
            continue;
        }
        // whisper.cpp wants at least 1 s of audio.
        if phrase.len() < RATE + 100 * MS {
            phrase.resize(RATE + 100 * MS, 0.0);
        }
        transcribe(app, &state, &settings, &mut wstate, &phrase, &mut last_shown, stop);
    }
    Ok(())
}

fn transcribe(
    app: &AppHandle,
    state: &AppState,
    settings: &Settings,
    wstate: &mut WhisperState,
    audio: &[f32],
    last_shown: &mut HashMap<ItemRef, Instant>,
    stop: &Arc<AtomicBool>,
) {
    let started = Instant::now();
    let Some((text, confidence)) = decode(wstate, audio, &settings.accuracy, false, stop) else { return };
    log::debug!("recognized {text:?} (confidence {confidence:.2}) from {} ms in {} ms", audio.len() / MS, started.elapsed().as_millis());

    // Overlapping voices show up as low confidence. If there's time, decode again
    // with beam search and look for names in both versions.
    let audio_ms = (audio.len() / MS) as u128;
    let mut versions = vec![text];
    let mut retried = false;
    if confidence < RETRY_BELOW && settings.accuracy != "best" && started.elapsed().as_millis() * 4 < audio_ms {
        if let Some((again, _)) = decode(wstate, audio, &settings.accuracy, true, stop) {
            log::debug!("retry: {again:?}");
            if normalize(&again) != normalize(&versions[0]) {
                versions.push(again);
            }
            retried = true;
        }
    }
    versions.retain(|t| !t.is_empty());
    if versions.is_empty() {
        return;
    }
    let mut shown = Vec::new();
    if let Some(matcher) = state.matcher.read().clone() {
        let cooldown = Duration::from_secs_f32(settings.cooldown_secs.max(0.0));
        for m in versions.iter().flat_map(|t| matcher.find(t)) {
            if last_shown.get(&m.item).is_some_and(|t| t.elapsed() < cooldown) {
                continue;
            }
            if crate::emit_show(app, &m.item, &m.heard) {
                last_shown.insert(m.item.clone(), Instant::now());
                shown.push(m.heard.clone());
            }
        }
    }
    let _ = app.emit(
        "transcript",
        Transcript {
            text: versions.join(" ⟲ "),
            matches: shown,
            took_ms: started.elapsed().as_millis(),
            retried,
            audio_ms: audio.len() / MS,
        },
    );
}

/// Speech regions in `audio` as sample ranges.
fn detect_speech(vad: &mut WhisperVadContext, audio: &[f32]) -> Vec<(usize, usize)> {
    let mut params = WhisperVadParams::new();
    params.set_threshold(0.5);
    params.set_min_speech_duration(150);
    params.set_min_silence_duration(150);
    params.set_speech_pad(0);
    match vad.segments_from_samples(params, audio) {
        // timestamps are in centiseconds
        Ok(segs) => segs
            .map(|s| ((s.start.max(0.0) * 160.0) as usize, ((s.end * 160.0) as usize).min(audio.len())))
            .filter(|(a, b)| b > a)
            .collect(),
        Err(e) => {
            log::warn!("VAD failed: {e}");
            Vec::new()
        }
    }
}

/// Holds not-yet-transcribed audio and decides when a phrase is complete.
#[derive(Default)]
struct Segmenter {
    buf: Vec<f32>,
}

impl Segmenter {
    /// `speech`: speech regions of `buf`. Returns a finished phrase, if any.
    fn next_phrase(&mut self, speech: &[(usize, usize)], max_len: usize) -> Option<Vec<f32>> {
        let len = self.buf.len();
        let Some(&(first, _)) = speech.first() else {
            // No speech: keep a little tail so a phrase onset isn't lost.
            let keep = len.min(PAD);
            self.buf.drain(..len - keep);
            return None;
        };
        let start = first.saturating_sub(PAD);

        // The first pause (between segments, or after the last one) ends a phrase.
        // Audio can arrive in bursts, so the pause isn't necessarily at the end.
        for (i, &(_, end)) in speech.iter().enumerate() {
            let next_start = speech.get(i + 1).map_or(len, |s| s.0);
            if next_start.saturating_sub(end) >= PAUSE {
                let cut = (end + PAD).min(next_start);
                let phrase = self.buf[start..cut].to_vec();
                self.buf.drain(..cut);
                return Some(phrase);
            }
        }
        // Long continuous talking: cut at the latest gap, else hard-cut with overlap.
        if len - start >= max_len {
            let gap = speech
                .windows(2)
                .filter(|w| w[1].0 > w[0].1 && w[1].0 - w[0].1 >= 100 * MS && w[0].1 > start + RATE)
                .map(|w| (w[0].1 + w[1].0) / 2)
                .next_back();
            let (cut, resume) = match gap {
                Some(g) => (g, g),
                None => (len, len.saturating_sub(OVERLAP)),
            };
            let phrase = self.buf[start..cut].to_vec();
            self.buf.drain(..resume);
            return Some(phrase);
        }
        // Still talking; drop leading silence so the buffer doesn't grow.
        self.buf.drain(..start);
        None
    }

    /// Fallback without VAD: fixed chunks with overlap.
    fn next_chunk(&mut self, len: usize) -> Option<Vec<f32>> {
        if self.buf.len() < len + OVERLAP {
            return None;
        }
        let chunk = self.buf[..len + OVERLAP].to_vec();
        self.buf.drain(..len);
        Some(chunk)
    }
}

/// Recognition settings for an accuracy mode:
/// - "fast": encoder sized to the phrase (whisper otherwise always encodes 30 s)
/// - "balanced": encoder sized with plenty of headroom
/// - "best": full 30 s encoder and beam search
pub fn whisper_params(threads: i32, n_samples: usize, accuracy: &str, beam: bool) -> FullParams<'static, 'static> {
    let mut params = if beam || accuracy == "best" {
        FullParams::new(SamplingStrategy::BeamSearch { beam_size: 4, patience: -1.0 })
    } else {
        FullParams::new(SamplingStrategy::Greedy { best_of: 1 })
    };
    params.set_language(Some("en"));
    params.set_n_threads(threads);
    params.set_no_context(true);
    params.set_no_timestamps(true);
    params.set_single_segment(true);
    params.set_suppress_blank(true);
    params.set_suppress_nst(true);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_initial_prompt("Slay the Spire 2. Ironclad, Silent, Defect, Necrobinder, Regent. Cards and relics.");
    // 50 encoder positions per second of audio; 1500 = 30 s.
    let needed = n_samples / (RATE / 50);
    let ctx = match accuracy {
        "fast" => Some(needed + 64),
        "balanced" => Some(needed * 2 + 256),
        _ => None,
    };
    if let Some(ctx) = ctx {
        params.set_audio_ctx((ctx.div_ceil(64) * 64).min(1500) as i32);
    }
    params
}

pub fn thread_count() -> i32 {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8) as i32
}

/// Whisper marks non-speech as "[Music]", "(laughs)", "*sigh*" – drop those.
fn strip_annotations(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0i32;
    for ch in s.chars() {
        match ch {
            '[' | '(' | '{' => depth += 1,
            ']' | ')' | '}' => depth = (depth - 1).max(0),
            '*' | '♪' => {}
            c if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn rms(s: &[f32]) -> f32 {
    if s.is_empty() {
        return 0.0;
    }
    (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt()
}

/// Streaming windowed-sinc resampler. The low-pass stops content above the
/// new Nyquist frequency (8 kHz) from aliasing into the speech band.
struct Resampler {
    step: f64,
    cutoff: f64,
    half: usize,
    buf: Vec<f32>,
    pos: f64,
}

impl Resampler {
    fn new(ratio: f64) -> Self {
        let half = (16.0 * ratio.max(1.0)).ceil() as usize;
        Self {
            step: ratio,
            // cycles per input sample, a little under the output Nyquist
            cutoff: 0.5 * 0.92 / ratio.max(1.0),
            half,
            buf: vec![0.0; half],
            pos: half as f64,
        }
    }

    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.buf.extend_from_slice(input);
        let half = self.half as isize;
        while (self.pos.floor() as isize + half) < self.buf.len() as isize {
            let center = self.pos.floor() as isize;
            let frac = self.pos - center as f64;
            let (mut acc, mut norm) = (0.0f64, 0.0f64);
            for k in (center - half + 1)..=(center + half) {
                let d = k as f64 - center as f64 - frac;
                let u = d / half as f64;
                if u.abs() >= 1.0 {
                    continue;
                }
                let x = 2.0 * self.cutoff * d;
                let sinc = if x.abs() < 1e-9 { 1.0 } else { (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x) };
                let pi_u = std::f64::consts::PI * u;
                let w = 0.42 + 0.5 * pi_u.cos() + 0.08 * (2.0 * pi_u).cos(); // Blackman
                let h = sinc * w;
                acc += self.buf[k as usize] as f64 * h;
                norm += h;
            }
            out.push((acc / norm) as f32);
            self.pos += self.step;
        }
        let consumed = (self.pos.floor() as usize).saturating_sub(self.half);
        self.buf.drain(..consumed);
        self.pos -= consumed as f64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, rate: f64, secs: f64) -> Vec<f32> {
        (0..(rate * secs) as usize).map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / rate).sin() as f32).collect()
    }

    #[test]
    fn resampler_rate_and_dc() {
        for rate in [48_000.0, 44_100.0, 16_000.0, 8_000.0] {
            let mut r = Resampler::new(rate / 16_000.0);
            let mut out = Vec::new();
            for _ in 0..100 {
                r.process(&vec![0.5; (rate / 100.0) as usize], &mut out);
            }
            assert!((out.len() as i64 - 16_000).abs() <= 80, "{rate}: {}", out.len());
            assert!(out[100..].iter().all(|x| (x - 0.5).abs() < 1e-3), "{rate}");
        }
    }

    #[test]
    fn resampler_filters_aliases() {
        let mut out = Vec::new();
        let mut r = Resampler::new(3.0);
        r.process(&sine(1_000.0, 48_000.0, 1.0), &mut out);
        assert!((rms(&out[200..]) - 0.707).abs() < 0.02, "speech band kept: {}", rms(&out[200..]));
        let mut out = Vec::new();
        let mut r = Resampler::new(3.0);
        r.process(&sine(12_000.0, 48_000.0, 1.0), &mut out);
        assert!(rms(&out[200..]) < 0.01, "12 kHz must not alias into 4 kHz: {}", rms(&out[200..]));
    }

    #[test]
    fn segmenter_waits_for_pause_then_cuts() {
        let s = |ms: usize| ms * MS;
        let mut seg = Segmenter { buf: vec![0.1; s(3000)] };
        // speech 1.0–2.7 s, only 0.3 s silence after: keep waiting
        assert!(seg.next_phrase(&[(s(1000), s(2700))], s(8000)).is_none());
        assert_eq!(seg.buf.len(), s(2200)); // leading silence trimmed to PAD
        seg.buf.extend(vec![0.0; s(400)]);
        let p = seg.next_phrase(&[(s(200), s(1900))], s(8000)).unwrap();
        assert_eq!(p.len(), s(2100)); // padded both sides
        assert_eq!(seg.buf.len(), s(500));
    }

    #[test]
    fn segmenter_cuts_long_speech_at_gap() {
        let s = |ms: usize| ms * MS;
        let mut seg = Segmenter { buf: vec![0.1; s(9000)] };
        let p = seg.next_phrase(&[(s(0), s(5000)), (s(5300), s(9000))], s(8000)).unwrap();
        assert_eq!(p.len(), s(5150));
        assert_eq!(seg.buf.len(), s(9000) - s(5150));
        // no gap at all: hard cut, keep overlap
        let mut seg = Segmenter { buf: vec![0.1; s(9000)] };
        let p = seg.next_phrase(&[(s(0), s(9000))], s(8000)).unwrap();
        assert_eq!(p.len(), s(9000));
        assert_eq!(seg.buf.len(), OVERLAP);
    }

    #[test]
    fn segmenter_finds_pause_inside_burst() {
        let s = |ms: usize| ms * MS;
        // one burst holds phrase 1, the pause and the start of phrase 2
        let mut seg = Segmenter { buf: vec![0.1; s(7200)] };
        let p = seg.next_phrase(&[(s(300), s(2700)), (s(4000), s(7200))], s(8000)).unwrap();
        assert_eq!(p.len(), s(2800));
        assert_eq!(seg.buf.len(), s(7200) - s(2900));
    }

    #[test]
    fn silence_is_dropped() {
        let mut seg = Segmenter { buf: vec![0.0; RATE * 5] };
        assert!(seg.next_phrase(&[], RATE * 8).is_none());
        assert_eq!(seg.buf.len(), PAD);
    }

    #[test]
    fn annotations() {
        assert_eq!(strip_annotations("[Music] play Bash (laughs) now ♪").split_whitespace().collect::<Vec<_>>(), ["play", "Bash", "now"]);
    }
}

#[cfg(test)]
mod vad_stream {
    //! `STS2_VAD=<silero.bin> STS2_AUDIO=<16 kHz f32 file> cargo test vad_stream -- --nocapture`
    use super::*;

    #[test]
    fn streaming_phrases() {
        let (Ok(model), Ok(audio)) = (std::env::var("STS2_VAD"), std::env::var("STS2_AUDIO")) else { return };
        let bytes = std::fs::read(audio).unwrap();
        let audio: Vec<f32> = bytes.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect();
        let mut vad = WhisperVadContext::new(&model, WhisperVadContextParams::default()).unwrap();
        let mut seg = Segmenter::default();
        let mut fed = 0;
        let step = std::env::var("STS2_CHUNK_MS").map_or(VAD_STEP, |v| v.parse::<usize>().unwrap() * MS);
        for chunk in audio.chunks(step) {
            seg.buf.extend_from_slice(chunk);
            fed += chunk.len();
            let speech = detect_speech(&mut vad, &seg.buf);
            let before = seg.buf.len();
            if let Some(p) = seg.next_phrase(&speech, 8 * RATE) {
                println!("at {:.2}s: phrase of {:.2}s (buffer {:.2}s, speech {:?})", fed as f32 / RATE as f32, p.len() as f32 / RATE as f32, before as f32 / RATE as f32,
                    speech.iter().map(|(a, b)| (*a as f32 / RATE as f32, *b as f32 / RATE as f32)).collect::<Vec<_>>());
            }
        }
    }
}
