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

/// Split a 16 kHz mono buffer into utterance-ish ranges at silent gaps, for the
/// offline second pass (ADR-0017 §2): each range is transcribed and diarized on its
/// own, so boundaries land on natural pauses, not mid-word. Returns `(start, end)`
/// index pairs (end exclusive). The silence threshold is adaptive — a fraction of the
/// loudest frame — so it tracks recording gain; a long quiet run ends a range, and an
/// over-long range is force-split so one pause-free monologue can't become a single
/// huge chunk. Always yields at least one range for non-empty input (the whole buffer
/// when it finds no usable split), so the second pass still runs.
pub fn split_on_silence(samples: &[f32], rate: u32) -> Vec<(usize, usize)> {
    if samples.is_empty() {
        return Vec::new();
    }
    let rate = rate.max(1) as usize;
    let frame = (rate / 50).max(1); // 20 ms analysis frames
    let min_silence_frames = (rate * 6 / 10 / frame).max(1); // ~0.6 s gap splits
    let min_seg = rate * 3 / 10; // drop ranges under ~0.3 s
    let max_seg = rate * 30; // force a split past ~30 s

    let rms: Vec<f32> = samples
        .chunks(frame)
        .map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt())
        .collect();
    let peak = rms.iter().copied().fold(0.0f32, f32::max);
    // Quiet = below 12% of the peak, with a tiny absolute floor for true silence.
    let thresh = (peak * 0.12).max(1e-4);

    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut seg_start: Option<usize> = None;
    let mut silence_run = 0usize;
    for (fi, &r) in rms.iter().enumerate() {
        let sample_i = fi * frame;
        if r >= thresh {
            silence_run = 0;
            let start = *seg_start.get_or_insert(sample_i);
            if sample_i - start >= max_seg {
                ranges.push((start, (sample_i + frame).min(samples.len())));
                seg_start = Some(sample_i);
            }
        } else {
            silence_run += 1;
            if let Some(start) = seg_start {
                if silence_run >= min_silence_frames {
                    let end = ((fi + 1 - silence_run) * frame).min(samples.len());
                    if end > start {
                        ranges.push((start, end));
                    }
                    seg_start = None;
                }
            }
        }
    }
    if let Some(start) = seg_start {
        ranges.push((start, samples.len()));
    }
    ranges.retain(|(s, e)| e - s >= min_seg);
    if ranges.is_empty() {
        ranges.push((0, samples.len()));
    }
    ranges
}

/// Write mono f32 `samples` as a 16-bit PCM WAV at `rate` — used to persist a
/// person's short **voice clip** (ADR-0017 §6). Dependency-free (a minimal RIFF
/// writer) so it's available in every build and keeps clips compact (16-bit). f32
/// samples are clamped to [-1, 1] before scaling.
pub fn write_wav_i16(path: &std::path::Path, samples: &[f32], rate: u32) -> std::io::Result<()> {
    use std::io::Write;
    let channels: u16 = 1;
    let bits: u16 = 16;
    let block_align = channels * bits / 8;
    let byte_rate = rate * block_align as u32;
    let data_len = (samples.len() * 2) as u32;

    let mut w = std::io::BufWriter::new(std::fs::File::create(path)?);
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data_len).to_le_bytes())?;
    w.write_all(b"WAVE")?;
    w.write_all(b"fmt ")?;
    w.write_all(&16u32.to_le_bytes())?; // fmt chunk size
    w.write_all(&1u16.to_le_bytes())?; // PCM
    w.write_all(&channels.to_le_bytes())?;
    w.write_all(&rate.to_le_bytes())?;
    w.write_all(&byte_rate.to_le_bytes())?;
    w.write_all(&block_align.to_le_bytes())?;
    w.write_all(&bits.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&data_len.to_le_bytes())?;
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        w.write_all(&v.to_le_bytes())?;
    }
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_wav_i16_emits_a_riff_header_and_pcm_payload() {
        let dir = std::env::temp_dir().join(format!("sediment-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.wav");
        let samples = vec![0.0f32, 0.5, -0.5, 1.0, -1.0];
        write_wav_i16(&path, &samples, TARGET_RATE).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        // 44-byte header + 2 bytes per sample.
        assert_eq!(bytes.len(), 44 + samples.len() * 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn split_on_silence_separates_utterances_and_falls_back() {
        let rate = TARGET_RATE as usize;
        let tone = || (0..rate).map(|i| (i as f32 * 0.1).sin() * 0.5);
        let mut buf: Vec<f32> = Vec::new();
        buf.extend(tone()); // 1 s speech
        buf.extend(std::iter::repeat(0.0).take(rate)); // 1 s pause
        buf.extend(tone()); // 1 s speech
        let ranges = split_on_silence(&buf, TARGET_RATE);
        assert_eq!(ranges.len(), 2, "two utterances split by the pause: {ranges:?}");
        assert!(ranges[0].0 < ranges[0].1 && ranges[1].0 > ranges[0].1);

        // Pure silence → one fallback range spanning the whole buffer.
        assert_eq!(
            split_on_silence(&vec![0.0f32; rate], TARGET_RATE),
            vec![(0, rate)]
        );
        // Empty input → no ranges.
        assert!(split_on_silence(&[], TARGET_RATE).is_empty());
    }

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
