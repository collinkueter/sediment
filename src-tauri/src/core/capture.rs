//! Audio capture sources (ADR-0017 §1, plan M2).
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

// M2 capture scaffolding: the trait + test source are exercised by unit tests and
// the real backend lives behind the `audio` feature; unused in the default lib
// build, so allow dead_code rather than delete the seams.
#![allow(dead_code)]

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
                                let _ = tx.send(
                                    data.iter()
                                        .map(|&s| (s as f32 - 32768.0) / 32768.0)
                                        .collect(),
                                );
                            },
                            err_fn,
                            None,
                        )
                    }
                    other => {
                        tracing::error!("[capture] unsupported sample format {other:?}");
                        Err(cpal::BuildStreamError::StreamConfigNotSupported)
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

// ──────────────────────────────────────────────────────────────────────────
// Mixing mic + system-output loopback (ADR-0017 §1). The two streams are mixed to
// one 16 kHz mono stream before transcription so both sides of a meeting land in
// one transcript. The mix is **mic-driven**: output follows the always-present mic
// clock and folds in whatever loopback samples have arrived, so a silent or
// not-yet-permitted loopback never stalls capture.
// ──────────────────────────────────────────────────────────────────────────

use std::collections::VecDeque;

/// Sums two mono streams (already at a common rate) into one, driven by stream A.
/// Pure and unit-tested; the threading in [`MixedSource`] wraps it. Averaging
/// (`* 0.5`) keeps the sum inside `[-1, 1]` without a separate limiter.
#[derive(Default)]
pub struct Mixer {
    a: VecDeque<f32>,
    b: VecDeque<f32>,
}

impl Mixer {
    /// Cap on the loopback (B) backlog — ~5 s at 16 kHz. If B outruns A (it should
    /// not, both nominally 16 kHz) the oldest is dropped rather than grow forever.
    const B_CAP: usize = 16_000 * 5;

    pub fn push_a(&mut self, samples: &[f32]) {
        self.a.extend(samples);
    }

    pub fn push_b(&mut self, samples: &[f32]) {
        self.b.extend(samples);
        while self.b.len() > Self::B_CAP {
            self.b.pop_front();
        }
    }

    /// Emit one mixed sample per buffered A sample, folding in B where present
    /// (zero when B has nothing yet). Drains A fully; consumes the matching B prefix.
    pub fn drain(&mut self) -> Vec<f32> {
        let n = self.a.len();
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let a = self.a.pop_front().unwrap_or(0.0);
            let b = self.b.pop_front().unwrap_or(0.0);
            out.push(((a + b) * 0.5).clamp(-1.0, 1.0));
        }
        out
    }
}

/// Captures two sources at once (mic + loopback), resamples each to 16 kHz mono,
/// and mixes them into one stream. Presents that mix as a [`CaptureSource`] at
/// `TARGET_RATE` mono, so the rest of the pipeline is unchanged.
#[cfg(feature = "loopback")]
pub struct MixedSource {
    pub mic: Box<dyn CaptureSource>,
    pub loopback: Box<dyn CaptureSource>,
}

#[cfg(feature = "loopback")]
impl CaptureSource for MixedSource {
    fn start(&self, stop: Arc<AtomicBool>) -> AppResult<CaptureHandle> {
        use crate::core::audio::{downmix_to_mono, Resampler, TARGET_RATE};

        let mic = self.mic.start(stop.clone())?;
        // Loopback is best-effort: if it can't start (e.g. permission), keep the
        // mic-only stream rather than fail the whole Session.
        let loopback = match self.loopback.start(stop.clone()) {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::warn!("[capture] loopback unavailable, mic only: {e}");
                None
            }
        };

        let (tx, rx) = std::sync::mpsc::channel::<Vec<f32>>();
        std::thread::spawn(move || {
            let mut rs_a = Resampler::new(mic.format.sample_rate);
            let mic_ch = mic.format.channels;
            let mut rs_b = loopback
                .as_ref()
                .map(|h| (Resampler::new(h.format.sample_rate), h.format.channels));
            let mut mixer = Mixer::default();

            loop {
                if stop.load(std::sync::atomic::Ordering::Acquire) {
                    break;
                }
                // Block on the mic (the driving clock); fold in any loopback that has
                // queued up since the last tick.
                match mic.rx.recv_timeout(std::time::Duration::from_millis(250)) {
                    Ok(frame) => {
                        let mono = downmix_to_mono(&frame, mic_ch);
                        mixer.push_a(&rs_a.process(&mono));
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
                if let (Some(h), Some((rs, ch))) = (loopback.as_ref(), rs_b.as_mut()) {
                    while let Ok(frame) = h.rx.try_recv() {
                        let mono = downmix_to_mono(&frame, *ch);
                        mixer.push_b(&rs.process(&mono));
                    }
                }
                let mixed = mixer.drain();
                if !mixed.is_empty() && tx.send(mixed).is_err() {
                    break;
                }
            }
        });

        Ok(CaptureHandle {
            format: CaptureFormat {
                sample_rate: TARGET_RATE,
                channels: 1,
            },
            rx,
        })
    }
}

// ──────────────────────────────────────────────────────────────────────────
// macOS system-output loopback via ScreenCaptureKit (ADR-0017 §1). Captures the
// display's audio (the meeting's far side) as 48 kHz mono f32. Needs the Screen
// Recording permission; NOT verifiable in CI / headless — validate on hardware.
// ──────────────────────────────────────────────────────────────────────────

#[cfg(all(feature = "loopback", target_os = "macos"))]
pub struct ScreenCaptureSource;

#[cfg(all(feature = "loopback", target_os = "macos"))]
impl CaptureSource for ScreenCaptureSource {
    fn start(&self, stop: Arc<AtomicBool>) -> AppResult<CaptureHandle> {
        use crate::error::AppError;
        use screencapturekit::prelude::*;

        const SR: u32 = 48_000;
        let (tx, rx) = std::sync::mpsc::channel::<Vec<f32>>();

        // SCStream and its ObjC objects live on a dedicated thread; the handler is
        // invoked on ScreenCaptureKit's own dispatch queue and forwards samples.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        std::thread::spawn(move || {
            let content = match SCShareableContent::get() {
                Ok(c) => c,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("shareable content: {e:?}")));
                    return;
                }
            };
            let Some(display) = content.displays().into_iter().next() else {
                let _ = ready_tx.send(Err("no display to capture".into()));
                return;
            };
            let filter = SCContentFilter::create()
                .with_display(&display)
                .with_excluding_windows(&[])
                .build();
            let config = SCStreamConfiguration::new()
                .with_captures_audio(true)
                .with_sample_rate(SR as i32)
                .with_channel_count(1)
                .with_excludes_current_process_audio(true);

            let mut stream = SCStream::new(&filter, &config);
            stream.add_output_handler(
                AudioSink {
                    tx: std::sync::Mutex::new(tx),
                },
                SCStreamOutputType::Audio,
            );
            if let Err(e) = stream.start_capture() {
                let _ = ready_tx.send(Err(format!("start capture: {e:?}")));
                return;
            }
            let _ = ready_tx.send(Ok(()));
            while !stop.load(std::sync::atomic::Ordering::Acquire) {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            let _ = stream.stop_capture();
        });

        // Surface a start error (e.g. permission) to the caller so MixedSource can
        // fall back to mic-only. Bounded wait: `SCShareableContent::get()` blocks on
        // the Screen-Recording permission prompt, so without a timeout an unanswered
        // dialog would stall the whole capture (the mic too). Time out → mic-only.
        match ready_rx.recv_timeout(std::time::Duration::from_secs(8)) {
            Ok(Ok(())) => Ok(CaptureHandle {
                format: CaptureFormat {
                    sample_rate: SR,
                    channels: 1,
                },
                rx,
            }),
            Ok(Err(e)) => Err(AppError::other(format!("ScreenCaptureKit loopback: {e}"))),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(AppError::other(
                "ScreenCaptureKit loopback did not start (Screen Recording permission?)",
            )),
            Err(_) => Err(AppError::other("ScreenCaptureKit loopback thread died")),
        }
    }
}

/// ScreenCaptureKit audio handler: reinterpret each mono f32 audio buffer and
/// forward it. Must be `Send + Sync` — the dispatch queue may call it from any
/// thread (hence the `Mutex` around the sender).
#[cfg(all(feature = "loopback", target_os = "macos"))]
struct AudioSink {
    tx: std::sync::Mutex<std::sync::mpsc::Sender<Vec<f32>>>,
}

#[cfg(all(feature = "loopback", target_os = "macos"))]
impl screencapturekit::prelude::SCStreamOutputTrait for AudioSink {
    fn did_output_sample_buffer(
        &self,
        sample: screencapturekit::prelude::CMSampleBuffer,
        of_type: screencapturekit::prelude::SCStreamOutputType,
    ) {
        use screencapturekit::prelude::*;
        if of_type != SCStreamOutputType::Audio {
            return;
        }
        let _ = sample.make_data_ready();
        let Some(list) = sample.audio_buffer_list() else {
            return;
        };
        // channel_count(1) → a single mono buffer of 32-bit float PCM.
        let Some(buffer) = list.get(0) else { return };
        let bytes = buffer.data();
        let samples: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        if !samples.is_empty() {
            if let Ok(tx) = self.tx.lock() {
                let _ = tx.send(samples);
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Windows system-output loopback via WASAPI (ADR-0017 §1). Captures the default
// render endpoint in loopback mode (render device + `Direction::Capture` →
// `AUDCLNT_STREAMFLAGS_LOOPBACK`). 48 kHz stereo f32; the mixer downmixes. Mirrors
// `wasapi`'s own loopback example. Windows-only — validate on a Windows host.
// ──────────────────────────────────────────────────────────────────────────

#[cfg(all(feature = "loopback", target_os = "windows"))]
pub struct WasapiLoopbackSource;

#[cfg(all(feature = "loopback", target_os = "windows"))]
impl CaptureSource for WasapiLoopbackSource {
    fn start(&self, stop: Arc<AtomicBool>) -> AppResult<CaptureHandle> {
        use crate::error::AppError;
        use std::collections::VecDeque as ByteDeque;
        use wasapi::{
            initialize_mta, DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat,
        };

        const SR: u32 = 48_000;
        const CH: u16 = 2;
        let (tx, rx) = std::sync::mpsc::channel::<Vec<f32>>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();

        std::thread::spawn(move || {
            // COM must be initialised on the capture thread. Ignore the result —
            // it returns `S_FALSE` when COM is already initialised on the thread,
            // and any genuine failure surfaces on the first device call below.
            let _ = initialize_mta();
            // Signal "started" once the stream is live, *before* the capture loop —
            // separate sender so the loop's own later errors can't clobber it.
            let ready_started = ready_tx.clone();
            let run = || -> Result<(), String> {
                let enumerator = DeviceEnumerator::new().map_err(|e| format!("{e:?}"))?;
                // Default *render* endpoint, captured in loopback.
                let device = enumerator
                    .get_default_device(&Direction::Render)
                    .map_err(|e| format!("{e:?}"))?;
                let mut audio_client = device.get_iaudioclient().map_err(|e| format!("{e:?}"))?;
                let format =
                    WaveFormat::new(32, 32, &SampleType::Float, SR as usize, CH as usize, None);
                let (_def, min_time) = audio_client
                    .get_device_period()
                    .map_err(|e| format!("{e:?}"))?;
                let mode = StreamMode::EventsShared {
                    autoconvert: true,
                    buffer_duration_hns: min_time,
                };
                // Render device + Capture direction → loopback (see wasapi docs).
                audio_client
                    .initialize_client(&format, &Direction::Capture, &mode)
                    .map_err(|e| format!("{e:?}"))?;
                let h_event = audio_client
                    .set_get_eventhandle()
                    .map_err(|e| format!("{e:?}"))?;
                let capture_client = audio_client
                    .get_audiocaptureclient()
                    .map_err(|e| format!("{e:?}"))?;
                audio_client.start_stream().map_err(|e| format!("{e:?}"))?;
                // Setup succeeded — tell the caller loopback is live so it doesn't
                // time out and fall back to mic-only.
                let _ = ready_started.send(Ok(()));

                let mut queue: ByteDeque<u8> = ByteDeque::new();
                loop {
                    // Wait for the audio engine to signal a buffer period is ready
                    // before reading — the canonical WASAPI event-driven pattern.
                    // The 200 ms timeout re-checks `stop` even when audio is silent.
                    let _ = h_event.wait_for_event(200);
                    if stop.load(std::sync::atomic::Ordering::Acquire) {
                        let _ = audio_client.stop_stream();
                        return Ok(());
                    }
                    capture_client
                        .read_from_device_to_deque(&mut queue)
                        .map_err(|e| format!("{e:?}"))?;
                    if queue.len() >= 4 {
                        let bytes: Vec<u8> = queue.drain(..(queue.len() / 4) * 4).collect();
                        let samples: Vec<f32> = bytes
                            .chunks_exact(4)
                            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                            .collect();
                        if !samples.is_empty() && tx.send(samples).is_err() {
                            let _ = audio_client.stop_stream();
                            return Ok(());
                        }
                    }
                }
            };
            // Only setup failures (before `start_stream`) reach here unsignaled;
            // a post-start loop error is sent too but the receiver has already moved
            // on (harmless). Success was already signaled above.
            if let Err(e) = run() {
                let _ = ready_tx.send(Err(e));
            }
        });

        // Wait for the explicit "started" signal; a setup failure surfaces as Err so
        // MixedSource degrades to mic-only. A generous window covers slow COM init.
        match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Err(e)) => Err(AppError::other(format!("WASAPI loopback: {e}"))),
            Ok(Ok(())) => Ok(CaptureHandle {
                format: CaptureFormat {
                    sample_rate: SR,
                    channels: CH,
                },
                rx,
            }),
            Err(_) => Err(AppError::other(
                "WASAPI loopback did not start in time (degrading to mic-only)",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn mixer_is_mic_driven_and_folds_in_loopback() {
        let mut m = Mixer::default();
        m.push_a(&[1.0, 1.0, 1.0]);
        m.push_b(&[1.0, 1.0]); // loopback shorter than mic this tick
        let out = m.drain();
        // (1+1)/2, (1+1)/2, (1+0)/2 — mic length drives, B zero-filled at the tail.
        assert_eq!(out, vec![1.0, 1.0, 0.5]);
        // Leftover B is consumed against the next mic samples.
        m.push_a(&[0.0]);
        m.push_b(&[1.0]);
        assert_eq!(m.drain(), vec![0.5]);
    }

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
