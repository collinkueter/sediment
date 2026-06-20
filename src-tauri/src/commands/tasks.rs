//! Tauri commands for the task list and reminders (ADR-0007).
//!
//! `list_tasks` feeds the in-app reminders surface. `complete_task` and
//! `snooze_task` are the two actions on a reminder. `Tasks.md` is canonical,
//! so `complete_task` flips the checklist line there and mirrors the change
//! into the `task` table; `snooze_task` only touches the table, since
//! `remind_at` is a scheduling field the markdown does not express.

use crate::commands::formation::APP_DIR;
use crate::core::embedding::EmbeddingProvider;
use crate::core::formation_state::{atomic_write, AppConfig, FormationState};
use crate::core::indexer::{apply_task_completions, index_note_path};
use crate::core::memory::MemoryHandle;
use crate::core::ollama_sidecar::OllamaSidecar;
use crate::core::task_note::{parse_tasks_section, render_tasks_note};
use crate::core::tasks::{self, task_key, Task, TASKS_NOTE_PATH};
use crate::core::watcher::FormationWatcher;
use crate::error::{AppError, AppResult};
use tauri::{AppHandle, State};

/// Every task in the open formation, for the in-app reminders list.
#[tauri::command]
pub async fn list_tasks(
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
) -> AppResult<Vec<Task>> {
    let root = formation.require()?;
    let store = memory
        .get_or_init(&root.join(APP_DIR).join("memory"))
        .await?;
    tasks::list_tasks(store).await
}

/// Mark a task complete: flip its `## Tasks` checklist line in `Tasks.md` (the
/// canonical store), then let `reconcile_tasks_md` mirror the change into the
/// `task` table and surface the open→done transition. The transition drives
/// ADR-0010 §5's daily-note `## Did` append.
///
/// Crucially: we do **not** pre-update the table here. If we did, the row
/// would already be `Done` by the time the reconcile runs, and the
/// "open→done" transition would not fire — silently skipping the daily-note
/// append on the in-app Complete-button path. Letting the reconcile own the
/// table write keeps the two paths (in-app + external Obsidian edit)
/// behaviourally identical.
#[tauri::command]
pub async fn complete_task(
    id: String,
    app: AppHandle,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
    sidecar: State<'_, OllamaSidecar>,
    watcher: State<'_, FormationWatcher>,
) -> AppResult<()> {
    let root = formation.require()?;
    let store = memory
        .get_or_init(&root.join(APP_DIR).join("memory"))
        .await?;

    // Verify the task exists; reconcile owns the actual table update.
    if tasks::get_task(store, &id).await?.is_none() {
        return Err(AppError::other(format!("no such task: {id}")));
    }

    // Tasks.md checklist line → [x], with today's completion date.
    let key = task_key(&id);
    let tasks_path = root.join(TASKS_NOTE_PATH);
    let Ok(content) = std::fs::read_to_string(&tasks_path) else {
        return Ok(()); // No Tasks.md on disk — nothing to flip.
    };

    let mut lines = parse_tasks_section(&content);
    let mut changed = false;
    for line in &mut lines {
        if line.id.as_deref() == Some(key.as_str()) && !line.done {
            line.done = true;
            line.completed = Some(chrono::Utc::now().date_naive());
            changed = true;
        }
    }
    if !changed {
        return Ok(()); // Already done in the markdown — nothing to do.
    }

    let new_content = render_tasks_note(Some(&content), &lines);
    watcher.mark_self_write(TASKS_NOTE_PATH);
    atomic_write(&tasks_path, new_content.as_bytes())?;

    // Re-index Tasks.md: the reconcile flips the row to Done and returns the
    // open→done transition; routing it through apply_task_completions lands
    // the daily-note bullet, the audit entry, and the toast event.
    let provider =
        EmbeddingProvider::from_config(AppConfig::load(&app).embedding_provider.as_deref());
    match index_note_path(&root, store, &sidecar, provider, TASKS_NOTE_PATH).await {
        Ok(out) if !out.task_completions.is_empty() => {
            apply_task_completions(&app, &root, &out.task_completions);
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("re-index Tasks.md after complete failed: {e}"),
    }
    Ok(())
}

/// Change a task's due date/time, or clear it. `due` is an RFC3339 timestamp
/// (e.g. `2026-06-25T14:30:00Z`), or `None` to remove the due date entirely.
///
/// Updates both stores so the in-app surface and an external Obsidian edit stay
/// consistent: the canonical `Tasks.md` checklist line carries the **date** in
/// its `📅` token (Obsidian-Tasks dates are date-granularity), and the `task`
/// table row carries the **full datetime** on `due`/`remind_at` so the scheduler
/// fires at the chosen time. `notified` resets so a re-armed reminder fires
/// again. The chosen time-of-day lives on `remind_at`, which a later
/// `reconcile_tasks_md` preserves even though the markdown only round-trips the
/// date.
#[tauri::command]
pub async fn reschedule_task(
    id: String,
    due: Option<String>,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
    watcher: State<'_, FormationWatcher>,
) -> AppResult<()> {
    let root = formation.require()?;
    let store = memory
        .get_or_init(&root.join(APP_DIR).join("memory"))
        .await?;

    // Parse the requested due; an absent/blank value clears the date.
    let new_due: Option<chrono::DateTime<chrono::Utc>> = match due {
        Some(s) if !s.trim().is_empty() => Some(
            chrono::DateTime::parse_from_rfc3339(s.trim())
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| AppError::other(format!("parse due time: {e}")))?,
        ),
        _ => None,
    };

    let Some(mut task) = tasks::get_task(store, &id).await? else {
        return Err(AppError::other(format!("no such task: {id}")));
    };

    // Canonical store: update the checklist line's `📅` date (date-granularity).
    let key = task_key(&id);
    let tasks_path = root.join(TASKS_NOTE_PATH);
    if let Ok(content) = std::fs::read_to_string(&tasks_path) {
        let new_date = new_due.map(|d| d.date_naive());
        let mut lines = parse_tasks_section(&content);
        let mut changed = false;
        for line in &mut lines {
            if line.id.as_deref() == Some(key.as_str()) && line.due != new_date {
                line.due = new_date;
                changed = true;
            }
        }
        if changed {
            let new_content = render_tasks_note(Some(&content), &lines);
            watcher.mark_self_write(TASKS_NOTE_PATH);
            atomic_write(&tasks_path, new_content.as_bytes())?;
        }
    }

    // Table mirror: due + remind_at carry the full datetime, and notified resets
    // so the scheduler re-fires at the new time.
    task.due = new_due;
    task.remind_at = new_due;
    task.notified = false;
    tasks::put_task(store, &task).await?;
    Ok(())
}

/// Snooze a task's reminder to `until` (an RFC3339 timestamp). Table-only —
/// `remind_at` is a scheduling field, not part of the markdown checklist —
/// and re-arms `notified` so the scheduler fires it again at the new time.
#[tauri::command]
pub async fn snooze_task(
    id: String,
    until: String,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
) -> AppResult<()> {
    let root = formation.require()?;
    let store = memory
        .get_or_init(&root.join(APP_DIR).join("memory"))
        .await?;
    let until = chrono::DateTime::parse_from_rfc3339(&until)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .map_err(|e| AppError::other(format!("parse snooze time: {e}")))?;

    let Some(mut task) = tasks::get_task(store, &id).await? else {
        return Err(AppError::other(format!("no such task: {id}")));
    };
    task.remind_at = Some(until);
    task.notified = false;
    tasks::put_task(store, &task).await?;
    Ok(())
}
