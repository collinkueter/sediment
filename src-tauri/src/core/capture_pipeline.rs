//! Capture → transcription pipeline (ADR-0017 §1/§2, plan M2).
//!
//! Connects a [`CaptureSource`] to a [`Transcriber`]: pull native frames, downmix
//! and resample to 16 kHz ([`crate::core::audio`]), feed the transcriber, and hand
//! each final utterance to a callback as a `(offset_ms, speaker, text)` segment —
//! the same shape `session_push_segment` produces, so it lands in the Meeting note
//! through the one [`crate::core::session::record_segment`] path.
//!
//! The whole thing is platform-independent and tested with a [`VecSource`] +
//! [`MockTranscriber`]; the real mic/loopback sources and ASR engine slot in
//! behind it without changing this orchestration.
//!
//! M2 has no diarization — every utterance is attributed to one placeholder
//! `speaker`. M4 replaces that with per-segment diarization + Voiceprint matching.

// M2 pipeline scaffolding: tested with VecSource + MockTranscriber and wired into
// the app when the real capture/ASR backends land; unused in the default lib build.
#![allow(dead_code)]

use crate::core::audio::{downmix_to_mono, Resampler, TARGET_RATE};
use crate::core::capture::CaptureSource;
use crate::core::transcription::Transcriber;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

/// Owns a running pipeline. Dropping it (or calling [`CaptureController::stop`])
/// signals teardown and joins the worker — so a Session that drops its controller
/// on stop tears capture down deterministically (ADR-0017 §3).
pub struct CaptureController {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl CaptureController {
    /// User-initiated teardown: signal the source/worker to stop and wait for the
    /// worker to exit. This is what `session_stop` triggers (via `Drop`).
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }

    /// Wait for the pipeline to finish *on its own* — i.e. when the source ends
    /// (EOF). Unlike [`stop`](Self::stop) it does not force the source to stop, so
    /// a finite source (a WAV, the test `VecSource`) drains fully first. Used by
    /// tests; live capture uses `stop`.
    #[cfg(test)]
    pub fn join(mut self) {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for CaptureController {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Spawn the pipeline. For each final utterance, `resolve_speaker` is handed the
/// segment's 16 kHz mono audio (the samples since the previous final) and returns
/// a speaker label — `None` falls back to `default_speaker`. `on_segment(offset_ms,
/// speaker, text)` then lands the segment. Both callbacks run on the worker thread
/// and must be `Send`. The resolver is the seam where diarization
/// (`core::diarization`) plugs in without this orchestration knowing about it.
pub fn spawn<S, F, R>(
    source: S,
    mut transcriber: Box<dyn Transcriber>,
    default_speaker: String,
    mut resolve_speaker: R,
    mut on_segment: F,
) -> CaptureController
where
    S: CaptureSource + 'static,
    F: FnMut(i64, &str, &str) + Send + 'static,
    R: FnMut(&[f32]) -> Option<String> + Send + 'static,
{
    let stop = Arc::new(AtomicBool::new(false));
    let stop_worker = stop.clone();

    let handle = std::thread::spawn(move || {
        let cap = match source.start(stop_worker.clone()) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("[pipeline] capture start failed: {e}");
                return;
            }
        };
        let mut resampler = Resampler::new(cap.format.sample_rate);
        let mut total_16k: u64 = 0;
        let offset_ms = |samples: u64| (samples as i64 * 1000) / TARGET_RATE as i64;
        // The audio of the segment currently being decoded — the samples since the
        // last committed final. Handed to `resolve_speaker` when a final lands, then
        // cleared. This is the window diarization embeds.
        let mut segment_audio: Vec<f32> = Vec::new();

        loop {
            if stop_worker.load(Ordering::Acquire) {
                break;
            }
            match cap.rx.recv_timeout(Duration::from_millis(250)) {
                Ok(frame) => {
                    let mono = downmix_to_mono(&frame, cap.format.channels);
                    let samples = resampler.process(&mono);
                    total_16k += samples.len() as u64;
                    segment_audio.extend_from_slice(&samples);
                    for u in transcriber.accept(&samples) {
                        if u.is_final {
                            let speaker = resolve_speaker(&segment_audio)
                                .unwrap_or_else(|| default_speaker.clone());
                            on_segment(offset_ms(total_16k), &speaker, &u.text);
                            segment_audio.clear();
                        }
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        for u in transcriber.finish() {
            let speaker =
                resolve_speaker(&segment_audio).unwrap_or_else(|| default_speaker.clone());
            on_segment(offset_ms(total_16k), &speaker, &u.text);
            segment_audio.clear();
        }
    });

    CaptureController {
        stop,
        handle: Some(handle),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::capture::{CaptureFormat, VecSource};
    use crate::core::transcription::MockTranscriber;
    use std::sync::Mutex;

    #[test]
    fn pipeline_resamples_and_emits_segments() {
        // 3 s of 32 kHz mono audio in 0.5 s chunks → resamples to ~3 s @16 kHz →
        // MockTranscriber(1 s) emits ~3 finals.
        let rate = 32_000usize;
        let chunk = vec![0.0f32; rate / 2]; // 0.5 s
        let chunks = vec![chunk; 6]; // 3 s total
        let source = VecSource::new(
            CaptureFormat {
                sample_rate: rate as u32,
                channels: 1,
            },
            chunks,
        );

        let segments = Arc::new(Mutex::new(Vec::<(i64, String, String)>::new()));
        let sink = segments.clone();
        let controller = spawn(
            source,
            Box::new(MockTranscriber::new(1.0)),
            "Unknown speaker 1".to_string(),
            |_audio| None, // no diarization in the pipeline test
            move |offset, speaker, text| {
                sink.lock()
                    .unwrap()
                    .push((offset, speaker.to_string(), text.to_string()));
            },
        );
        controller.join(); // wait for the finite source to drain (no forced stop)

        let got = segments.lock().unwrap();
        assert!(
            (2..=4).contains(&got.len()),
            "expected ~3 segments, got {}: {:?}",
            got.len(),
            *got
        );
        // Offsets are monotonic and attributed to the placeholder speaker.
        assert!(got.windows(2).all(|w| w[0].0 <= w[1].0));
        assert!(got.iter().all(|(_, sp, _)| sp == "Unknown speaker 1"));
    }

    /// Full real pipeline: a WAV-replaying source → real `LocalTranscriber` → real
    /// `Diarizer` → segments, exercising every link except the mic/loopback backend
    /// (the same cpal code the M0 spike validated on hardware). Ignored; needs the
    /// spike ASR model and a speaker model. Run on hardware:
    /// `SEDIMENT_SPEAKER_MODEL=/path/to/wespeaker_en_voxceleb_CAM++.onnx \
    ///  cargo test --no-default-features --features audio,local-asr \
    ///  real_pipeline_wav_to_segments -- --ignored --nocapture`
    #[cfg(feature = "local-asr")]
    #[test]
    #[ignore]
    fn real_pipeline_wav_to_segments() {
        use crate::core::diarization::Diarizer;
        use crate::core::transcription::{AsrModelPaths, LocalTranscriber, Transcriber};
        use std::collections::HashMap;
        use std::path::PathBuf;

        let spike = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("spikes/m0-capture-asr");
        let f = |n: &str| spike.join(n).to_string_lossy().into_owned();
        let paths = AsrModelPaths {
            encoder: f("encoder-epoch-99-avg-1-chunk-16-left-128.onnx"),
            decoder: f("decoder-epoch-99-avg-1-chunk-16-left-128.onnx"),
            joiner: f("joiner-epoch-99-avg-1-chunk-16-left-128.onnx"),
            tokens: f("tokens.txt"),
            provider: "cpu".to_string(),
        };
        let transcriber: Box<dyn Transcriber> =
            Box::new(LocalTranscriber::new(&paths).expect("recognizer"));

        // Replay the WAV at its native rate through a VecSource (the pipeline
        // downmixes + resamples, as it does for a real device).
        let mut reader = hound::WavReader::open(spike.join("sample-0.wav")).expect("wav");
        let spec = reader.spec();
        let samples: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Float => reader.samples::<f32>().map(|s| s.unwrap()).collect(),
            hound::SampleFormat::Int => {
                let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
                reader
                    .samples::<i32>()
                    .map(|s| s.unwrap() as f32 / max)
                    .collect()
            }
        };
        let chunks: Vec<Vec<f32>> = samples.chunks(1600).map(|c| c.to_vec()).collect();
        let source = VecSource::new(
            CaptureFormat {
                sample_rate: spec.sample_rate,
                channels: spec.channels,
            },
            chunks,
        );

        // Real diarizer over a speaker model (env override → the downloaded model).
        let model = std::env::var("SEDIMENT_SPEAKER_MODEL")
            .unwrap_or_else(|_| "/tmp/wespeaker_en_voxceleb_CAM++.onnx".to_string());
        let centroids = Arc::new(Mutex::new(HashMap::new()));
        let mut diarizer = Diarizer::new(&model, Vec::new(), centroids).expect("diarizer");

        let segments = Arc::new(Mutex::new(Vec::<(i64, String, String)>::new()));
        let sink = segments.clone();
        let controller = spawn(
            source,
            transcriber,
            "Unknown speaker 1".to_string(),
            move |audio| Some(diarizer.assign(audio)),
            move |offset, speaker, text| {
                sink.lock()
                    .unwrap()
                    .push((offset, speaker.to_string(), text.to_string()));
            },
        );
        controller.join();

        let got = segments.lock().unwrap();
        println!("pipeline segments: {got:#?}");
        assert!(!got.is_empty(), "no segments produced");
        let words: usize = got
            .iter()
            .map(|(_, _, t)| t.split_whitespace().count())
            .sum();
        assert!(words >= 3, "expected real words across segments: {got:?}");
        assert!(
            got.iter()
                .all(|(_, sp, _)| sp.starts_with("Unknown speaker") || !sp.is_empty()),
            "every segment has a speaker label: {got:?}"
        );
    }
}
