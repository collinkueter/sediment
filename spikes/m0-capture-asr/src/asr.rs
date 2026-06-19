//! Thin wrapper over the official `sherpa-onnx` streaming (online) recognizer.
//!
//! API confirmed against sherpa-onnx 1.13.x and its
//! `rust-api-examples/examples/streaming_zipformer_microphone.rs`:
//!   - `OnlineRecognizerConfig::default()`, set `model_config.transducer.{encoder,
//!     decoder,joiner}` + `model_config.tokens` + `model_config.provider`
//!   - `OnlineRecognizer::create(&config)`, `recognizer.create_stream()`
//!   - feed: `stream.accept_waveform(sample_rate, &samples)`
//!   - loop: `while recognizer.is_ready(&stream) { recognizer.decode(&stream) }`
//!   - read: `recognizer.get_result(&stream).text`
//!   - endpoint: `recognizer.is_endpoint(&stream)` then `recognizer.reset(&stream)`
//!   - flush: `stream.input_finished()`
//!
//! This wires the *natively streaming* path (streaming zipformer transducer).
//! NOTE for ADR-0017 Q5: NVIDIA **Parakeet-TDT** in sherpa-onnx is an *offline*
//! recognizer run under VAD-based *simulated* streaming (interim every ~0.2 s
//! within a speech segment) — not a true online model. To benchmark that path,
//! run sherpa-onnx's own `parakeet_tdt_simulate_streaming_microphone` example and
//! record its interim latency; compare against the numbers this spike produces for
//! the streaming-zipformer path. That comparison is the real V1 model decision.

use anyhow::Result;
use sherpa_onnx::{OnlineRecognizer, OnlineRecognizerConfig};

pub struct ModelPaths {
    pub encoder: String,
    pub decoder: String,
    pub joiner: String,
    pub tokens: String,
    pub provider: String, // "cpu", "coreml" (macOS), "cuda", ...
}

pub struct Asr {
    recognizer: OnlineRecognizer,
    stream: sherpa_onnx::OnlineStream,
}

impl Asr {
    pub fn new(m: &ModelPaths) -> Result<Self> {
        let mut config = OnlineRecognizerConfig::default();
        config.model_config.transducer.encoder = Some(m.encoder.clone());
        config.model_config.transducer.decoder = Some(m.decoder.clone());
        config.model_config.transducer.joiner = Some(m.joiner.clone());
        config.model_config.tokens = Some(m.tokens.clone());
        config.model_config.provider = Some(m.provider.clone());
        config.enable_endpoint = true;
        config.decoding_method = Some("greedy_search".to_string());

        let recognizer = OnlineRecognizer::create(&config)
            .ok_or_else(|| anyhow::anyhow!("failed to create OnlineRecognizer (check model paths/provider)"))?;
        let stream = recognizer.create_stream();
        Ok(Self { recognizer, stream })
    }

    /// Feed one chunk of 16 kHz mono samples and drain ready decode steps.
    /// Returns the current (partial) hypothesis text.
    pub fn feed(&mut self, samples: &[f32]) -> String {
        self.stream.accept_waveform(super::audio::TARGET_RATE, samples);
        self.drain()
    }

    /// Signal end-of-input and drain the tail.
    pub fn finish(&mut self) -> String {
        self.stream.input_finished();
        self.drain()
    }

    fn drain(&mut self) -> String {
        while self.recognizer.is_ready(&self.stream) {
            self.recognizer.decode(&self.stream);
        }
        let text = self
            .recognizer
            .get_result(&self.stream)
            .map(|r| r.text)
            .unwrap_or_default();
        // On endpoint, the model has committed a segment; reset so the next
        // segment decodes fresh. (We return the text *before* resetting.)
        if self.recognizer.is_endpoint(&self.stream) {
            self.recognizer.reset(&self.stream);
        }
        text
    }
}
