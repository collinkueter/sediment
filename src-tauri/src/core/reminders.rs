//! Background reminder scheduler (ADR-0007).
//!
//! A tokio task spawned at startup, like the indexer. Every `TICK` it queries
//! the `task` table for reminders that have come due, raises an OS
//! notification and a `reminder-due` event for each, and marks them notified
//! so they never fire twice. Reminders that came due while the app was closed
//! fire on the first tick after launch — the V1 stand-in for true background
//! alarms (see ADR-0007, "app-closed alarms are out of scope").

use crate::commands::formation::APP_DIR;
use crate::core::formation_state::FormationState;
use crate::core::memory::{MemoryHandle, MemoryStore};
use crate::core::tasks::{self, Task};
use crate::error::AppResult;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

/// How often the scheduler checks for due reminders. Reminders are
/// day-granular, so sub-minute precision buys nothing.
const TICK: Duration = Duration::from_secs(30);

/// Event emitted to the front end when a reminder fires.
pub const EVENT_NAME: &str = "reminder-due";

/// Spawn the scheduler loop on the Tauri async runtime. Call once in `setup()`.
pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(run(app));
}

async fn run(app: AppHandle) {
    let mut tick = tokio::time::interval(TICK);
    loop {
        tick.tick().await;
        fire_due(&app).await;
    }
}

/// Resolve the open formation's store, collect the reminders that have come
/// due, and fire each. A no-op when no formation is open yet (e.g. the first
/// tick after launch, before `restore_last_formation` has run).
async fn fire_due(app: &AppHandle) {
    let Some(root) = app.state::<FormationState>().get() else {
        return;
    };
    let memory = app.state::<MemoryHandle>();
    let store = match memory.get_or_init(&root.join(APP_DIR).join("memory")).await {
        Ok(store) => store,
        Err(e) => {
            tracing::warn!("reminder scheduler: open store failed: {e}");
            return;
        }
    };
    let due = match collect_due(store, chrono::Utc::now()).await {
        Ok(due) => due,
        Err(e) => {
            tracing::warn!("reminder scheduler: due query failed: {e}");
            return;
        }
    };
    for task in due {
        notify(app, &task);
        if let Err(e) = app.emit(EVENT_NAME, &task) {
            tracing::warn!("emit {EVENT_NAME} failed: {e}");
        }
    }
}

/// Query the reminders that have come due as of `now` and atomically arm them
/// — mark each notified so it is never fired again. Returns the tasks to alert
/// the user about. The testable core of `fire_due`, free of `AppHandle` side
/// effects.
pub async fn collect_due(
    store: &MemoryStore,
    now: chrono::DateTime<chrono::Utc>,
) -> AppResult<Vec<Task>> {
    let due = tasks::due_reminders(store, now).await?;
    for task in &due {
        tasks::mark_notified(store, &task.id).await?;
    }
    Ok(due)
}

/// Raise a native OS notification for a due reminder. Best-effort — a denied
/// notification permission is logged, not fatal; the in-app surface still
/// shows the reminder.
fn notify(app: &AppHandle, task: &Task) {
    let body = match task.due {
        Some(due) => format!("{} · due {}", task.title, due.format("%b %-d")),
        None => task.title.clone(),
    };
    if let Err(e) = app
        .notification()
        .builder()
        .title("Reminder")
        .body(body)
        .show()
    {
        tracing::warn!("os notification failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tasks::{put_task, Task, TaskStatus};
    use chrono::{Duration as ChronoDuration, Utc};

    /// A reminder that has come due is fired exactly once: the first sweep
    /// returns it, and a second sweep returns nothing because it was armed.
    #[tokio::test]
    async fn collect_due_fires_each_reminder_exactly_once() {
        let dir = std::env::temp_dir()
            .join("sediment-test-reminders")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&dir).expect("tempdir");
        let store = MemoryStore::open(&dir).await.expect("open");
        let now = Utc::now();

        let overdue = Task {
            id: Task::new_id("call the dentist"),
            title: "Call the dentist".into(),
            status: TaskStatus::Open,
            due: Some(now - ChronoDuration::hours(1)),
            remind_at: Some(now - ChronoDuration::hours(1)),
            notified: false,
            created: now,
            completed_at: None,
            source_chat_id: None,
        };
        put_task(&store, &overdue).await.expect("put");

        let fired = collect_due(&store, now).await.expect("collect");
        assert_eq!(fired.len(), 1, "the overdue reminder fires");
        assert_eq!(fired[0].title, "Call the dentist");

        assert!(
            collect_due(&store, now)
                .await
                .expect("collect again")
                .is_empty(),
            "an armed reminder is never fired twice"
        );

        std::fs::remove_dir_all(dir).ok();
    }
}
