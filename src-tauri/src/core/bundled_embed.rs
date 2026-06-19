//! In-process embedding model (ADR-0014, ADR-0016) — local semantic search
//! with **no Ollama daemon** and **no runtime network**.
//!
//! Wraps fastembed's `nomic-embed-text-v1.5` (a 768-d ONNX model, matching the
//! existing HNSW index dimension in `core::memory`). Unlike the original
//! implementation, the weights are **not** fetched from Hugging Face on first
//! use: the model files live in a fixed, app-owned directory and are loaded
//! strictly from disk via fastembed's user-defined-model API. Acquiring those
//! files (download or import) happens only during explicit setup — see
//! `commands::models`. This is the "local-only line in the sand": indexing and
//! search never reach out to a model host.
//!
//! The model is loaded lazily once per process and held behind a `Mutex`; the
//! same app-owned directory is shared between the main process and the
//! `--mcp-stdio` subprocess. The CPU-bound `embed` runs on a blocking thread so
//! it never stalls the async runtime.

use crate::error::{AppError, AppResult};
use fastembed::{
    InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

static MODEL: OnceLock<Mutex<TextEmbedding>> = OnceLock::new();

/// The five files fastembed needs to build the model from disk, in the layout
/// of the `nomic-ai/nomic-embed-text-v1.5` repo. Loading these with mean pooling
/// and no quantization reproduces the exact vectors fastembed produced when it
/// fetched the same model from Hugging Face (768-d), so an existing bundled
/// index stays valid — no re-index needed.
pub const MODEL_FILES: [&str; 5] = [
    "onnx/model.onnx",
    "tokenizer.json",
    "config.json",
    "special_tokens_map.json",
    "tokenizer_config.json",
];

/// The user's home directory (`HOME` on macOS/Linux, `USERPROFILE` on Windows).
fn home_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
}

/// Where the on-device model files live. Fixed and app-owned so the main process
/// and the MCP subprocess resolve the same directory.
pub fn model_dir() -> PathBuf {
    home_dir()
        .join(".sediment")
        .join("models")
        .join("nomic-embed-text-v1.5")
}

/// A scratch directory the downloader/importer fills before atomically promoting
/// it into `model_dir`, so a half-finished acquisition is never loaded.
pub fn staging_dir() -> PathBuf {
    home_dir()
        .join(".sediment")
        .join("models")
        .join(".staging-nomic-embed-text-v1.5")
}

/// True when every required model file is present in `model_dir` — the signal
/// the launch-time readiness check and the setup screen use.
pub fn present() -> bool {
    let dir = model_dir();
    MODEL_FILES.iter().all(|f| dir.join(f).is_file())
}

/// Build a `TextEmbedding` from the model files in `dir`, reading them into
/// memory. Mean pooling + no quantization match `NomicEmbedTextV15`'s defaults,
/// so the vectors are identical to the Hugging-Face-fetched build.
fn build_from(dir: &Path) -> AppResult<TextEmbedding> {
    let read = |rel: &str| -> AppResult<Vec<u8>> {
        std::fs::read(dir.join(rel)).map_err(|e| {
            AppError::other(format!(
                "On-device model file missing or unreadable: {rel} ({e}). Run model \
                 setup to download or import the model, or switch to keyword search \
                 in Settings."
            ))
        })
    };
    let onnx = read("onnx/model.onnx")?;
    let tokenizer_files = TokenizerFiles {
        tokenizer_file: read("tokenizer.json")?,
        config_file: read("config.json")?,
        special_tokens_map_file: read("special_tokens_map.json")?,
        tokenizer_config_file: read("tokenizer_config.json")?,
    };
    let model = UserDefinedEmbeddingModel::new(onnx, tokenizer_files).with_pooling(Pooling::Mean);
    TextEmbedding::try_new_from_user_defined(model, InitOptionsUserDefined::default())
        .map_err(|e| AppError::other(format!("init on-device embedder: {e}")))
}

/// Get (or lazily build) the process-wide model from `model_dir`. The first call
/// loads the ONNX session (slow), so callers invoke this from a blocking thread.
/// Returns an actionable error when the files are absent.
fn model() -> AppResult<&'static Mutex<TextEmbedding>> {
    if let Some(existing) = MODEL.get() {
        return Ok(existing);
    }
    // Under `local-asr`, `ort` loads ONNX Runtime dynamically — make sure
    // `ORT_DYLIB_PATH` points at the provisioned lib before the first session build.
    #[cfg(feature = "local-asr")]
    crate::core::ort_runtime::set_env_if_present();
    let embedder = build_from(&model_dir())?;
    // A concurrent racer may have set it first — either way `get` succeeds.
    let _ = MODEL.set(Mutex::new(embedder));
    MODEL
        .get()
        .ok_or_else(|| AppError::other("on-device embedder set race"))
}

/// Embed `text` into a 768-d vector with the in-process model. Errors clearly if
/// the model files are not installed; work runs on a blocking thread.
pub async fn embed(text: &str) -> AppResult<Vec<f32>> {
    let owned = text.to_string();
    tokio::task::spawn_blocking(move || {
        let mutex = model()?;
        let embedder = mutex
            .lock()
            .map_err(|_| AppError::other("bundled embedder mutex poisoned"))?;
        let mut out = embedder
            .embed(vec![owned.as_str()], None)
            .map_err(|e| AppError::other(format!("bundled embed: {e}")))?;
        out.pop()
            .ok_or_else(|| AppError::other("bundled embedder returned no vector"))
    })
    .await
    .map_err(|e| AppError::other(format!("bundled embed task join: {e}")))?
}

/// Eagerly load the model so the first real search doesn't pay the load cost.
/// Requires the files to be installed — call after a successful download/import.
pub async fn warmup() -> AppResult<()> {
    embed("warmup").await.map(|_| ())
}

/// Validate the files in `dir` by actually loading a session from them. Used to
/// confirm a freshly downloaded/imported pack is complete and well-formed before
/// it is promoted into `model_dir`. Runs on a blocking thread.
pub async fn validate_dir(dir: PathBuf) -> AppResult<()> {
    tokio::task::spawn_blocking(move || build_from(&dir).map(|_| ()))
        .await
        .map_err(|e| AppError::other(format!("validate model task join: {e}")))?
}

/// Validate `staging_dir` and, if it loads, atomically promote it to `model_dir`
/// (replacing any previous install). On failure the staging dir is left for
/// inspection and `model_dir` is untouched.
///
/// Note: if a model was already loaded this process, the in-memory session is
/// not swapped — a model *replacement* takes effect on the next launch. A
/// first-time install in the same session loads fine (the session was never
/// built because the files were absent).
pub async fn promote_staging() -> AppResult<()> {
    let staging = staging_dir();
    validate_dir(staging.clone()).await?;
    let target = model_dir();
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AppError::other(format!("create models dir: {e}")))?;
    }
    // Replace atomically where possible: drop the old dir, then rename staging in.
    if target.exists() {
        std::fs::remove_dir_all(&target)
            .map_err(|e| AppError::other(format!("clear previous model: {e}")))?;
    }
    std::fs::rename(&staging, &target)
        .map_err(|e| AppError::other(format!("install model files: {e}")))
}

/// Copy a model pack from a user-chosen folder into `staging_dir`, then promote
/// it. Accepts either the repo layout (`onnx/model.onnx` plus the four JSON
/// files at the root) or a flat folder (all files by basename), so a hand-
/// assembled offline pack works.
pub async fn install_from_dir(src: PathBuf) -> AppResult<()> {
    let staging = staging_dir();
    // Stage on a blocking thread (file copies), then validate+promote.
    let staging_for_copy = staging.clone();
    tokio::task::spawn_blocking(move || -> AppResult<()> {
        if staging_for_copy.exists() {
            std::fs::remove_dir_all(&staging_for_copy)
                .map_err(|e| AppError::other(format!("clear staging dir: {e}")))?;
        }
        for rel in MODEL_FILES {
            let source = find_source(&src, rel).ok_or_else(|| {
                AppError::other(format!(
                    "Model folder is missing {rel}. It must contain {}.",
                    MODEL_FILES.join(", ")
                ))
            })?;
            let dest = staging_for_copy.join(rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| AppError::other(format!("create {}: {e}", parent.display())))?;
            }
            std::fs::copy(&source, &dest)
                .map_err(|e| AppError::other(format!("copy {rel}: {e}")))?;
        }
        Ok(())
    })
    .await
    .map_err(|e| AppError::other(format!("import model task join: {e}")))??;
    promote_staging().await
}

/// Resolve a required file inside an import folder: prefer the repo-relative
/// path (`onnx/model.onnx`), fall back to the basename at the folder root
/// (`model.onnx`).
fn find_source(src: &Path, rel: &str) -> Option<PathBuf> {
    let direct = src.join(rel);
    if direct.is_file() {
        return Some(direct);
    }
    let base = Path::new(rel).file_name()?;
    let flat = src.join(base);
    flat.is_file().then_some(flat)
}
