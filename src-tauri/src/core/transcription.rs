//! Transcription engine seam (ADR-0016 §2, plan M2/M3).
//!
//! ADR-0016 Q1 resolved to **stay concrete** — no `TranscriptionEngine` trait
//! designed against one real impl and a stub. But the *pipeline* still needs to be
//! testable without a real model, so this defines a minimal [`Transcriber`] seam
//! with two implementations: [`MockTranscriber`] (default build + tests — emits
//! placeholder utterances on a fixed audio cadence) and, in M3, a real on-device
//! engine behind the `local-asr` feature (sherpa-onnx streaming zipformer; see the
//! M0 spike). The seam is intentionally tiny: feed mono-16 kHz samples, get
//! utterances out. Speaker attribution is NOT here — that is diarization +
//! Voiceprints (M4); M2 attributes every utterance to a single placeholder speaker.

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
}
