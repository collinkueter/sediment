//! Audio capture sources (ADR-0016 §1, plan M2).
//!
//! A [`CaptureSource`] begins capturing on `start` and streams native-rate,
//! possibly-multichannel interleaved f32 frames over a channel until its `stop`
//! flag is set. The capture pipeline ([`crate::core::capture_pipeline`]) downmixes
//! and resamples those frames before transcription.
//!
//! The trait and the test [`VecSource`] are always built (default build + tests).
//! The real microphone backend ([`MicSource`], cpal) is behind the **`audio`**
//! feature so the default build pulls no audio crate and stays verifiable on this
//! headless CI. System-output **loopback** (the meeting's far-side audio — macOS
//! ScreenCaptureKit, Windows WASAPI) is the immediate follow-on within M2: it
//! slots in as another `CaptureSource`, and the pipeline mixes it with the mic.
//! It is not implemented here because it is macOS/Windows-only native code that
//! can neither compile nor run in this container — it needs on-hardware iteration.
//!
//! `cpal::Stream` is `!Send` on several backends, so [`MicSource`] owns the stream
//! on a dedicated thread and communicates via a channel + the shared stop flag,
//! rather than moving the stream across threads.

use crate::error::AppResult;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Receiver;
use std::sync::Arc;

#[derive(Debug, Clone, Copy)]
pub struct CaptureFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

/// A live capture: the stream format plus a receiver of native-rate interleaved
/// f32 frames. The frames stop arriving (and the sender drops) when the source's
/// stop flag is set.
pub struct CaptureHandle {
    pub format: CaptureFormat,
    pub rx: Receiver<Vec<f32>>,
}

/// Something that can be captured from. `Send` so the pipeline can own it on its
/// worker thread.
pub trait CaptureSource: Send {
    /// Begin capturing. `stop` is the shared teardown signal — the source must
    /// stop producing and release resources once it is set.
    fn start(&self, stop: Arc<AtomicBool>) -> AppResult<CaptureHandle>;
}

// ──────────────────────────────────────────────────────────────────────────
// Test / programmatic source — drives the pipeline without any audio backend.
// ──────────────────────────────────────────────────────────────────────────

/// Replays a fixed list of frames at a fixed format, then ends. Used by the
/// pipeline tests as a deterministic stand-in for a real source. Honors the stop
/// flag between frames. Test-only for now; a real WAV-file source (M3 offline
/// bench) would be its own type.
#[cfg(test)]
pub struct VecSource {
    format: CaptureFormat,
    chunks: Vec<Vec<f32>>,
}

#[cfg(test)]
impl VecSource {
    pub fn new(format: CaptureFormat, chunks: Vec<Vec<f32>>) -> Self {
        Self { format, chunks }
    }
}

#[cfg(test)]
impl CaptureSource for VecSource {
    fn start(&self, stop: Arc<AtomicBool>) -> AppResult<CaptureHandle> {
        let (tx, rx) = std::sync::mpsc::channel();
        let chunks = self.chunks.clone();
        std::thread::spawn(move || {
            for chunk in chunks {
                if stop.load(std::sync::atomic::Ordering::Acquire) {
                    break;
                }
                if tx.send(chunk).is_err() {
                    break;
                }
            }
            // Dropping `tx` here closes the channel — the pipeline sees EOF.
        });
        Ok(CaptureHandle {
            format: self.format,
            rx,
        })
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Microphone backend (cpal) — real capture, behind the `audio` feature.
// NOT verifiable in this container (no audio devices); validate on hardware.
// ──────────────────────────────────────────────────────────────────────────

#[cfg(feature = "audio")]
pub struct MicSource;

#[cfg(feature = "audio")]
impl CaptureSource for MicSource {
    fn start(&self, stop: Arc<AtomicBool>) -> AppResult<CaptureHandle> {
        use crate::error::AppError;
        use cpal::traits::{DeviceTrait, HostTrait};

        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| AppError::other("no default input device"))?;
        let supported = device
            .default_input_config()
            .map_err(|e| AppError::other(format!("default input config: {e}")))?;
        let format = CaptureFormat {
            sample_rate: supported.sample_rate().0,
            channels: supported.channels(),
        };
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.config();

        let (tx, rx) = std::sync::mpsc::channel::<Vec<f32>>();

        // The cpal Stream is !Send on some backends, so build and hold it on a
        // dedicated thread; the thread parks until `stop` is set, then drops it.
        std::thread::spawn(move || {
            use cpal::traits::StreamTrait;
            let err_fn = |e| tracing::warn!("[capture] mic stream error: {e}");
            let build = || -> Result<cpal::Stream, cpal::BuildStreamError> {
                match sample_format {
                    cpal::SampleFormat::F32 => {
                        let tx = tx.clone();
                        device.build_input_stream(
                            &config,
                            move |data: &[f32], _: &_| {
                                let _ = tx.send(data.to_vec());
                            },
                            err_fn,
                            None,
                        )
                    }
                    cpal::SampleFormat::I16 => {
                        let tx = tx.clone();
                        device.build_input_stream(
                            &config,
                            move |data: &[i16], _: &_| {
                                let _ = tx.send(data.iter().map(|&s| s as f32 / 32768.0).collect());
                            },
                            err_fn,
                            None,
                        )
                    }
                    cpal::SampleFormat::U16 => {
                        let tx = tx.clone();
                        device.build_input_stream(
                            &config,
                            move |data: &[u16], _: &_| {
                                let _ = tx
                                    .send(data.iter().map(|&s| (s as f32 - 32768.0) / 32768.0).collect());
                            },
                            err_fn,
                            None,
                        )
                    }
                    other => {
                        tracing::error!("[capture] unsupported sample format {other:?}");
                        return Err(cpal::BuildStreamError::StreamConfigNotSupported);
                    }
                }
            };

            let stream = match build() {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("[capture] build mic stream failed: {e}");
                    return;
                }
            };
            if let Err(e) = stream.play() {
                tracing::error!("[capture] mic stream play failed: {e}");
                return;
            }
            // Hold the stream alive until teardown is requested.
            while !stop.load(std::sync::atomic::Ordering::Acquire) {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            drop(stream); // stops capture; `tx` drops with the closures → EOF
        });

        Ok(CaptureHandle { format, rx })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn vec_source_streams_then_eofs() {
        let src = VecSource::new(
            CaptureFormat {
                sample_rate: 16_000,
                channels: 1,
            },
            vec![vec![0.1, 0.2], vec![0.3]],
        );
        let stop = Arc::new(AtomicBool::new(false));
        let handle = src.start(stop).unwrap();
        let got: Vec<Vec<f32>> = handle.rx.iter().collect();
        assert_eq!(got, vec![vec![0.1, 0.2], vec![0.3]]);
    }

    #[test]
    fn vec_source_respects_stop() {
        let src = VecSource::new(
            CaptureFormat {
                sample_rate: 16_000,
                channels: 1,
            },
            (0..1000).map(|_| vec![0.0; 4]).collect(),
        );
        let stop = Arc::new(AtomicBool::new(true)); // already stopped
        let handle = src.start(stop.clone()).unwrap();
        // With stop pre-set, the source sends nothing (or stops immediately).
        let got: Vec<Vec<f32>> = handle.rx.iter().collect();
        assert!(got.len() < 1000, "stop cut the stream short: {}", got.len());
        stop.store(true, Ordering::Release);
    }
}
