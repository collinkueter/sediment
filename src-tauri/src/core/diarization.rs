//! Speaker diarization + identification (ADR-0017 §6), behind `local-asr`.
//!
//! The capture pipeline hands each *finalized* ASR segment's audio to [`Diarizer`]
//! to answer "who said this". One mechanism does both jobs the ADR splits into
//! diarization and identification:
//!
//!   - extract a **speaker embedding** (WeSpeaker CAM++ ONNX) for the segment;
//!   - match it, by cosine, against a set of running **centroids**. Centroids
//!     pre-seeded from a person's enrolled **Voiceprint** carry that person's name,
//!     so a match *identifies* them (auto-label, ADR §6). Unmatched audio starts a
//!     new `Unknown speaker N` centroid — that is *diarization*. The two are the
//!     same nearest-centroid step over one embedding space.
//!
//! Labels are **liberal** (ADR §6): guess the nearest speaker above a modest
//! threshold, because a wrong transcript label is visible and one rename to fix.
//! The conservative, gated decision (whether to attribute a *Fact* to a named
//! person) lives in the M6 distillation turn, not here.
//!
//! Enrolled-person centroids are held fixed (`known`); unknown speakers' centroids
//! drift as a running mean so a speaker's later segments still match their first.
//! Every centroid is published into a [`SharedCentroids`] map so a hand-rename
//! ("that was Sarah") can persist the centroid as a real Voiceprint.

use crate::core::session::SharedCentroids;
use crate::error::{AppError, AppResult};
use sherpa_onnx::{SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig};

/// Cosine bar to merge a segment into an existing speaker / match a known
/// Voiceprint. Liberal per ADR §6; WeSpeaker CAM++ same-speaker cosine sits well
/// above this, cross-speaker well below.
pub const DIARIZE_THRESHOLD: f32 = 0.5;

/// Shortest segment (in 16 kHz samples ≈ 0.5 s) worth embedding. Below this the
/// extractor has too little audio for a stable vector, so we keep the previous
/// speaker rather than spawn a spurious one.
const MIN_SAMPLES: usize = 8_000;

struct Cluster {
    label: String,
    centroid: Vec<f32>,
    count: f32,
    /// A pre-seeded enrolled Voiceprint — its centroid is authoritative and is not
    /// drifted by in-meeting audio.
    known: bool,
}

/// Resolves a segment's audio to a speaker label. Owns the embedding extractor and
/// the live cluster set; not `Clone`. `Send` (the sherpa handle is) so the capture
/// pipeline can hold it on its worker thread.
pub struct Diarizer {
    extractor: SpeakerEmbeddingExtractor,
    threshold: f32,
    clusters: Vec<Cluster>,
    next_unknown: usize,
    last_label: Option<String>,
    centroids: SharedCentroids,
    relabels: crate::core::session::SharedRelabels,
}

impl Diarizer {
    /// Build a diarizer from the speaker-embedding model at `model_path`, seeding
    /// it with the formation's enrolled Voiceprints (`(name, centroid)`), so a
    /// known voice is named on its first segment. `centroids` is the shared map the
    /// rename flow reads; `relabels` is the queue a live rename pushes into so the
    /// diarizer renames its own clusters.
    pub fn new(
        model_path: &str,
        known: Vec<(String, Vec<f32>)>,
        centroids: SharedCentroids,
        relabels: crate::core::session::SharedRelabels,
    ) -> AppResult<Self> {
        let config = SpeakerEmbeddingExtractorConfig {
            model: Some(model_path.to_string()),
            ..Default::default()
        };
        let extractor = SpeakerEmbeddingExtractor::create(&config).ok_or_else(|| {
            AppError::other(
                "Failed to load the speaker-embedding model (missing or corrupt). \
                 Run ASR model setup.",
            )
        })?;
        let clusters = known
            .into_iter()
            .filter(|(_, c)| !c.is_empty())
            .map(|(label, centroid)| Cluster {
                label,
                centroid,
                count: 1.0,
                known: true,
            })
            .collect();
        Ok(Self {
            extractor,
            threshold: DIARIZE_THRESHOLD,
            clusters,
            next_unknown: 1,
            last_label: None,
            centroids,
            relabels,
        })
    }

    /// Apply any pending live renames to the cluster set, so a speaker named
    /// mid-meeting keeps that name on subsequent segments (and the voice is treated
    /// as a known one, no longer drifting). Drains the shared queue.
    fn apply_relabels(&mut self) {
        let pending: Vec<(String, String)> = match self.relabels.lock() {
            Ok(mut q) if !q.is_empty() => std::mem::take(&mut *q),
            _ => return,
        };
        for (from, to) in pending {
            for c in self.clusters.iter_mut() {
                if c.label == from {
                    c.label = to.clone();
                    c.known = true;
                }
            }
            if self.last_label.as_deref() == Some(&from) {
                self.last_label = Some(to);
            }
        }
    }

    /// Attribute `samples` (16 kHz mono, one finalized segment) to a speaker label.
    /// Returns a known person's name on a Voiceprint match, an existing or fresh
    /// `Unknown speaker N` otherwise, or the previous speaker when the clip is too
    /// short to embed.
    pub fn assign(&mut self, samples: &[f32]) -> String {
        // Pick up any "that was Sarah" renames issued since the last segment.
        self.apply_relabels();
        let label = match self.embed(samples) {
            Some(embedding) => self.assign_embedding(embedding),
            None => self
                .last_label
                .clone()
                .unwrap_or_else(|| self.fresh_unknown_label()),
        };
        self.last_label = Some(label.clone());
        label
    }

    /// Compute the speaker embedding for a segment, or `None` when there is too
    /// little audio or the model declines.
    fn embed(&self, samples: &[f32]) -> Option<Vec<f32>> {
        if samples.len() < MIN_SAMPLES {
            return None;
        }
        let stream = self.extractor.create_stream()?;
        stream.accept_waveform(crate::core::audio::TARGET_RATE as i32, samples);
        stream.input_finished();
        if !self.extractor.is_ready(&stream) {
            return None;
        }
        self.extractor.compute(&stream)
    }

    /// Nearest-centroid assignment over one embedding: merge into the best cluster
    /// above threshold (drifting unknown centroids), else open a new speaker.
    fn assign_embedding(&mut self, embedding: Vec<f32>) -> String {
        let mut best: Option<(usize, f32)> = None;
        for (i, c) in self.clusters.iter().enumerate() {
            let score = cosine(&embedding, &c.centroid);
            if score >= self.threshold && best.map(|(_, b)| score > b).unwrap_or(true) {
                best = Some((i, score));
            }
        }
        let label = match best {
            Some((i, _)) => {
                let c = &mut self.clusters[i];
                if !c.known {
                    merge_centroid(&mut c.centroid, &mut c.count, &embedding);
                }
                c.label.clone()
            }
            None => {
                let label = self.fresh_unknown_label();
                self.clusters.push(Cluster {
                    label: label.clone(),
                    centroid: embedding.clone(),
                    count: 1.0,
                    known: false,
                });
                label
            }
        };
        // Publish the current centroid for this label so a rename can enroll it.
        if let Some(c) = self.clusters.iter().find(|c| c.label == label) {
            if let Ok(mut map) = self.centroids.lock() {
                map.insert(label.clone(), c.centroid.clone());
            }
        }
        label
    }

    fn fresh_unknown_label(&mut self) -> String {
        let label = format!("Unknown speaker {}", self.next_unknown);
        self.next_unknown += 1;
        label
    }
}

/// Cosine similarity; 0.0 when either side is empty / a different length / a zero
/// vector. Same maths as `memory::cosine`, kept local so the modules stay
/// independent (the choice `meeting_note` made for its section helpers).
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut na = 0.0;
    let mut nb = 0.0;
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// Fold `sample` into a running-mean `centroid` of `count` prior samples.
fn merge_centroid(centroid: &mut [f32], count: &mut f32, sample: &[f32]) {
    if centroid.len() != sample.len() {
        return;
    }
    let n = *count;
    for (c, s) in centroid.iter_mut().zip(sample) {
        *c = (*c * n + s) / (n + 1.0);
    }
    *count = n + 1.0;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_basics() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn merge_centroid_is_running_mean() {
        let mut c = vec![2.0, 0.0];
        let mut n = 1.0;
        merge_centroid(&mut c, &mut n, &[0.0, 2.0]);
        assert_eq!(c, vec![1.0, 1.0]);
        assert_eq!(n, 2.0);
        merge_centroid(&mut c, &mut n, &[2.0, 2.0]);
        // (1*2 + 2)/3 = 1.333..., (1*2 + 2)/3 = 1.333...
        assert!((c[0] - 4.0 / 3.0).abs() < 1e-6);
    }
}
