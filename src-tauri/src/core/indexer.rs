//! Background note indexer.
//!
//! Chunks markdown, embeds each chunk via Ollama, and replaces the note's
//! `note_chunk` rows in SurrealDB. Auto-triggered after in-app saves and
//! external edits (via the file watcher), with a per-path debounce so a
//! flurry of Cmd+S presses coalesces into a single re-embed pass.
//!
//! `index_note_path` is the shared core; both the `index_note` Tauri command
//! and this background task call it.

use crate::core::formation_state::FormationState;
use crate::core::memory::{MemoryHandle, MemoryStore, NoteChunkInput};
use crate::core::ollama_sidecar::{OllamaSidecar, DEFAULT_EMBED_MODEL};
use crate::error::{AppError, AppResult};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc;
use tokio::time::Instant;

/// How long a path must be quiet before we (re-)index it.
const DEBOUNCE: Duration = Duration::from_millis(1500);
/// How often the debounce loop checks for ready paths.
const TICK: Duration = Duration::from_millis(500);
/// Max chars per chunk before splitting. nomic-embed-text tolerates ~8K but
/// recall is empirically better on smaller windows.
const CHUNK_MAX_CHARS: usize = 1500;

/// Handle to the background indexer. Cheap to clone; lives in Tauri state.
pub struct Indexer {
    tx: mpsc::UnboundedSender<String>,
}

impl Indexer {
    /// Spawn the background debounce loop. Call once in `setup()`.
    pub fn start(app: AppHandle) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(run(rx, app));
        Self { tx }
    }

    /// Queue a formation-relative `.md` path for (re-)indexing. Coalesced with
    /// any other request for the same path inside the debounce window.
    pub fn request(&self, relative_path: String) {
        // Send failure only happens if the loop died — log and move on.
        if self.tx.send(relative_path).is_err() {
            tracing::warn!("indexer channel closed; index request dropped");
        }
    }
}

async fn run(mut rx: mpsc::UnboundedReceiver<String>, app: AppHandle) {
    let mut pending: HashMap<String, Instant> = HashMap::new();
    let mut tick = tokio::time::interval(TICK);
    loop {
        tokio::select! {
            maybe = rx.recv() => {
                match maybe {
                    Some(path) => {
                        // Re-arm the debounce timer for this path.
                        pending.insert(path, Instant::now());
                    }
                    None => break, // all senders dropped — app shutting down
                }
            }
            _ = tick.tick() => {
                let now = Instant::now();
                let ready: Vec<String> = pending
                    .iter()
                    .filter(|(_, t)| now.duration_since(**t) >= DEBOUNCE)
                    .map(|(p, _)| p.clone())
                    .collect();
                for path in ready {
                    pending.remove(&path);
                    match index_in_app(&app, &path).await {
                        Ok(n) => tracing::info!("auto-indexed {path}: {n} chunks"),
                        Err(e) => tracing::warn!("auto-index {path} failed: {e}"),
                    }
                }
            }
        }
    }
}

/// Resolve managed state from the app handle and index one note.
async fn index_in_app(app: &AppHandle, relative_path: &str) -> AppResult<usize> {
    let formation = app.state::<FormationState>();
    let memory = app.state::<MemoryHandle>();
    let sidecar = app.state::<OllamaSidecar>();

    let formation_root = formation.require()?;
    let memory_dir = formation_root.join(".chat-notes").join("memory");
    let store = memory.get_or_init(&memory_dir).await?;
    index_note_path(&formation_root, store, &sidecar, relative_path).await
}

/// Shared indexing core. Reads `relative_path` from `formation_root`, chunks
/// it, embeds each chunk, and replaces the note's rows in SurrealDB.
/// Idempotent — calling twice produces the same stored state.
pub async fn index_note_path(
    formation_root: &Path,
    store: &MemoryStore,
    sidecar: &OllamaSidecar,
    relative_path: &str,
) -> AppResult<usize> {
    let abs = formation_root.join(relative_path);
    let content = std::fs::read_to_string(&abs)
        .map_err(|e| AppError::other(format!("read {}: {e}", abs.display())))?;

    let chunks = chunk_markdown(&content);
    let mut inputs = Vec::with_capacity(chunks.len());
    for (idx, text) in chunks.iter().enumerate() {
        let embedding = sidecar.embed(DEFAULT_EMBED_MODEL, text).await?;
        inputs.push(NoteChunkInput {
            note_path: relative_path.to_string(),
            chunk_idx: idx as i64,
            text: text.clone(),
            embedding,
        });
    }
    let count = inputs.len();
    store.replace_note_chunks(relative_path, inputs).await?;
    Ok(count)
}

/// Split markdown into chunks of `CHUNK_MAX_CHARS` or less, preferring
/// paragraph breaks. Conservative for Phase 1/2 — a markdown-aware splitter
/// that respects headings and code fences is a later refinement.
pub fn chunk_markdown(content: &str) -> Vec<String> {
    let paragraphs: Vec<&str> = content
        .split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for para in paragraphs {
        if !current.is_empty() && current.len() + para.len() + 2 > CHUNK_MAX_CHARS {
            out.push(std::mem::take(&mut current));
        }
        if para.len() > CHUNK_MAX_CHARS {
            // Hard-split very long paragraphs at char boundaries.
            for slice in para.as_bytes().chunks(CHUNK_MAX_CHARS) {
                let s = std::str::from_utf8(slice).unwrap_or("");
                if !s.is_empty() {
                    out.push(s.to_string());
                }
            }
        } else {
            if !current.is_empty() {
                current.push_str("\n\n");
            }
            current.push_str(para);
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_markdown_splits_on_paragraphs() {
        let md = "First para.\n\nSecond para.\n\nThird para.";
        let chunks = chunk_markdown(md);
        // All three fit comfortably under the ceiling — coalesced into one chunk.
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].contains("First para."));
        assert!(chunks[0].contains("Third para."));
    }

    #[test]
    fn chunk_markdown_respects_ceiling() {
        // Two paragraphs each ~1000 chars: together they exceed 1500, so split.
        let big = "x".repeat(1000);
        let md = format!("{big}\n\n{big}");
        let chunks = chunk_markdown(&md);
        assert_eq!(
            chunks.len(),
            2,
            "expected a split when combined size exceeds ceiling"
        );
    }

    #[test]
    fn chunk_markdown_hard_splits_huge_paragraph() {
        let huge = "y".repeat(4000);
        let chunks = chunk_markdown(&huge);
        assert!(
            chunks.len() >= 3,
            "4000-char paragraph should hard-split into 3+ chunks"
        );
        assert!(chunks.iter().all(|c| c.len() <= CHUNK_MAX_CHARS));
    }

    #[test]
    fn chunk_markdown_empty_input_yields_nothing() {
        assert!(chunk_markdown("").is_empty());
        assert!(chunk_markdown("\n\n   \n\n").is_empty());
    }
}
