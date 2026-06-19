//! On-device ASR + speaker-embedding model provisioning (ADR-0017 §2/§6, plan M3).
//!
//! Mirrors `core::bundled_embed` (ADR-0016): the runtime is **local-only** —
//! models load strictly from a fixed, app-owned directory, never fetched inside
//! the capture hot path. Acquiring them (download or folder import) is an explicit
//! setup step in `commands::asr`. Two models live here:
//!
//!   - the **streaming-zipformer transducer** that `LocalTranscriber` runs (four
//!     files: encoder / decoder / joiner / tokens — the M0-benched
//!     `sherpa-onnx-streaming-zipformer-en-2023-06-26`), and
//!   - a **speaker-embedding** model (one ONNX file, a WeSpeaker CAM++ export)
//!     that `core::diarization` runs to attribute each segment to a speaker.
//!
//! Both are validated by *loading a session* before they are promoted, so a
//! partial or corrupt download is rejected at setup, not at record time. Gated on
//! `local-asr` because validation links the native sherpa-onnx runtime.

use crate::core::transcription::{AsrModelPaths, LocalTranscriber};
use crate::error::{AppError, AppResult};
use std::path::{Path, PathBuf};

// ──────────────────────────────────────────────────────────────────────────
// Locations
// ──────────────────────────────────────────────────────────────────────────

/// The user's home directory (`HOME`, or `USERPROFILE` on Windows). Matches
/// `bundled_embed::home_dir` so all on-device models share `~/.sediment/models`.
fn home_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
}

fn models_root() -> PathBuf {
    home_dir().join(".sediment").join("models")
}

/// Directory holding the streaming-ASR model files.
pub fn asr_dir() -> PathBuf {
    models_root().join(ASR_MODEL_NAME)
}

/// Directory holding the speaker-embedding model file.
pub fn speaker_dir() -> PathBuf {
    models_root().join("speaker-embedding")
}

fn asr_staging() -> PathBuf {
    models_root().join(".staging-asr")
}

fn speaker_staging() -> PathBuf {
    models_root().join(".staging-speaker")
}

// ──────────────────────────────────────────────────────────────────────────
// Model identity — the M0-benched release and a compact speaker model
// ──────────────────────────────────────────────────────────────────────────

/// The streaming-zipformer release (English), benched in M0 at RTF ≈ 0.05 on
/// Apple Silicon (`docs/plans/m0-benchmark-results.md`).
pub const ASR_MODEL_NAME: &str = "sherpa-onnx-streaming-zipformer-en-2023-06-26";

/// The four files of the streaming-transducer model, in the release's own layout
/// (flat). Order is irrelevant; presence of all four is what `present` checks.
pub const ASR_FILES: [&str; 4] = [
    "encoder-epoch-99-avg-1-chunk-16-left-128.onnx",
    "decoder-epoch-99-avg-1-chunk-16-left-128.onnx",
    "joiner-epoch-99-avg-1-chunk-16-left-128.onnx",
    "tokens.txt",
];

/// Where the ASR files are fetched from when no mirror is set. The four files
/// live directly under this base (`<base>/<file>`).
pub const ASR_BASE_URL: &str =
    "https://huggingface.co/csukuangfj/sherpa-onnx-streaming-zipformer-en-2023-06-26/resolve/main";

/// The speaker-embedding model file (WeSpeaker CAM++, English VoxCeleb, 16 kHz,
/// 192-d). Compact (~28 MB) and runs comfortably alongside ASR in real time.
pub const SPEAKER_FILE: &str = "wespeaker_en_voxceleb_CAM++.onnx";

/// Full default URL for the speaker model (a single file on the sherpa-onnx
/// maintainer's HuggingFace model repo). The `++` in the filename is percent-
/// encoded for the URL; on disk it stays literal ([`SPEAKER_FILE`]).
pub const SPEAKER_URL: &str =
    "https://huggingface.co/csukuangfj/speaker-embedding-models/resolve/main/wespeaker_en_voxceleb_CAM%2B%2B.onnx";

/// `SEDIMENT_ASR_MODEL_BASE_URL` overrides [`ASR_BASE_URL`] (mirror / offline host).
pub fn asr_base_url() -> String {
    std::env::var("SEDIMENT_ASR_MODEL_BASE_URL")
        .ok()
        .map(|u| u.trim().trim_end_matches('/').to_string())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| ASR_BASE_URL.to_string())
}

/// `SEDIMENT_SPEAKER_MODEL_URL` overrides [`SPEAKER_URL`].
pub fn speaker_url() -> String {
    std::env::var("SEDIMENT_SPEAKER_MODEL_URL")
        .ok()
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| SPEAKER_URL.to_string())
}

// ──────────────────────────────────────────────────────────────────────────
// Presence + path resolution
// ──────────────────────────────────────────────────────────────────────────

/// True when all four ASR files are present on disk.
pub fn asr_present() -> bool {
    let dir = asr_dir();
    ASR_FILES.iter().all(|f| dir.join(f).is_file())
}

/// True when the speaker-embedding model is present on disk.
pub fn speaker_present() -> bool {
    speaker_dir().join(SPEAKER_FILE).is_file()
}

/// Resolve the installed ASR files into [`AsrModelPaths`] for `LocalTranscriber`.
/// Errors (actionably) when the model is not installed.
pub fn asr_paths() -> AppResult<AsrModelPaths> {
    let dir = asr_dir();
    let p = |f: &str| -> AppResult<String> {
        let path = dir.join(f);
        if path.is_file() {
            Ok(path.to_string_lossy().into_owned())
        } else {
            Err(AppError::other(format!(
                "On-device ASR model file missing: {f}. Run ASR model setup to \
                 download or import the transcription model."
            )))
        }
    };
    Ok(AsrModelPaths {
        encoder: p(ASR_FILES[0])?,
        decoder: p(ASR_FILES[1])?,
        joiner: p(ASR_FILES[2])?,
        tokens: p(ASR_FILES[3])?,
        provider: "cpu".to_string(),
    })
}

/// Absolute path of the installed speaker-embedding model, or an actionable error.
pub fn speaker_model_path() -> AppResult<String> {
    let path = speaker_dir().join(SPEAKER_FILE);
    if path.is_file() {
        Ok(path.to_string_lossy().into_owned())
    } else {
        Err(AppError::other(
            "On-device speaker model missing. Run ASR model setup to download or \
             import the speaker-embedding model.",
        ))
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Validation (load a session) — rejects a bad/partial pack up front
// ──────────────────────────────────────────────────────────────────────────

/// Validate the ASR files in `dir` by building a `LocalTranscriber` from them.
fn validate_asr_dir(dir: &Path) -> AppResult<()> {
    let p = |f: &str| dir.join(f).to_string_lossy().into_owned();
    let paths = AsrModelPaths {
        encoder: p(ASR_FILES[0]),
        decoder: p(ASR_FILES[1]),
        joiner: p(ASR_FILES[2]),
        tokens: p(ASR_FILES[3]),
        provider: "cpu".to_string(),
    };
    LocalTranscriber::new(&paths).map(|_| ())
}

/// Validate the speaker model in `dir` by creating a `SpeakerEmbeddingExtractor`.
fn validate_speaker_dir(dir: &Path) -> AppResult<()> {
    use sherpa_onnx::{SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig};
    let model = dir.join(SPEAKER_FILE);
    let config = SpeakerEmbeddingExtractorConfig {
        model: Some(model.to_string_lossy().into_owned()),
        ..Default::default()
    };
    SpeakerEmbeddingExtractor::create(&config)
        .map(|_| ())
        .ok_or_else(|| {
            AppError::other("Speaker-embedding model failed to load (missing or corrupt).")
        })
}

// ──────────────────────────────────────────────────────────────────────────
// Promote (atomic install) + import-from-folder, mirroring bundled_embed
// ──────────────────────────────────────────────────────────────────────────

fn promote(staging: &Path, target: &Path) -> AppResult<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AppError::other(format!("create models dir: {e}")))?;
    }
    if target.exists() {
        std::fs::remove_dir_all(target)
            .map_err(|e| AppError::other(format!("clear previous model: {e}")))?;
    }
    std::fs::rename(staging, target)
        .map_err(|e| AppError::other(format!("install model files: {e}")))
}

/// Validate the staged ASR files and atomically promote them to [`asr_dir`].
pub async fn promote_asr_staging() -> AppResult<()> {
    let staging = asr_staging();
    let s = staging.clone();
    tokio::task::spawn_blocking(move || validate_asr_dir(&s))
        .await
        .map_err(|e| AppError::other(format!("validate ASR model join: {e}")))??;
    promote(&staging, &asr_dir())
}

/// Validate the staged speaker model and atomically promote it to [`speaker_dir`].
pub async fn promote_speaker_staging() -> AppResult<()> {
    let staging = speaker_staging();
    let s = staging.clone();
    tokio::task::spawn_blocking(move || validate_speaker_dir(&s))
        .await
        .map_err(|e| AppError::other(format!("validate speaker model join: {e}")))??;
    promote(&staging, &speaker_dir())
}

/// Install the ASR model from a user-chosen folder (offline path). The folder must
/// contain the four files by basename (the release layout). Validated before
/// install. Reusable to seed the model dir from the M0 spike folder.
pub async fn import_asr_from_dir(src: PathBuf) -> AppResult<()> {
    let staging = asr_staging();
    let staging_for_copy = staging.clone();
    tokio::task::spawn_blocking(move || -> AppResult<()> {
        stage_copy(&src, &staging_for_copy, &ASR_FILES)
    })
    .await
    .map_err(|e| AppError::other(format!("import ASR model join: {e}")))??;
    promote_asr_staging().await
}

/// Install the speaker model from a user-chosen folder (offline path).
pub async fn import_speaker_from_dir(src: PathBuf) -> AppResult<()> {
    let staging = speaker_staging();
    let staging_for_copy = staging.clone();
    tokio::task::spawn_blocking(move || -> AppResult<()> {
        stage_copy(&src, &staging_for_copy, &[SPEAKER_FILE])
    })
    .await
    .map_err(|e| AppError::other(format!("import speaker model join: {e}")))??;
    promote_speaker_staging().await
}

/// Copy each required file from `src` (by basename, searched one level deep) into
/// a fresh `staging` dir.
fn stage_copy(src: &Path, staging: &Path, files: &[&str]) -> AppResult<()> {
    if staging.exists() {
        std::fs::remove_dir_all(staging)
            .map_err(|e| AppError::other(format!("clear staging dir: {e}")))?;
    }
    std::fs::create_dir_all(staging)
        .map_err(|e| AppError::other(format!("create staging dir: {e}")))?;
    for &rel in files {
        let source = find_source(src, rel).ok_or_else(|| {
            AppError::other(format!(
                "Model folder is missing {rel}. It must contain {}.",
                files.join(", ")
            ))
        })?;
        std::fs::copy(&source, staging.join(rel))
            .map_err(|e| AppError::other(format!("copy {rel}: {e}")))?;
    }
    Ok(())
}

/// Find `name` directly in `src` or one subdirectory deep (release archives often
/// nest the files in a folder named after the model).
fn find_source(src: &Path, name: &str) -> Option<PathBuf> {
    let direct = src.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    let entries = std::fs::read_dir(src).ok()?;
    for entry in entries.flatten() {
        let candidate = entry.path().join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// The staging dir for the ASR download (filled file-by-file by the command, then
/// promoted). Exposed so the streaming downloader can write into it.
pub fn asr_staging_dir() -> PathBuf {
    asr_staging()
}

/// The staging dir for the speaker-model download.
pub fn speaker_staging_dir() -> PathBuf {
    speaker_staging()
}
