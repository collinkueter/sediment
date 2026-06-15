//! Memory store commands. After M7 only one command remains JS-callable —
//! `index_formation`, the formation-wide re-index. The graph and search APIs
//! are reached by the conversational agent through the in-app MCP server
//! (`core/formation_mcp.rs`), so there is no need to expose them to JS.

use crate::commands::formation::APP_DIR;
use crate::core::formation_state::FormationState;
use crate::core::indexer::{apply_task_completions, index_note_path};
use crate::core::memory::MemoryHandle;
use crate::core::ollama_sidecar::OllamaSidecar;
use crate::error::AppResult;
use serde::Serialize;
use tauri::State;

#[derive(Debug, Clone, Serialize)]
pub struct IndexProgress {
    pub done: usize,
    pub total: usize,
    pub current_path: String,
}

#[derive(Debug, Serialize)]
pub struct IndexFormationResult {
    pub total: usize,
    pub indexed: usize,
    pub skipped: usize,
    pub failed: usize,
}

/// Walk the open formation and index every `.md` file. Files whose mtime has
/// not advanced past their recorded index-state are skipped unless `force`.
/// Emits `index-progress` events so the UI can show a bar.
#[tauri::command]
pub async fn index_formation(
    force: bool,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
    sidecar: State<'_, OllamaSidecar>,
    app: tauri::AppHandle,
) -> AppResult<IndexFormationResult> {
    use tauri::Emitter;

    let formation_root = formation.require()?;
    let memory_dir = formation_root.join(APP_DIR).join("memory");
    let store = memory.get_or_init(&memory_dir).await?;

    let notes = crate::commands::formation::walk_notes(&formation_root)?;
    let total = notes.len();
    let mut indexed = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;

    for (i, note) in notes.iter().enumerate() {
        let needs_index = force
            || match store.indexed_mtime(&note.relative_path).await? {
                Some(recorded) => note.modified_secs > recorded,
                None => true,
            };
        if !needs_index {
            skipped += 1;
            continue;
        }
        let _ = app.emit(
            "index-progress",
            IndexProgress {
                done: i,
                total,
                current_path: note.relative_path.clone(),
            },
        );
        match index_note_path(&formation_root, store, &sidecar, &note.relative_path).await {
            Ok(out) => {
                indexed += 1;
                // A formation-wide re-index may discover `Tasks.md` open→done
                // transitions the indexer never saw (e.g. boxes checked in
                // Obsidian while Sediment was closed). Route them through the
                // same daily-note append path the watcher-driven flow uses.
                if !out.task_completions.is_empty() {
                    apply_task_completions(&app, &formation_root, &out.task_completions);
                }
            }
            Err(e) => {
                tracing::warn!("index_formation: {} failed: {e}", note.relative_path);
                failed += 1;
            }
        }
    }

    // Final 100% tick so the UI can clear its progress affordance.
    let _ = app.emit(
        "index-progress",
        IndexProgress {
            done: total,
            total,
            current_path: String::new(),
        },
    );

    Ok(IndexFormationResult {
        total,
        indexed,
        skipped,
        failed,
    })
}
