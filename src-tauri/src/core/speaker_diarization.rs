//! High-accuracy offline speaker diarization (ADR-0017 §A2), behind `local-asr`.
//!
//! The live `core::diarization::Diarizer` answers "who said this" one ASR segment
//! at a time by clustering speaker embeddings greedily. That is good enough for the
//! *live* transcript but structurally weak: the diarization window is the ASR
//! endpoint (so a turn-change mid-segment is invisible), and a first-come centroid
//! that absorbs one wrong segment stays wrong for the rest of the meeting.
//!
//! This module is the second-pass upgrade the ADR named and deferred: a proper
//! **pyannote segmentation → speaker embedding → agglomerative clustering** pipeline
//! (`sherpa-onnx`'s `OfflineSpeakerDiarization`). It sees the *whole* meeting at
//! once, finds speaker-turn boundaries with a segmentation model (not ASR pauses),
//! and clusters globally — so it splits speakers the live path merged and merges
//! segments the live path split. It runs only at stop, over the buffered audio, and
//! its output supersedes the live transcript (`replace_transcript`).
//!
//! Diarization here is *anonymous* — it returns turns tagged with a local speaker
//! index. Putting a **name** on each index (matching enrolled Voiceprints) stays in
//! `core::diarization`, reused over each cluster's audio by the second pass.

use crate::core::diarization::{cosine, embed_clip, DIARIZE_THRESHOLD};
use crate::error::{AppError, AppResult};
use sherpa_onnx::{
    FastClusteringConfig, OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig,
    OfflineSpeakerSegmentationModelConfig, OfflineSpeakerSegmentationPyannoteModelConfig,
    SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig,
};

/// Cosine bar the clustering uses to decide two embeddings are the same speaker.
/// Matches the live `DIARIZE_THRESHOLD` so the two stages agree on what "same
/// voice" means; the global clustering is far more robust at the same bar.
const CLUSTER_THRESHOLD: f32 = 0.5;

/// Shortest speech run kept as its own turn (seconds) — drops sub-word blips.
const MIN_DURATION_ON: f32 = 0.3;
/// Shortest silence that ends a turn (seconds) — a pause below this stays one turn.
const MIN_DURATION_OFF: f32 = 0.5;

/// One anonymous speaker turn: a `[start_ms, end_ms)` span attributed to a local
/// speaker index (`0..num_speakers`). Names are resolved later, per cluster.
#[derive(Debug, Clone, Copy)]
pub struct DiarTurn {
    pub start_ms: i64,
    pub end_ms: i64,
    pub speaker: i32,
}

/// Owns one `OfflineSpeakerDiarization` pipeline (segmentation + embedding +
/// clustering). `Send`/`Sync` via the underlying handle, so the second pass can run
/// it on a blocking thread.
pub struct OfflineDiarizer {
    inner: OfflineSpeakerDiarization,
}

impl OfflineDiarizer {
    /// Build the pipeline from a pyannote `segmentation_model` and a WeSpeaker
    /// `embedding_model` (the same file the live diarizer uses). Errors actionably
    /// when either model is missing or the wrong shape.
    pub fn new(segmentation_model: &str, embedding_model: &str) -> AppResult<Self> {
        let config = OfflineSpeakerDiarizationConfig {
            segmentation: OfflineSpeakerSegmentationModelConfig {
                pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
                    model: Some(segmentation_model.to_string()),
                },
                provider: Some("cpu".to_string()),
                ..Default::default()
            },
            embedding: SpeakerEmbeddingExtractorConfig {
                model: Some(embedding_model.to_string()),
                provider: Some("cpu".to_string()),
                ..Default::default()
            },
            clustering: FastClusteringConfig {
                // -1 = estimate the number of speakers from the data (we never know
                // it up front); merge clusters whose cosine clears the threshold.
                num_clusters: -1,
                threshold: CLUSTER_THRESHOLD,
            },
            min_duration_on: MIN_DURATION_ON,
            min_duration_off: MIN_DURATION_OFF,
        };
        let inner = OfflineSpeakerDiarization::create(&config).ok_or_else(|| {
            AppError::other(
                "Failed to load the speaker-diarization pipeline (segmentation or \
                 speaker model missing or corrupt). Run ASR model setup.",
            )
        })?;
        Ok(Self { inner })
    }

    /// Diarize a complete 16 kHz mono buffer into anonymous speaker turns, sorted by
    /// start time. Empty when the pipeline finds no speech (the caller leaves the
    /// live transcript in place).
    pub fn diarize(&self, samples: &[f32]) -> Vec<DiarTurn> {
        // Segment `start`/`end` are in seconds (pyannote convention).
        let Some(result) = self.inner.process(samples) else {
            return Vec::new();
        };
        result
            .sort_by_start_time()
            .into_iter()
            .map(|s| DiarTurn {
                start_ms: (s.start * 1000.0).round() as i64,
                end_ms: (s.end * 1000.0).round() as i64,
                speaker: s.speaker,
            })
            .collect()
    }
}

/// Cosine bar above which a Voiceprint match is asserted outright (the speaker is
/// named in the transcript). WeSpeaker CAM++ same-speaker cosine sits comfortably
/// here; below it, down to [`crate::core::diarization::DIARIZE_THRESHOLD`], the match
/// is only a **suggestion** the user confirms ("possibly Dana") rather than a silent
/// assertion — confidence asymmetry, ADR-0017 §6.
const ASSERT_BAR: f32 = 0.62;

/// The outcome of matching a cluster against the seeds, split by confidence so the
/// caller can assert a strong match, *suggest* a borderline one, or leave it unknown.
#[derive(Debug, Clone)]
pub enum SpeakerMatch {
    /// Strong match — name the speaker outright.
    Certain(String),
    /// Borderline match — keep the speaker `Unknown` but offer this name for the user
    /// to confirm. Carries the cosine score for telemetry/sorting.
    Likely(String, f32),
    /// No seed cleared the suggest floor — a genuinely unknown voice.
    Unknown,
}

/// Puts a **name** on an anonymous diarization cluster by cosine-matching its
/// representative audio against enrolled/known Voiceprints (the *identification*
/// half of ADR-0017 §6, reused for the second pass). Owns its own embedding
/// extractor; a non-match yields `Unknown` so the caller can label it `Unknown
/// speaker N`. Stateless per call — distinct clusters are matched independently
/// (no drift, no order bias), unlike the live `Diarizer`.
pub struct SpeakerNamer {
    extractor: SpeakerEmbeddingExtractor,
    seeds: Vec<(String, Vec<f32>)>,
}

impl SpeakerNamer {
    /// Build from the WeSpeaker `embedding_model` and the formation's known
    /// Voiceprints (`(name, centroid)`) plus this meeting's named live speakers.
    pub fn new(embedding_model: &str, mut seeds: Vec<(String, Vec<f32>)>) -> AppResult<Self> {
        let config = SpeakerEmbeddingExtractorConfig {
            model: Some(embedding_model.to_string()),
            provider: Some("cpu".to_string()),
            ..Default::default()
        };
        let extractor = SpeakerEmbeddingExtractor::create(&config).ok_or_else(|| {
            AppError::other("Failed to load the speaker-embedding model (missing or corrupt).")
        })?;
        // Seeds compare on the unit sphere, like live embeddings.
        for (_, centroid) in seeds.iter_mut() {
            crate::core::diarization::l2_normalize(centroid);
        }
        seeds.retain(|(_, c)| !c.is_empty());
        Ok(Self { extractor, seeds })
    }

    /// Classify the cluster whose representative audio is `samples` by confidence:
    /// `Certain` above [`ASSERT_BAR`], `Likely` down to
    /// [`crate::core::diarization::DIARIZE_THRESHOLD`], else `Unknown` (also when the
    /// clip is too weak to embed). The split is what lets the second pass assert a
    /// strong voice but only *suggest* a shaky one.
    pub fn classify(&self, samples: &[f32]) -> SpeakerMatch {
        let Some(embedding) = embed_clip(&self.extractor, samples) else {
            return SpeakerMatch::Unknown;
        };
        let mut best: Option<(&str, f32)> = None;
        for (name, centroid) in &self.seeds {
            let score = cosine(&embedding, centroid);
            if score >= DIARIZE_THRESHOLD && best.map(|(_, b)| score > b).unwrap_or(true) {
                best = Some((name.as_str(), score));
            }
        }
        match best {
            Some((name, score)) if score >= ASSERT_BAR => SpeakerMatch::Certain(name.to_string()),
            Some((name, score)) => SpeakerMatch::Likely(name.to_string(), score),
            None => SpeakerMatch::Unknown,
        }
    }
}
