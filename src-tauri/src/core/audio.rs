//! Audio DSP for the capture pipeline (ADR-0017 §1, plan M2): downmix to mono and
//! resample to the 16 kHz the transcriber expects.
//!
//! Pure and dependency-free — no audio backend, no platform code — so it compiles
//! and is unit-tested in the default build. The platform capture backends
//! (`core::capture`) feed their native-rate, possibly-multichannel frames through
//! here before the [`crate::core::transcription`] engine sees them.
//!
//! The resampler is the same linear interpolation the M0 spike used. Linear is
//! fine for the M2 spine; if measured WER warrants it, swap in a sinc resampler
//! (`rubato`) behind the `audio` feature — the call sites do not change.

// M2 capture-pipeline scaffolding: exercised by unit tests and the `audio` feature,
// wired into the running app when M3/M4 land. Unused in the default lib build, so
// allow dead_code rather than delete the seams (keeps `clippy -D warnings` green).
#![allow(dead_code)]

/// Sample rate the transcription stack consumes (ADR-0017 §2).
pub const TARGET_RATE: u32 = 16_000;

/// Average interleaved frames down to mono. `channels <= 1` is a passthrough.
pub fn downmix_to_mono(interleaved: &[f32], channels: u16) -> Vec<f32> {
    let channels = channels.max(1) as usize;
    if channels == 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / frame.len().max(1) as f32)
        .collect()
}

/// Stateful linear resampler to [`TARGET_RATE`], mono in / mono out. Carries one
/// sample across chunk boundaries so streamed capture has no per-chunk seam.
pub struct Resampler {
    step: f64, // input samples advanced per output sample
    t: f64,    // fractional read index into [prev, input...]
    prev: f32,
}

impl Resampler {
    pub fn new(src_rate: u32) -> Self {
        let src = src_rate.max(1) as f64;
        Self {
            step: src / TARGET_RATE as f64,
            t: 1.0, // start at the first real input sample (index 0 is `prev`)
            prev: 0.0,
        }
    }

    /// Resample one chunk of mono input to 16 kHz, continuing from prior chunks.
    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if input.is_empty() {
            return Vec::new();
        }
        let n = input.len();
        // Virtual buffer: index 0 = prev, indices 1..=n = input.
        let at = |i: usize| if i == 0 { self.prev } else { input[i - 1] };

        let mut out = Vec::with_capacity((n as f64 / self.step) as usize + 1);
        while (self.t as usize) < n {
            let i = self.t as usize;
            let frac = (self.t - i as f64) as f32;
            out.push(at(i) * (1.0 - frac) + at(i + 1) * frac);
            self.t += self.step;
        }
        // Re-anchor: the last input sample becomes the next chunk's index 0.
        self.t -= n as f64;
        self.prev = input[n - 1];
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_averages_stereo_and_passes_mono() {
        assert_eq!(downmix_to_mono(&[1.0, 3.0, 2.0, 4.0], 2), vec![2.0, 3.0]);
        let mono = vec![0.5, -0.5, 0.25];
        assert_eq!(downmix_to_mono(&mono, 1), mono);
    }

    #[test]
    fn resampler_is_passthrough_at_target_rate() {
        let mut r = Resampler::new(TARGET_RATE);
        // At 1:1 a lone chunk reproduces the input except its last sample, which
        // is carried across the boundary (standard 1-sample streaming latency).
        let input: Vec<f32> = (0..10).map(|i| i as f32).collect();
        let out = r.process(&input);
        assert_eq!(out.as_slice(), &input[..input.len() - 1]);
        // The carried sample emerges as the first output of the next chunk.
        let next = r.process(&[10.0, 11.0]);
        assert_eq!(next.first().copied(), Some(9.0));
    }

    #[test]
    fn resampler_halves_length_when_downsampling_2x() {
        // 32 kHz → 16 kHz roughly halves the sample count, stable across chunks.
        let mut r = Resampler::new(2 * TARGET_RATE);
        let chunk: Vec<f32> = (0..1000).map(|i| (i as f32).sin()).collect();
        let mut total = 0usize;
        for _ in 0..5 {
            total += r.process(&chunk).len();
        }
        let expected = 5 * 1000 / 2;
        assert!(
            (total as i64 - expected as i64).abs() <= 2,
            "got {total}, expected ~{expected}"
        );
    }
}
