//! In-process embedding model (ADR-0014) — local semantic search with **no
//! Ollama daemon**.
//!
//! Wraps fastembed's `nomic-embed-text-v1.5` (a 768-d ONNX model, matching the
//! existing HNSW index dimension in `core::memory`). The model is loaded lazily
//! once per process and held behind a `Mutex`; the ONNX weights are fetched to a
//! fixed, app-owned cache directory on first use (shared between the main
//! process and the `--mcp-stdio` subprocess so the download happens once). The
//! CPU-bound `embed` runs on a blocking thread so it never stalls the async
//! runtime.

use crate::error::{AppError, AppResult};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

static MODEL: OnceLock<Mutex<TextEmbedding>> = OnceLock::new();

/// Where fastembed caches the ONNX weights + tokenizer. Fixed and app-owned so
/// the main process and the MCP subprocess share one ~80 MB download.
fn cache_dir() -> PathBuf {
    // `HOME` on macOS/Linux; `USERPROFILE` on Windows (where `HOME` is usually
    // unset). The MCP subprocess and the main process resolve the same dir.
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".sediment").join("fastembed")
}

/// Get (or lazily build) the process-wide model. The first call downloads the
/// weights if they are not cached, then loads them — both potentially slow, so
/// callers invoke this from a blocking thread.
fn model() -> AppResult<&'static Mutex<TextEmbedding>> {
    if let Some(existing) = MODEL.get() {
        return Ok(existing);
    }
    let dir = cache_dir();
    std::fs::create_dir_all(&dir).ok();
    let embedder = TextEmbedding::try_new(
        InitOptions::new(EmbeddingModel::NomicEmbedTextV15)
            .with_cache_dir(dir)
            .with_show_download_progress(false),
    )
    .map_err(|e| {
        AppError::other(format!(
            "On-device model unavailable — the embedding model files couldn't be \
             loaded. They download once on first use, so check your network \
             connection and try again, or switch to keyword search in Settings. ({e})"
        ))
    })?;
    // A concurrent racer may have set it first — either way `get` succeeds.
    let _ = MODEL.set(Mutex::new(embedder));
    MODEL
        .get()
        .ok_or_else(|| AppError::other("bundled embedder set race"))
}

/// Embed `text` into a 768-d vector with the in-process model. The model loads
/// (and downloads weights on first use) on the first call; work runs on a
/// blocking thread.
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

/// Eagerly load (and, if needed, download) the model so the first real search
/// doesn't pay the cost. Called when the user opts into on-device search.
pub async fn warmup() -> AppResult<()> {
    embed("warmup").await.map(|_| ())
}
