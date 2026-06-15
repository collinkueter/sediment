//! Audit-log commands — the browsable, revertable backstop for `chat_turn`
//! (ADR-0009 §6, plan M4).
//!
//! `chat_turn` writes one [`crate::core::audit::AuditEntry`] per turn. These
//! commands let the front-end audit-log panel list those entries and revert a
//! whole turn or a single recorded Fact. The mechanics live in
//! [`crate::core::audit`]; this module is the thin Tauri-state edge.

use crate::commands::formation::APP_DIR;
use crate::core::audit::{self, AuditEntry, UndoTaskCompletionResult};
use crate::core::formation_state::FormationState;
use crate::core::memory::MemoryHandle;
use crate::error::AppResult;
use tauri::State;

/// Every turn's audit entry, newest-first, for the audit-log panel.
#[tauri::command]
pub async fn list_audit(formation: State<'_, FormationState>) -> AppResult<Vec<AuditEntry>> {
    let formation_root = formation.require()?;
    audit::read_all_audit(&formation_root)
}

/// Revert a whole turn: restore every changed note from the pre-turn snapshot
/// and delete every Fact the turn recorded (ADR-0009 §6 — the quiet undo).
#[tauri::command]
pub async fn undo_turn(
    turn_id: String,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
) -> AppResult<()> {
    let formation_root = formation.require()?;
    let store = memory
        .get_or_init(&formation_root.join(APP_DIR).join("memory"))
        .await?;
    audit::undo_turn(&formation_root, store, &turn_id).await
}

/// Revert one Fact from a turn — per-Fact granularity (ADR-0009 §6: a turn
/// that recorded eight Facts can have one reverted without losing the rest).
/// The turn's notes and other Facts are untouched.
#[tauri::command]
pub async fn undo_fact(
    turn_id: String,
    fact_id: String,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
) -> AppResult<()> {
    let formation_root = formation.require()?;
    let store = memory
        .get_or_init(&formation_root.join(APP_DIR).join("memory"))
        .await?;
    audit::undo_fact(&formation_root, store, &turn_id, &fact_id).await
}

/// Revert one indexer-driven `task_completion` append — ADR-0010 §8. Removes
/// the exact appended bullet from today's daily note; refuses (returns
/// `EditedSinceAppended`) if the user has edited the line since logging,
/// preserving their edit.
#[tauri::command]
pub async fn undo_task_completion(
    entry_id: String,
    formation: State<'_, FormationState>,
) -> AppResult<UndoTaskCompletionResult> {
    let formation_root = formation.require()?;
    audit::undo_task_completion(&formation_root, &entry_id)
}
