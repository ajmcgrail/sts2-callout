//! Captures what the computer is playing.
//!
//! - Windows: WASAPI loopback on an output device.
//! - macOS 14.6+: CoreAudio process tap on an output device (cpal handles it).
//! - Linux: the PulseAudio/PipeWire "Monitor of ..." source of the output.
//!
//! Any regular input device (e.g. a virtual cable) can be picked instead.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{ErrorKind, FromSample, SampleFormat, SizedSample};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    pub key: String,
    pub label: String,
    pub host: String,
    pub loopback: bool,
}

/// Can we record an *output* device directly on this platform?
const LOOPBACK_SUPPORTED: bool = cfg!(any(target_os = "windows", target_os = "macos"));

fn key(host: &str, loopback: bool, name: &str) -> String {
    format!("{host}|{}|{name}", if loopback { "loop" } else { "in" })
}

pub fn list_devices() -> Vec<DeviceInfo> {
    let mut out = Vec::new();
    for id in cpal::available_hosts() {
        let Ok(host) = cpal::host_from_id(id) else { continue };
        let host_name = id.name().to_string();
        let Ok(devices) = host.devices() else { continue };
        for d in devices {
            let name = d.to_string();
            if LOOPBACK_SUPPORTED && d.supports_output() {
                out.push(DeviceInfo {
                    key: key(&host_name, true, &name),
                    label: format!("{name} (system output)"),
                    host: host_name.clone(),
                    loopback: true,
                });
            }
            if d.supports_input() {
                let is_monitor = name.to_lowercase().contains("monitor");
                out.push(DeviceInfo {
                    key: key(&host_name, false, &name),
                    label: if is_monitor { format!("{name} (system output)") } else { name.clone() },
                    host: host_name.clone(),
                    loopback: is_monitor,
                });
            }
        }
    }
    out
}

fn resolve(key_str: Option<&str>) -> Result<(cpal::Device, bool, String)> {
    if let Some(k) = key_str {
        let mut parts = k.splitn(3, '|');
        let (h, mode, name) = (parts.next(), parts.next(), parts.next());
        if let (Some(h), Some(mode), Some(name)) = (h, mode, name) {
            for id in cpal::available_hosts() {
                if id.name() != h {
                    continue;
                }
                let host = cpal::host_from_id(id)?;
                if let Some(d) = host.devices()?.find(|d| d.to_string() == name) {
                    return Ok((d, mode == "loop", name.to_string()));
                }
            }
        }
        log::warn!("saved audio device {k:?} not found, falling back to automatic");
    }
    default_system_output()
}

fn default_system_output() -> Result<(cpal::Device, bool, String)> {
    let host = cpal::default_host();
    if LOOPBACK_SUPPORTED {
        let d = host.default_output_device().ok_or_else(|| anyhow!("no output device"))?;
        let name = d.to_string();
        return Ok((d, true, name));
    }

    // Linux & co: look for a monitor source, preferring the default sink's.
    let sink_name = host.default_output_device().map(|d| d.to_string().to_lowercase());
    for id in cpal::available_hosts() {
        let Ok(h) = cpal::host_from_id(id) else { continue };
        let Ok(devices) = h.devices() else { continue };
        let monitors: Vec<_> = devices
            .filter(|d| d.supports_input() && d.to_string().to_lowercase().contains("monitor"))
            .collect();
        let best = monitors
            .iter()
            .position(|d| sink_name.as_ref().is_some_and(|s| d.to_string().to_lowercase().contains(s.as_str())))
            .unwrap_or(0);
        if let Some(d) = monitors.into_iter().nth(best) {
            let name = d.to_string();
            return Ok((d, false, name));
        }
    }
    let d = host
        .default_input_device()
        .ok_or_else(|| anyhow!("no monitor source or input device found"))?;
    log::warn!("no monitor source found; using default input {d}");
    let name = d.to_string();
    Ok((d, false, name))
}

pub struct Capture {
    pub sample_rate: u32,
    pub label: String,
}

/// Starts capturing on a dedicated thread (cpal streams aren't `Send` on every
/// platform). Mono f32 chunks go to `tx` until `stop` is set.
pub fn start(device_key: Option<String>, tx: Sender<Vec<f32>>, stop: Arc<AtomicBool>) -> Result<Capture> {
    let (ready_tx, ready_rx) = mpsc::channel::<Result<Capture>>();
    std::thread::Builder::new().name("audio-capture".into()).spawn(move || {
        let failed = Arc::new(AtomicBool::new(false));
        let stream = match open(device_key.as_deref(), tx, failed.clone()) {
            Ok((stream, cap)) => {
                let _ = ready_tx.send(Ok(cap));
                stream
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                return;
            }
        };
        while !stop.load(Ordering::Relaxed) && !failed.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
        }
        // Dropping the stream drops the sender, which ends the speech thread.
        drop(stream);
    })?;
    ready_rx.recv().map_err(|_| anyhow!("audio thread died"))?
}

fn open(device_key: Option<&str>, tx: Sender<Vec<f32>>, failed: Arc<AtomicBool>) -> Result<(cpal::Stream, Capture)> {
    let (device, loopback, name) = resolve(device_key)?;
    let supported = if loopback { device.default_output_config()? } else { device.default_input_config()? };
    let format = supported.sample_format();
    #[allow(unused_mut)]
    let mut config: cpal::StreamConfig = supported.config();
    // PulseAudio's default buffering adds ~1.5 s of latency; ask for ~50 ms blocks.
    #[cfg(target_os = "linux")]
    if !loopback {
        config.buffer_size = cpal::BufferSize::Fixed(config.sample_rate / 20);
    }
    log::info!("capturing {name} ({} Hz, {} ch, {format:?}, loopback={loopback})", config.sample_rate, config.channels);
    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, &config, tx, failed)?,
        SampleFormat::F64 => build::<f64>(&device, &config, tx, failed)?,
        SampleFormat::I16 => build::<i16>(&device, &config, tx, failed)?,
        SampleFormat::I32 => build::<i32>(&device, &config, tx, failed)?,
        SampleFormat::I8 => build::<i8>(&device, &config, tx, failed)?,
        SampleFormat::U16 => build::<u16>(&device, &config, tx, failed)?,
        SampleFormat::U8 => build::<u8>(&device, &config, tx, failed)?,
        other => return Err(anyhow!("unsupported sample format {other:?}")),
    };
    stream.play()?;
    Ok((stream, Capture { sample_rate: config.sample_rate, label: name }))
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    tx: Sender<Vec<f32>>,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = config.channels.max(1) as usize;
    let stream = device.build_input_stream::<T, _, _>(
        *config,
        move |data: &[T], _| {
            let mono: Vec<f32> = data
                .chunks(channels)
                .map(|frame| frame.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() / channels as f32)
                .collect();
            let _ = tx.send(mono);
        },
        move |e: cpal::Error| {
            log::warn!("audio stream error: {e}");
            // Unplugged / switched device: the stream won't recover, so stop
            // instead of listening to nothing.
            if matches!(e.kind(), ErrorKind::DeviceNotAvailable | ErrorKind::StreamInvalidated | ErrorKind::DeviceChanged) {
                failed.store(true, Ordering::Relaxed);
            }
        },
        None,
    )?;
    Ok(stream)
}
