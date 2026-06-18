//! Audio sourcing for the spike: load a WAV (deterministic bench) or capture the
//! microphone (live feel), and resample everything to the 16 kHz mono f32 the ASR
//! model expects.
//!
//! NOT in scope for this crate: system-output *loopback* capture (the meeting
//! audio). That is the platform-specific half of ADR-0017 §1 — macOS
//! ScreenCaptureKit, Windows WASAPI loopback — and is tracked as the M2 capture
//! work. It is deliberately omitted here because (a) it cannot compile on the
//! Linux CI box this was scaffolded on, and (b) measuring ASR latency/RTF (the M0
//! decision gate) only needs *some* 16 kHz mono stream, which mic + WAV provide.
//! See the README for the loopback follow-on and the exact crates to use.

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::mpsc::{Receiver, Sender};

pub const TARGET_RATE: i32 = 16_000;

/// Read a WAV file fully and return 16 kHz mono f32 samples.
pub fn load_wav_16k_mono(path: &str) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("open {path}"))?;
    let spec = reader.spec();
    let channels = spec.channels as usize;

    // Decode to interleaved f32 regardless of the file's sample format.
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / max))
                .collect::<Result<_, _>>()?
        }
    };

    let mono = downmix_to_mono(&interleaved, channels);
    let mut resampler = LinearResampler::new(spec.sample_rate as f64);
    let mut out = resampler.process(&mono);
    out.extend(resampler.flush());
    Ok(out)
}

/// Start mic capture. Returns the live stream (kept alive by the caller), a
/// receiver of 16 kHz mono f32 chunks, and the device's native rate (for logging).
pub fn start_mic_16k_mono() -> Result<(cpal::Stream, Receiver<Vec<f32>>, u32)> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .context("no default input device")?;
    let supported = device.default_input_config()?;
    let native_rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;
    let config: cpal::StreamConfig = supported.config();
    let sample_format = supported.sample_format();

    let (tx, rx) = std::sync::mpsc::channel::<Vec<f32>>();
    let mut resampler = LinearResampler::new(native_rate as f64);
    let err_fn = |e| eprintln!("[mic] stream error: {e}");

    // One callback closure shared across sample formats: downmix → resample → send.
    let make_push = move |tx: Sender<Vec<f32>>| {
        move |mono_native: Vec<f32>| {
            let chunk = resampler.process(&mono_native);
            if !chunk.is_empty() {
                let _ = tx.send(chunk);
            }
        }
    };

    let stream = match sample_format {
        cpal::SampleFormat::F32 => {
            let mut push = make_push(tx);
            device.build_input_stream(
                &config,
                move |data: &[f32], _| push(downmix_to_mono(data, channels)),
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::I16 => {
            let mut push = make_push(tx);
            device.build_input_stream(
                &config,
                move |data: &[i16], _| {
                    let f: Vec<f32> = data.iter().map(|&s| s as f32 / 32768.0).collect();
                    push(downmix_to_mono(&f, channels));
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::U16 => {
            let mut push = make_push(tx);
            device.build_input_stream(
                &config,
                move |data: &[u16], _| {
                    let f: Vec<f32> = data.iter().map(|&s| (s as f32 - 32768.0) / 32768.0).collect();
                    push(downmix_to_mono(&f, channels));
                },
                err_fn,
                None,
            )?
        }
        other => anyhow::bail!("unsupported sample format: {other:?}"),
    };

    stream.play()?;
    Ok((stream, rx, native_rate))
}

fn downmix_to_mono(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Minimal stateful linear resampler to 16 kHz, mono. Carries one sample across
/// chunk boundaries so live capture has no per-chunk discontinuity.
///
/// Linear interpolation is *good enough for a latency/RTF spike*. Production
/// capture (M2) should use a proper sinc resampler (e.g. `rubato`) — ASR WER is
/// mildly sensitive to resampling quality.
struct LinearResampler {
    src_rate: f64,
    step: f64, // input samples advanced per output sample
    t: f64,    // fractional read index into [prev, input...]
    prev: f32,
}

impl LinearResampler {
    fn new(src_rate: f64) -> Self {
        Self {
            src_rate,
            step: src_rate / TARGET_RATE as f64,
            t: 1.0, // start at the first real input sample (index 1; index 0 is `prev`)
            prev: 0.0,
        }
    }

    fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if input.is_empty() {
            return Vec::new();
        }
        // Virtual buffer: data[0] = prev, data[1..=n] = input.
        let n = input.len();
        let at = |i: usize| if i == 0 { self.prev } else { input[i - 1] };

        let mut out = Vec::with_capacity(((n as f64) / self.step) as usize + 1);
        while (self.t as usize) + 1 <= n {
            let i = self.t as usize;
            let frac = (self.t - i as f64) as f32;
            out.push(at(i) * (1.0 - frac) + at(i + 1) * frac);
            self.t += self.step;
        }
        // Re-anchor: new index 0 is the last input sample.
        self.t -= n as f64;
        self.prev = input[n - 1];
        out
    }

    fn flush(&mut self) -> Vec<f32> {
        Vec::new() // linear resampler holds no tail buffer
    }
}
