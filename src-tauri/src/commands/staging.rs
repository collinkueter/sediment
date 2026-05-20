//! Tauri commands for the staging tray.
//!
//! P3-M1 scope: list / inspect / discard pre-commit entries. The commit and
//! undo handlers (`keep_staging`, `undo_commit`) land in P3-M6.

use crate::commands::formation::APP_DIR;
use crate::core::formation_state::FormationState;
use crate::core::staging::{self, StagingEntry};
use crate::error::AppResult;
use std::path::{Path, PathBuf};
use tauri::State;

/// `.chat-notes/staging/` inside the open formation.
pub(crate) fn staging_dir(formation_root: &Path) -> PathBuf {
    formation_root.join(APP_DIR).join("staging")
}

/// Every pending staging entry, oldest first.
#[tauri::command]
pub fn list_staging(formation: State<'_, FormationState>) -> AppResult<Vec<StagingEntry>> {
    let root = formation.require()?;
    staging::read_all(&staging_dir(&root))
}

/// One staging entry by id.
#[tauri::command]
pub fn get_staging(id: String, formation: State<'_, FormationState>) -> AppResult<StagingEntry> {
    let root = formation.require()?;
    staging::read_one(&staging_dir(&root), &id)
}

/// Discard a staging entry: deletes its JSON file. No notes and no graph edges
/// are touched — discarding pre-commit facts is a pure no-op everywhere else.
#[tauri::command]
pub fn discard_staging(id: String, formation: State<'_, FormationState>) -> AppResult<()> {
    let root = formation.require()?;
    staging::remove(&staging_dir(&root), &id)
}
