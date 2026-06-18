//! Background note indexer.
//!
//! Chunks markdown, embeds each chunk via the active embedding provider, and replaces the note's
//! `note_chunk` rows in SurrealDB. Auto-triggered after in-app saves and
//! external edits (via the file watcher), with a per-path debounce so a
//! flurry of Cmd+S presses coalesces into a single re-embed pass.
//!
//! `index_note_path` is the shared core; both the `index_note` Tauri command
//! and this background task call it.

use crate::core::audit;
use crate::core::daily_note;
use crate::core::embedding::{embed_query, EmbeddingProvider};
use crate::core::formation_state::{AppConfig, FormationState};
use crate::core::memory::{MemoryHandle, MemoryStore, NoteChunkInput};
use crate::core::ollama_sidecar::OllamaSidecar;
use crate::core::tasks::TaskCompletionEvent;
use crate::core::watcher::FormationWatcher;
use crate::error::{AppError, AppResult};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc;
use tokio::time::Instant;

/// The Tauri event emitted after the indexer appends a bullet to today's
/// daily note in response to a `Tasks.md` open→done transition (ADR-0010 §8).
/// The UI listens for this to fire a 10-second quiet-undo toast.
pub const DAILY_NOTE_APPENDED_EVENT: &str = "daily-note-appended";

/// Payload of the `daily-note-appended` event. Carries enough state for the
/// toast to call `undo_task_completion(entry_id)` and to render a one-line
/// confirmation ("Logged 'Call the dentist' to today").
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyNoteAppendedPayload {
    /// `task_completion` audit entry id — the handle the toast uses to undo.
    pub entry_id: String,
    /// The completed task's id, for the toast's debug context.
    pub task_id: String,
    /// Formation-relative POSIX path of the daily note that was appended.
    pub daily_note_path: String,
    /// The verbatim bullet line that was added.
    pub bullet_text: String,
}

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
        tauri::async_runtime::spawn(run(rx, app));
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

/// Resolve managed state from the app handle and index one note. After the
/// shared core finishes, any `Tasks.md` open→done transitions it observed
/// are routed through [`apply_task_completions`] — the indexer is the only
/// place that owns ADR-0010 §5's daily-note auto-append.
async fn index_in_app(app: &AppHandle, relative_path: &str) -> AppResult<usize> {
    let formation = app.state::<FormationState>();
    let memory = app.state::<MemoryHandle>();
    let sidecar = app.state::<OllamaSidecar>();

    let formation_root = formation.require()?;
    let provider =
        EmbeddingProvider::from_config(AppConfig::load(app).embedding_provider.as_deref());
    let memory_dir = formation_root.join(".chat-notes").join("memory");
    let store = memory.get_or_init(&memory_dir).await?;
    let IndexOutcome {
        chunk_count,
        task_completions,
    } = index_note_path(&formation_root, store, &sidecar, provider, relative_path).await?;
    if !task_completions.is_empty() {
        apply_task_completions(app, &formation_root, &task_completions);
    }
    Ok(chunk_count)
}

/// Side-effect arm of the indexer for `Tasks.md` open→done transitions
/// (ADR-0010 §5, §8). For each transition we:
///
/// 1. Materialise today's daily note (creating from `Templates/Daily.md`,
///    seeding the template from the default if missing).
/// 2. Tell the watcher we are about to write the daily note, so its own
///    watcher event does not bounce back into the indexer.
/// 3. Append `- <title>` to the `## Did` section (idempotent on the bullet
///    text — the transition-detection upstream guarantees one event per
///    check-off, but this is the safety net).
/// 4. Record a `task_completion` audit entry — the handle the audit-log
///    panel and the toast undo use.
/// 5. Emit a `daily-note-appended` Tauri event so the UI can show the quiet
///    10-second undo toast.
///
/// Failures at any step are logged-and-skipped per-event — the indexer's
/// main chunk-and-embed pass already succeeded for the `Tasks.md` write, so
/// we never tear that down because a daily-note side effect glitched.
///
/// Public so command handlers that drive their own `index_note_path` call
/// (e.g. `complete_task` and `index_formation`) can route the returned
/// transitions through here too — without this they would observe but drop
/// the events, and the in-app Complete button path would silently skip the
/// daily-note append.
pub fn apply_task_completions(
    app: &AppHandle,
    formation_root: &Path,
    events: &[TaskCompletionEvent],
) {
    let watcher = app.try_state::<FormationWatcher>();
    let today = daily_note::today_local();
    let daily_rel = daily_note::daily_note_relative_path(today);

    let daily_abs = match daily_note::ensure_daily_note(formation_root, today) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("daily-note ensure failed: {e}");
            return;
        }
    };

    for ev in events {
        let bullet = format!("- {}", ev.title);
        // Suppress the daily note's own watcher event — the indexer write is
        // not an external edit, and `Tasks.md` reconciliation must not bounce
        // off it on the next tick.
        if let Some(w) = watcher.as_ref() {
            w.mark_self_write(&daily_rel);
        }
        if let Err(e) = daily_note::append_did_bullet(&daily_abs, &bullet) {
            tracing::warn!("append daily-note bullet for {}: {e}", ev.task_id);
            continue;
        }
        let entry_id = match audit::write_task_completion(
            formation_root,
            &ev.task_id,
            &ev.title,
            &daily_rel,
            &bullet,
        ) {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!("write task-completion audit entry for {}: {e}", ev.task_id);
                continue;
            }
        };
        if let Err(e) = audit::prune_old(formation_root, audit::AUDIT_RETENTION) {
            tracing::warn!("prune old audit entries failed: {e}");
        }
        let payload = DailyNoteAppendedPayload {
            entry_id,
            task_id: ev.task_id.clone(),
            daily_note_path: daily_rel.clone(),
            bullet_text: bullet,
        };
        if let Err(e) = app.emit(DAILY_NOTE_APPENDED_EVENT, &payload) {
            tracing::warn!("emit {DAILY_NOTE_APPENDED_EVENT} failed: {e}");
        }
    }
}

/// What [`index_note_path`] produced. The `chunk_count` is the chunk-and-embed
/// pass result — what the public `index_note` Tauri command historically
/// returned. `task_completions` carries the open→done transitions observed
/// during `Tasks.md` reconciliation, which the in-app indexer routes into
/// `apply_task_completions` (ADR-0010 §5). For non-`Tasks.md` indexes the
/// vector is always empty.
#[derive(Debug, Default)]
pub struct IndexOutcome {
    pub chunk_count: usize,
    pub task_completions: Vec<TaskCompletionEvent>,
}

/// Shared indexing core. Reads `relative_path` from `formation_root`, chunks
/// it, embeds each chunk, replaces the note's rows in SurrealDB, and records
/// the file's mtime so a formation-wide re-index can skip it next time.
/// Idempotent — calling twice produces the same stored state.
///
/// For `Tasks.md` the function also reconciles the `task` table against the
/// markdown (ADR-0007). Any open→done transitions observed are returned in
/// `IndexOutcome::task_completions` — the caller (typically `index_in_app`)
/// drives ADR-0010 §5's daily-note auto-append from those events. Callers
/// outside the app loop (e.g. unit tests, the `index_note` Tauri command)
/// may simply ignore the events.
pub async fn index_note_path(
    formation_root: &Path,
    store: &MemoryStore,
    sidecar: &OllamaSidecar,
    provider: EmbeddingProvider,
    relative_path: &str,
) -> AppResult<IndexOutcome> {
    let abs = formation_root.join(relative_path);
    let content = std::fs::read_to_string(&abs)
        .map_err(|e| AppError::other(format!("read {}: {e}", abs.display())))?;
    let mtime = file_mtime_secs(&abs);

    // A Tasks.md change reconciles the `task` table first (ADR-0007), so an
    // external edit — a checked box, a removed line — is mirrored even when
    // the embedding model is down and the chunk pass below fails.
    let mut task_completions = Vec::new();
    if relative_path == crate::core::tasks::TASKS_NOTE_PATH {
        match crate::core::tasks::reconcile_tasks_md(store, &content).await {
            Ok(events) => task_completions = events,
            Err(e) => tracing::warn!("reconcile Tasks.md after edit failed: {e}"),
        }
    }

    let chunks = chunk_markdown(&content);
    let mut inputs = Vec::with_capacity(chunks.len());
    for (idx, text) in chunks.iter().enumerate() {
        // In keyword mode (no local model) `embed_query` returns `None` and we
        // store text-only chunks; the keyword search makes them findable.
        let embedding = embed_query(provider, sidecar, text).await?;
        inputs.push(NoteChunkInput {
            note_path: relative_path.to_string(),
            chunk_idx: idx as i64,
            text: text.clone(),
            embedding,
        });
    }
    let count = inputs.len();
    store.replace_note_chunks(relative_path, inputs).await?;
    store.record_index_state(relative_path, mtime).await?;
    Ok(IndexOutcome {
        chunk_count: count,
        task_completions,
    })
}

/// File mtime as unix epoch seconds, or 0 if unavailable.
pub fn file_mtime_secs(path: &Path) -> i64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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
