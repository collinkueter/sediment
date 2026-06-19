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

/// Spawn the pipeline. `on_segment(offset_ms, speaker, text)` is invoked on the
/// worker thread for each final utterance; it must be `Send`.
pub fn spawn<S, F>(
    source: S,
    mut transcriber: Box<dyn Transcriber>,
    speaker: String,
    mut on_segment: F,
) -> CaptureController
where
    S: CaptureSource + 'static,
    F: FnMut(i64, &str, &str) + Send + 'static,
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

        loop {
            if stop_worker.load(Ordering::Acquire) {
                break;
            }
            match cap.rx.recv_timeout(Duration::from_millis(250)) {
                Ok(frame) => {
                    let mono = downmix_to_mono(&frame, cap.format.channels);
                    let samples = resampler.process(&mono);
                    total_16k += samples.len() as u64;
                    for u in transcriber.accept(&samples) {
                        if u.is_final {
                            on_segment(offset_ms(total_16k), &speaker, &u.text);
                        }
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        for u in transcriber.finish() {
            on_segment(offset_ms(total_16k), &speaker, &u.text);
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
}
