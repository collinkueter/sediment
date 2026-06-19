//! Transcription engine seam (ADR-0017 §2, plan M2/M3).
//!
//! ADR-0017 Q1 resolved to **stay concrete** — no `TranscriptionEngine` trait
//! designed against one real impl and a stub. But the *pipeline* still needs to be
//! testable without a real model, so this defines a minimal [`Transcriber`] seam
//! with two implementations: [`MockTranscriber`] (default build + tests — emits
//! placeholder utterances on a fixed audio cadence) and [`LocalTranscriber`] (the
//! real on-device engine behind the **`local-asr`** feature: a sherpa-onnx
//! streaming-zipformer transducer, the model and API proven by the M0 spike). The
//! seam is intentionally tiny: feed mono-16 kHz samples, get utterances out.
//! Speaker attribution is NOT here — that is diarization + Voiceprints
//! (`core::diarization`), resolved per finalized segment in the capture pipeline.

// M2/M3 transcription seam: the mock + trait are exercised by tests and the real
// engine lands behind `local-asr`; unused in the default lib build, so allow
// dead_code rather than delete the seam.
#![allow(dead_code)]

/// One transcribed span. `is_final` marks an endpoint (a committed segment) vs an
/// in-progress partial. M2's mock only emits finals; a streaming engine (M3) emits
/// partials too.
#[derive(Debug, Clone)]
pub struct Utterance {
    pub text: String,
    pub is_final: bool,
}

/// Consumes mono 16 kHz f32 samples and yields utterances. `Send` so the capture
/// pipeline can own one on its worker thread.
pub trait Transcriber: Send {
    /// Feed a chunk; return any utterances that became available.
    fn accept(&mut self, samples: &[f32]) -> Vec<Utterance>;
    /// End of audio — flush any buffered tail.
    fn finish(&mut self) -> Vec<Utterance>;
}

/// Placeholder transcriber: emits one final utterance per `window` of audio so the
/// pipeline produces visible segments without a model. Used by the default build,
/// tests, and the `audio`-without-`local-asr` configuration.
pub struct MockTranscriber {
    window: usize, // samples per emitted utterance
    acc: usize,    // samples since the last emission
    count: usize,
}

impl MockTranscriber {
    /// `seconds` of audio per emitted utterance.
    pub fn new(seconds: f32) -> Self {
        let window = ((seconds.max(0.1)) * super::audio::TARGET_RATE as f32) as usize;
        Self {
            window: window.max(1),
            acc: 0,
            count: 0,
        }
    }
}

impl Default for MockTranscriber {
    fn default() -> Self {
        Self::new(3.0)
    }
}

impl Transcriber for MockTranscriber {
    fn accept(&mut self, samples: &[f32]) -> Vec<Utterance> {
        self.acc += samples.len();
        let mut out = Vec::new();
        while self.acc >= self.window {
            self.acc -= self.window;
            self.count += 1;
            out.push(Utterance {
                text: format!("(mock utterance {})", self.count),
                is_final: true,
            });
        }
        out
    }

    fn finish(&mut self) -> Vec<Utterance> {
        if self.acc > 0 {
            self.count += 1;
            self.acc = 0;
            vec![Utterance {
                text: format!("(mock tail {})", self.count),
                is_final: true,
            }]
        } else {
            Vec::new()
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Real on-device engine (sherpa-onnx streaming-zipformer), behind `local-asr`.
// API proven by the M0 spike (`spikes/m0-capture-asr/src/asr.rs`): build an
// OnlineRecognizer from the transducer model, feed 16 kHz mono waveform, drain
// ready decode steps, and commit a final on each endpoint. NOT compiled in the
// default build — it links a native lib (`sherpa-onnx-sys`).
// ──────────────────────────────────────────────────────────────────────────

#[cfg(feature = "local-asr")]
mod local {
    use super::{Transcriber, Utterance};
    use crate::core::audio::TARGET_RATE;
    use crate::error::{AppError, AppResult};
    use sherpa_onnx::{OnlineRecognizer, OnlineRecognizerConfig, OnlineStream};

    /// Filesystem paths of a streaming-transducer model (the four files of a
    /// sherpa-onnx streaming-zipformer release). Built by `core::asr_model`.
    pub struct AsrModelPaths {
        pub encoder: String,
        pub decoder: String,
        pub joiner: String,
        pub tokens: String,
        /// ONNX execution provider — `"cpu"` everywhere (the M0 bench found CoreML
        /// gave no speedup on the fp32 graph and `cpu` is the safe default).
        pub provider: String,
    }

    /// The real streaming transcriber. Wraps one `OnlineRecognizer` + its decode
    /// stream; endpoints commit final segments. `Send` because both sherpa handles
    /// are `unsafe impl Send` and the pipeline owns this on its worker thread.
    pub struct LocalTranscriber {
        recognizer: OnlineRecognizer,
        stream: OnlineStream,
    }

    impl LocalTranscriber {
        /// Build the recognizer from a model. Returns an actionable error if the
        /// native create fails (bad/missing model files or provider).
        pub fn new(m: &AsrModelPaths) -> AppResult<Self> {
            let mut config = OnlineRecognizerConfig::default();
            config.model_config.transducer.encoder = Some(m.encoder.clone());
            config.model_config.transducer.decoder = Some(m.decoder.clone());
            config.model_config.transducer.joiner = Some(m.joiner.clone());
            config.model_config.tokens = Some(m.tokens.clone());
            config.model_config.provider = Some(m.provider.clone());
            // Endpointing commits a segment on a natural pause — that boundary is
            // what becomes one `## Transcript` line and one diarization window.
            config.enable_endpoint = true;
            // Beam search over greedy: a small accuracy lift on the live transcript for
            // a modest decode cost, still comfortably real-time (the heavy accuracy work
            // is the offline second pass, `OfflineTranscriber`). ADR-0017 §2.
            config.decoding_method = Some("modified_beam_search".to_string());

            let recognizer = OnlineRecognizer::create(&config).ok_or_else(|| {
                AppError::other(
                    "Failed to create the on-device ASR recognizer. The transcription \
                     model may be missing or corrupt — run ASR model setup.",
                )
            })?;
            let stream = recognizer.create_stream();
            Ok(Self { recognizer, stream })
        }

        /// Decode whatever is buffered and, if the recognizer hit an endpoint,
        /// return the committed text and reset for the next segment. Mirrors the
        /// spike's `drain()`: read the result *before* resetting.
        fn drain(&mut self) -> Vec<Utterance> {
            while self.recognizer.is_ready(&self.stream) {
                self.recognizer.decode(&self.stream);
            }
            if !self.recognizer.is_endpoint(&self.stream) {
                return Vec::new();
            }
            let text = self
                .recognizer
                .get_result(&self.stream)
                .map(|r| r.text)
                .unwrap_or_default();
            self.recognizer.reset(&self.stream);
            let text = text.trim().to_string();
            if text.is_empty() {
                Vec::new()
            } else {
                vec![Utterance {
                    text,
                    is_final: true,
                }]
            }
        }
    }

    impl Transcriber for LocalTranscriber {
        fn accept(&mut self, samples: &[f32]) -> Vec<Utterance> {
            self.stream.accept_waveform(TARGET_RATE as i32, samples);
            self.drain()
        }

        fn finish(&mut self) -> Vec<Utterance> {
            // Flush the tail: signal end-of-input, decode, and commit whatever
            // remains even without a trailing endpoint.
            self.stream.input_finished();
            while self.recognizer.is_ready(&self.stream) {
                self.recognizer.decode(&self.stream);
            }
            let text = self
                .recognizer
                .get_result(&self.stream)
                .map(|r| r.text.trim().to_string())
                .unwrap_or_default();
            if text.is_empty() {
                Vec::new()
            } else {
                vec![Utterance {
                    text,
                    is_final: true,
                }]
            }
        }
    }
}

#[cfg(feature = "local-asr")]
pub use local::{AsrModelPaths, LocalTranscriber};

// ──────────────────────────────────────────────────────────────────────────
// Offline second pass (ADR-0017 §2 two-pass): a non-streaming, high-accuracy
// recognizer run ONCE over the whole buffered meeting at stop. The streaming
// engine above is tuned for sub-second partials; this one is tuned for accuracy,
// and its transcript supersedes the live one in the Meeting note. Behind
// `local-asr` like `LocalTranscriber`. Default model: Parakeet-TDT (NeMo
// transducer); the config shape is model-family-specific but provisioned generically.
// ──────────────────────────────────────────────────────────────────────────

#[cfg(feature = "local-asr")]
mod offline {
    use crate::core::audio::{split_on_silence, TARGET_RATE};
    use crate::error::{AppError, AppResult};
    use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig};

    /// Filesystem paths of an offline transducer model (Parakeet-TDT / NeMo export:
    /// encoder / decoder / joiner / tokens). Built by `core::asr_model`.
    pub struct OfflineModelPaths {
        pub encoder: String,
        pub decoder: String,
        pub joiner: String,
        pub tokens: String,
        pub provider: String,
    }

    /// One accurately-transcribed span from the second pass, timestamped (ms from the
    /// start of the buffer) so the caller can slice the audio for re-diarization.
    #[derive(Debug, Clone)]
    pub struct OfflineSegment {
        pub start_ms: i64,
        pub end_ms: i64,
        pub text: String,
    }

    /// The offline recognizer. Decodes a complete waveform per call (no streaming
    /// state), so one instance transcribes every silence-split range of a meeting.
    pub struct OfflineTranscriber {
        recognizer: OfflineRecognizer,
    }

    impl OfflineTranscriber {
        /// Build the recognizer from a NeMo-transducer model. Actionable error when
        /// the native create fails (missing/corrupt files or a wrong model family).
        pub fn new(m: &OfflineModelPaths) -> AppResult<Self> {
            let mut config = OfflineRecognizerConfig::default();
            config.model_config.transducer.encoder = Some(m.encoder.clone());
            config.model_config.transducer.decoder = Some(m.decoder.clone());
            config.model_config.transducer.joiner = Some(m.joiner.clone());
            config.model_config.tokens = Some(m.tokens.clone());
            config.model_config.provider = Some(m.provider.clone());
            config.model_config.model_type = Some("nemo_transducer".to_string());

            let recognizer = OfflineRecognizer::create(&config).ok_or_else(|| {
                AppError::other(
                    "Failed to create the offline ASR recognizer. The high-accuracy \
                     transcription model may be missing or corrupt — run ASR model setup.",
                )
            })?;
            Ok(Self { recognizer })
        }

        /// Transcribe `samples` (16 kHz mono) into timestamped segments, splitting on
        /// natural pauses so each segment is one diarizable utterance.
        pub fn transcribe(&self, samples: &[f32]) -> Vec<OfflineSegment> {
            let ms = |i: usize| (i as i64 * 1000) / TARGET_RATE as i64;
            let mut out = Vec::new();
            for (start, end) in split_on_silence(samples, TARGET_RATE) {
                let stream = self.recognizer.create_stream();
                stream.accept_waveform(TARGET_RATE as i32, &samples[start..end]);
                self.recognizer.decode(&stream);
                let text = stream
                    .get_result()
                    .map(|r| r.text.trim().to_string())
                    .unwrap_or_default();
                if !text.is_empty() {
                    out.push(OfflineSegment {
                        start_ms: ms(start),
                        end_ms: ms(end),
                        text,
                    });
                }
            }
            out
        }
    }
}

#[cfg(feature = "local-asr")]
pub use offline::{OfflineModelPaths, OfflineSegment, OfflineTranscriber};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_emits_one_utterance_per_window() {
        let rate = super::super::audio::TARGET_RATE as usize;
        let mut t = MockTranscriber::new(1.0); // 1 utterance per 16k samples
                                               // 2.5 s of audio fed in half-second chunks → 2 finals, 0.5 s buffered.
        let half = vec![0.0f32; rate / 2];
        let mut finals = 0;
        for _ in 0..5 {
            finals += t.accept(&half).len();
        }
        assert_eq!(finals, 2);
        // finish() flushes the buffered tail.
        assert_eq!(t.finish().len(), 1);
        assert_eq!(t.finish().len(), 0);
    }

    /// End-to-end proof the REAL engine transcribes real speech: feed the M0
    /// spike's sample WAV through `LocalTranscriber` and assert it returns actual
    /// words. Ignored by default — it needs the native sherpa lib and the model
    /// files in `spikes/m0-capture-asr/`. Run on hardware:
    /// `cargo test --no-default-features --features audio,local-asr \
    ///    real_wav_transcribes -- --ignored --nocapture`
    #[cfg(feature = "local-asr")]
    #[test]
    #[ignore]
    fn real_wav_transcribes() {
        use crate::core::audio::{downmix_to_mono, Resampler};
        use std::path::PathBuf;

        let spike = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("spikes/m0-capture-asr");
        let f = |n: &str| spike.join(n).to_string_lossy().into_owned();
        let paths = super::AsrModelPaths {
            encoder: f("encoder-epoch-99-avg-1-chunk-16-left-128.onnx"),
            decoder: f("decoder-epoch-99-avg-1-chunk-16-left-128.onnx"),
            joiner: f("joiner-epoch-99-avg-1-chunk-16-left-128.onnx"),
            tokens: f("tokens.txt"),
            provider: "cpu".to_string(),
        };
        let mut asr = super::LocalTranscriber::new(&paths).expect("build recognizer");

        // Decode the WAV to interleaved f32, downmix to mono, resample to 16 kHz.
        let mut reader = hound::WavReader::open(spike.join("sample-0.wav")).expect("open wav");
        let spec = reader.spec();
        let interleaved: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Float => reader.samples::<f32>().map(|s| s.unwrap()).collect(),
            hound::SampleFormat::Int => {
                let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
                reader
                    .samples::<i32>()
                    .map(|s| s.unwrap() as f32 / max)
                    .collect()
            }
        };
        let mono = downmix_to_mono(&interleaved, spec.channels);
        let mut resampler = Resampler::new(spec.sample_rate);
        let samples = resampler.process(&mono);

        // Stream it in 100 ms chunks, as live capture would.
        let mut text = String::new();
        for chunk in samples.chunks(1600) {
            for u in asr.accept(chunk) {
                text.push_str(&u.text);
                text.push(' ');
            }
        }
        for u in asr.finish() {
            text.push_str(&u.text);
        }

        println!("ASR transcript: {text:?}");
        let words = text.split_whitespace().count();
        assert!(words >= 3, "expected real words, got {words}: {text:?}");
        assert!(
            text.chars().any(|c| c.is_alphabetic()),
            "transcript has no letters: {text:?}"
        );
    }

    /// The integration that the M0 spike could not catch: the bundled embedder
    /// (`ort`/fastembed, ONNX Runtime via `load-dynamic`) and sherpa ASR (its own
    /// static ONNX Runtime) must run in ONE process without the two runtimes
    /// colliding. Needs `ORT_DYLIB_PATH` set to an onnxruntime dylib and the nomic
    /// model installed. Run on hardware:
    /// `ORT_DYLIB_PATH=/path/to/libonnxruntime.dylib cargo test \
    ///    --no-default-features --features audio,local-asr embedder_and_asr_coexist \
    ///    -- --ignored --nocapture`
    #[cfg(feature = "local-asr")]
    #[test]
    #[ignore]
    fn embedder_and_asr_coexist() {
        // 1. Bundled embedder first — forces ort to init its (dynamic) runtime.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let vec = rt
            .block_on(crate::core::bundled_embed::embed("hello world"))
            .expect("embed");
        assert_eq!(vec.len(), 768, "nomic embedding dim");
        println!("embedder OK: {}-d vector", vec.len());

        // 2. Then sherpa ASR in the same process — must not be clobbered by ort's ORT.
        real_wav_transcribes();
        println!("embedder + ASR coexist OK");
    }
}
