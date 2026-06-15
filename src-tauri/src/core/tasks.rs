//! Tasks & reminders — the model and its SurrealDB mirror (ADR-0007).
//!
//! A `Task` is a reminder: something the user wants to do and be alerted
//! about. It is deliberately distinct from the `task` *entity* type in the
//! knowledge graph — that stays for facts like "Josh owns the migration".
//!
//! Tasks live canonically as a `## Tasks` checklist in `Tasks.md` (see
//! `core::task_note`). This module owns the `Task` model and the `task` table
//! that mirrors the checklist so the scheduler can query due times. The
//! markdown owns `{title, status, due, id}`; the table adds the
//! scheduling-only fields (`remind_at`, `notified`, `source_chat_id`).

use crate::core::memory::{slugify, MemoryStore};
use crate::core::task_note::parse_tasks_section;
use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use surrealdb::types::{RecordId, RecordIdKey, SurrealValue};

/// Formation-relative path of the single managed task list.
pub const TASKS_NOTE_PATH: &str = "Tasks.md";

/// Whether a task is still outstanding or has been completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Open,
    Done,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Open => "open",
            TaskStatus::Done => "done",
        }
    }

    /// Parse a stored status string; anything unrecognised falls back to `Open`
    /// (the conservative choice — a reminder is never silently dropped).
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "done" => TaskStatus::Done,
            _ => TaskStatus::Open,
        }
    }
}

/// One reminder. `id` is `task:<slug>_<rand>` — stable across edits, and the
/// `🆔` token written into the checklist line so a line maps back to its row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub status: TaskStatus,
    /// When the task is due. Date-granularity in markdown, a datetime here.
    pub due: Option<chrono::DateTime<chrono::Utc>>,
    /// When to fire the reminder. Defaults to `due`; `snooze` overrides it.
    pub remind_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Set once the scheduler has fired this task's reminder.
    pub notified: bool,
    pub created: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    /// `chat_message:<...>` the task was extracted from, when applicable.
    pub source_chat_id: Option<String>,
}

impl Task {
    /// A fresh task id: a title slug (capped) plus a short random suffix, so
    /// two same-titled reminders never collide.
    pub fn new_id(title: &str) -> String {
        let slug = slugify(title);
        let base: String = if slug.is_empty() {
            "task".to_string()
        } else {
            slug.chars().take(40).collect()
        };
        format!(
            "task:{base}_{}",
            &uuid::Uuid::new_v4().simple().to_string()[..6]
        )
    }
}

/// Sanitised record-id key (the part after `task:`) — only `[a-z0-9_]`
/// survive, so it is always safe to splice into a DDL-position record id.
pub fn task_key(id: &str) -> String {
    let raw = id.strip_prefix("task:").unwrap_or(id);
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// The reminder instant for a date-granularity due date — 09:00 UTC of that
/// day. Markdown stores tasks date-only (`📅 YYYY-MM-DD`); this is the single
/// place a bare date is widened to the datetime the scheduler compares.
pub fn due_at(date: chrono::NaiveDate) -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    chrono::Utc.from_utc_datetime(
        &date
            .and_hms_opt(9, 0, 0)
            .expect("09:00:00 is always a valid time"),
    )
}

/// Raw `task` row shape for SurrealDB deserialisation.
#[derive(Debug, Clone, Deserialize, SurrealValue)]
struct TaskRow {
    id: RecordId,
    title: String,
    status: String,
    due: Option<chrono::DateTime<chrono::Utc>>,
    remind_at: Option<chrono::DateTime<chrono::Utc>>,
    notified: bool,
    created: chrono::DateTime<chrono::Utc>,
    completed_at: Option<chrono::DateTime<chrono::Utc>>,
    source_chat_id: Option<String>,
}

impl TaskRow {
    fn into_task(self) -> Task {
        Task {
            id: record_id_string(&self.id),
            title: self.title,
            status: TaskStatus::parse(&self.status),
            due: self.due,
            remind_at: self.remind_at,
            notified: self.notified,
            created: self.created,
            completed_at: self.completed_at,
            source_chat_id: self.source_chat_id,
        }
    }
}

fn record_id_string(rid: &RecordId) -> String {
    let key = match &rid.key {
        RecordIdKey::String(s) => s.clone(),
        RecordIdKey::Number(n) => n.to_string(),
        other => format!("{other:?}"),
    };
    format!("{}:{}", rid.table.as_str(), key)
}

/// Create or overwrite a task row. The caller owns the full `Task` state — on
/// an update it must carry forward `created` and any field it does not mean to
/// change (e.g. `put_task` after a `get_task` + mutate).
pub async fn put_task(store: &MemoryStore, task: &Task) -> AppResult<()> {
    let key = task_key(&task.id);
    if key.is_empty() {
        return Err(AppError::other(format!(
            "task id has no key: {:?}",
            task.id
        )));
    }
    let sql = format!(
        "UPSERT task:{key} SET \
         title = $title, status = $status, due = $due, remind_at = $remind_at, \
         notified = $notified, created = $created, completed_at = $completed_at, \
         source_chat_id = $source_chat_id;"
    );
    store
        .handle()
        .query(sql)
        .bind(("title", task.title.clone()))
        .bind(("status", task.status.as_str().to_string()))
        .bind(("due", task.due))
        .bind(("remind_at", task.remind_at))
        .bind(("notified", task.notified))
        .bind(("created", task.created))
        .bind(("completed_at", task.completed_at))
        .bind(("source_chat_id", task.source_chat_id.clone()))
        .await
        .map_err(|e| AppError::other(format!("put_task: {e}")))?
        .check()
        .map_err(|e| AppError::other(format!("put_task check: {e}")))?;
    Ok(())
}

/// Every task, oldest first. Drives the in-app reminders list.
pub async fn list_tasks(store: &MemoryStore) -> AppResult<Vec<Task>> {
    let mut res = store
        .handle()
        .query("SELECT * FROM task ORDER BY created;")
        .await
        .map_err(|e| AppError::other(format!("list_tasks: {e}")))?;
    let rows: Vec<TaskRow> = res
        .take(0)
        .map_err(|e| AppError::other(format!("list_tasks take: {e}")))?;
    Ok(rows.into_iter().map(TaskRow::into_task).collect())
}

/// One task by id, or `None` if it does not exist.
pub async fn get_task(store: &MemoryStore, id: &str) -> AppResult<Option<Task>> {
    let key = task_key(id);
    if key.is_empty() {
        return Ok(None);
    }
    let mut res = store
        .handle()
        .query(format!("SELECT * FROM task:{key};"))
        .await
        .map_err(|e| AppError::other(format!("get_task: {e}")))?;
    let rows: Vec<TaskRow> = res
        .take(0)
        .map_err(|e| AppError::other(format!("get_task take: {e}")))?;
    Ok(rows.into_iter().next().map(TaskRow::into_task))
}

/// Delete a task row by id. A missing row is not an error.
pub async fn delete_task(store: &MemoryStore, id: &str) -> AppResult<()> {
    let key = task_key(id);
    if key.is_empty() {
        return Ok(());
    }
    store
        .handle()
        .query(format!("DELETE task:{key};"))
        .await
        .map_err(|e| AppError::other(format!("delete_task: {e}")))?
        .check()
        .map_err(|e| AppError::other(format!("delete_task check: {e}")))?;
    Ok(())
}

/// Open, un-notified tasks whose `remind_at` has arrived — the rows the
/// scheduler fires. Includes reminders that came due while the app was closed.
pub async fn due_reminders(
    store: &MemoryStore,
    now: chrono::DateTime<chrono::Utc>,
) -> AppResult<Vec<Task>> {
    let mut res = store
        .handle()
        .query(
            "SELECT * FROM task \
             WHERE status = 'open' AND notified = false \
               AND remind_at IS NOT NONE AND remind_at <= $now \
             ORDER BY remind_at;",
        )
        .bind(("now", now))
        .await
        .map_err(|e| AppError::other(format!("due_reminders: {e}")))?;
    let rows: Vec<TaskRow> = res
        .take(0)
        .map_err(|e| AppError::other(format!("due_reminders take: {e}")))?;
    Ok(rows.into_iter().map(TaskRow::into_task).collect())
}

/// Mark a task's reminder as fired so the scheduler does not fire it again.
pub async fn mark_notified(store: &MemoryStore, id: &str) -> AppResult<()> {
    let key = task_key(id);
    if key.is_empty() {
        return Ok(());
    }
    store
        .handle()
        .query(format!("UPDATE task:{key} SET notified = true;"))
        .await
        .map_err(|e| AppError::other(format!("mark_notified: {e}")))?
        .check()
        .map_err(|e| AppError::other(format!("mark_notified check: {e}")))?;
    Ok(())
}

/// One task whose checklist line just flipped `open → done` in `Tasks.md`,
/// returned from [`reconcile_tasks_md`]. The indexer reacts to these by
/// appending `- <title>` to today's daily-note `## Did` section
/// (ADR-0010 §5) and recording a `task_completion` audit entry (ADR-0010 §8).
///
/// Idempotence is on the **transition** — the same `Tasks.md` content
/// re-reconciled (or the daily-note write itself coming back through the
/// watcher) yields an empty event list, so the side effect fires at most
/// once per check-off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCompletionEvent {
    /// `task:<key>` of the task whose box was just checked.
    pub task_id: String,
    /// The task's title at the moment of the transition — what the indexer
    /// puts in the appended bullet and stores in the audit-log header.
    pub title: String,
}

/// Reconcile the `task` table against the `## Tasks` section of `Tasks.md`
/// content — the markdown is canonical (ADR-0007). Each checklist line updates
/// its row (matched by `🆔`, or by title for an id-less hand-added line); a
/// row whose line has disappeared is deleted. Table-only fields — `remind_at`,
/// `notified`, `created`, `source_chat_id` — are carried forward across the
/// update so a checked box or an edited title does not lose the schedule.
///
/// Returns the list of `open → done` transitions observed in this pass — the
/// hook the indexer uses to drive ADR-0010 §5's daily-note auto-append. The
/// transition is detected against the **pre-reconcile** table snapshot, so
/// re-reconciling the same `Tasks.md` content yields an empty event list (a
/// second pass sees the task as already `Done`).
pub async fn reconcile_tasks_md(
    store: &MemoryStore,
    content: &str,
) -> AppResult<Vec<TaskCompletionEvent>> {
    let lines = parse_tasks_section(content);
    let by_key: HashMap<String, Task> = list_tasks(store)
        .await?
        .into_iter()
        .map(|t| (task_key(&t.id), t))
        .collect();

    // Each existing row is matched by at most one line.
    let mut claimed: HashSet<String> = HashSet::new();
    let mut transitions: Vec<TaskCompletionEvent> = Vec::new();
    let now = chrono::Utc::now();

    for line in &lines {
        // Resolve the row this line maps to: its `🆔`, else an unclaimed row
        // with the same title (a hand-added line), else a fresh id.
        let key = match &line.id {
            Some(id) => task_key(id),
            None => match_by_title(&by_key, &claimed, &line.title)
                .unwrap_or_else(|| task_key(&Task::new_id(&line.title))),
        };
        if key.is_empty() || !claimed.insert(key.clone()) {
            continue; // empty/duplicate key — skip rather than collide
        }

        let prior = by_key.get(&key);
        // open→done transition: the prior row existed and was Open, and the
        // new line is checked. A brand-new line that arrives already `[x]`
        // is NOT a transition (the user already had the bullet checked
        // when they pasted the line) — it would have no "Open" state to
        // transition from.
        if line.done && matches!(prior.map(|p| p.status), Some(TaskStatus::Open)) {
            transitions.push(TaskCompletionEvent {
                task_id: format!("task:{key}"),
                title: line.title.trim().to_string(),
            });
        }

        let due = line.due.map(due_at);
        let task = Task {
            id: format!("task:{key}"),
            title: line.title.trim().to_string(),
            status: if line.done {
                TaskStatus::Done
            } else {
                TaskStatus::Open
            },
            due,
            // remind_at is table-only — carry it forward, or seed it from due.
            remind_at: prior.and_then(|p| p.remind_at).or(due),
            notified: prior.is_some_and(|p| p.notified),
            created: prior.map_or(now, |p| p.created),
            completed_at: if line.done {
                line.completed
                    .map(due_at)
                    .or_else(|| prior.and_then(|p| p.completed_at))
                    .or(Some(now))
            } else {
                None
            },
            source_chat_id: prior.and_then(|p| p.source_chat_id.clone()),
        };
        put_task(store, &task).await?;
    }

    // A row with no matching line was removed from Tasks.md — delete it.
    let removed: Vec<String> = by_key
        .keys()
        .filter(|k| !claimed.contains(*k))
        .cloned()
        .collect();
    for key in removed {
        delete_task(store, &format!("task:{key}")).await?;
    }
    Ok(transitions)
}

/// Find an unclaimed existing row whose title matches `title` (case-insensitive)
/// — the fallback that links a hand-added, id-less checklist line to its row.
fn match_by_title(
    by_key: &HashMap<String, Task>,
    claimed: &HashSet<String>,
    title: &str,
) -> Option<String> {
    let needle = title.trim().to_lowercase();
    by_key
        .iter()
        .find(|(k, t)| !claimed.contains(k.as_str()) && t.title.trim().to_lowercase() == needle)
        .map(|(k, _)| k.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    fn tempdir() -> std::path::PathBuf {
        let p = std::env::temp_dir()
            .join("sediment-test-tasks")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&p).expect("tempdir");
        p
    }

    fn sample(title: &str) -> Task {
        let now = Utc::now();
        Task {
            id: Task::new_id(title),
            title: title.to_string(),
            status: TaskStatus::Open,
            due: Some(now + Duration::days(1)),
            remind_at: Some(now + Duration::days(1)),
            notified: false,
            created: now,
            completed_at: None,
            source_chat_id: Some("chat_message:abc".into()),
        }
    }

    #[test]
    fn new_id_is_slug_prefixed_and_unique() {
        let a = Task::new_id("Renew passport");
        let b = Task::new_id("Renew passport");
        assert!(a.starts_with("task:renew_passport_"));
        assert_ne!(a, b, "the random suffix keeps two same-title ids distinct");
        // An un-sluggable title still yields a usable key.
        assert!(Task::new_id("***").starts_with("task:task_"));
    }

    #[test]
    fn task_key_strips_prefix_and_sanitises() {
        assert_eq!(
            task_key("task:renew_passport_a1b2c3"),
            "renew_passport_a1b2c3"
        );
        assert_eq!(task_key("renew_passport_a1b2c3"), "renew_passport_a1b2c3");
        // A stray unsafe char is folded to `_`, never spliced raw.
        assert_eq!(task_key("task:bad;drop"), "bad_drop");
    }

    /// put → get → list round-trips a task unchanged, and a second put with a
    /// changed status is an update (no duplicate row).
    #[tokio::test]
    async fn put_get_list_round_trip_and_update() {
        let dir = tempdir();
        let store = MemoryStore::open(&dir).await.expect("open");

        let mut task = sample("Renew passport");
        put_task(&store, &task).await.expect("put");

        let got = get_task(&store, &task.id)
            .await
            .expect("get")
            .expect("found");
        assert_eq!(got.title, "Renew passport");
        assert_eq!(got.status, TaskStatus::Open);
        assert!(got.due.is_some());
        assert_eq!(got.source_chat_id.as_deref(), Some("chat_message:abc"));

        // A second put with the same id is an update, not a new row.
        task.status = TaskStatus::Done;
        task.completed_at = Some(Utc::now());
        put_task(&store, &task).await.expect("update");

        let all = list_tasks(&store).await.expect("list");
        assert_eq!(all.len(), 1, "update must not create a second row");
        assert_eq!(all[0].status, TaskStatus::Done);
        assert!(all[0].completed_at.is_some());

        delete_task(&store, &task.id).await.expect("delete");
        assert!(list_tasks(&store).await.expect("list").is_empty());
        // Deleting an already-gone task is a no-op.
        delete_task(&store, &task.id)
            .await
            .expect("idempotent delete");

        std::fs::remove_dir_all(dir).ok();
    }

    /// reconcile_tasks_md treats the markdown as canonical: a checked line
    /// marks its row done, a removed line deletes its row, a new line creates
    /// one, and the table-only `remind_at` survives the round trip.
    #[tokio::test]
    async fn reconcile_mirrors_tasks_md_into_the_table() {
        let dir = tempdir();
        let store = MemoryStore::open(&dir).await.expect("open");
        let now = Utc::now();

        // Two tasks already in the table.
        let mut keep = sample("Renew passport");
        keep.id = "task:renew_passport_keep01".into();
        keep.remind_at = Some(now + Duration::days(3));
        let mut gone = sample("Old errand");
        gone.id = "task:old_errand_gone01".into();
        put_task(&store, &keep).await.expect("put keep");
        put_task(&store, &gone).await.expect("put gone");

        // Markdown: "Renew passport" is now checked done, "Old errand" is gone,
        // and a hand-added "Buy milk" line appears with its own id.
        let md = "## Tasks\n\n\
            - [x] Renew passport 📅 2026-06-01 ✅ 2026-05-25 🆔 renew_passport_keep01\n\
            - [ ] Buy milk 🆔 buy_milk_new001\n";
        let events = reconcile_tasks_md(&store, md).await.expect("reconcile");
        assert_eq!(
            events,
            vec![TaskCompletionEvent {
                task_id: "task:renew_passport_keep01".into(),
                title: "Renew passport".into(),
            }],
            "the open→done transition is reported exactly once"
        );

        let all = list_tasks(&store).await.expect("list");
        assert_eq!(all.len(), 2, "the removed line's row is deleted");

        let renew = all
            .iter()
            .find(|t| t.id == "task:renew_passport_keep01")
            .expect("the kept task survives");
        assert_eq!(renew.status, TaskStatus::Done, "the checked line is done");
        assert!(
            renew.completed_at.is_some(),
            "a done task has a completed_at"
        );
        assert_eq!(
            renew.remind_at, keep.remind_at,
            "the table-only remind_at is carried forward"
        );
        assert!(
            all.iter().any(|t| t.id == "task:buy_milk_new001"),
            "the hand-added line created a row"
        );
        assert!(
            !all.iter().any(|t| t.id == "task:old_errand_gone01"),
            "the removed line's row is gone"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    /// A second reconcile of the same `Tasks.md` content emits no
    /// transition events — the task is already `Done`, so there is no
    /// `open → done` edge to fire on. This is the idempotence guarantee
    /// the indexer relies on (ADR-0010 §5: idempotent on the transition,
    /// not on the file save).
    #[tokio::test]
    async fn reconcile_is_idempotent_on_transitions() {
        let dir = tempdir();
        let store = MemoryStore::open(&dir).await.expect("open");

        // Seed one Open task.
        let mut t = sample("Take vitamins");
        t.id = "task:take_vitamins_seed01".into();
        put_task(&store, &t).await.expect("put");

        let md = "## Tasks\n\n\
            - [x] Take vitamins 🆔 take_vitamins_seed01\n";
        let first = reconcile_tasks_md(&store, md).await.expect("first");
        assert_eq!(first.len(), 1, "the first pass observes the transition");
        assert_eq!(first[0].task_id, "task:take_vitamins_seed01");

        // A second pass with the same content — the task is already `Done`
        // in the table, so no transition is reported.
        let second = reconcile_tasks_md(&store, md).await.expect("second");
        assert!(
            second.is_empty(),
            "re-reconciling the same content does not re-fire"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    /// A line that arrives already `[x]` for a task that did not exist in
    /// the table is **not** a transition — no Open→Done edge to detect.
    /// (The user pasted in an already-checked bullet; no daily-note append.)
    #[tokio::test]
    async fn reconcile_does_not_treat_new_checked_lines_as_transitions() {
        let dir = tempdir();
        let store = MemoryStore::open(&dir).await.expect("open");
        let md = "## Tasks\n\n\
            - [x] Already done 🆔 already_done_new001\n";
        let events = reconcile_tasks_md(&store, md).await.expect("reconcile");
        assert!(
            events.is_empty(),
            "a new line that arrives done is not a transition"
        );
        // The row is still upserted as Done — only the event channel skips it.
        let row = get_task(&store, "task:already_done_new001")
            .await
            .expect("get")
            .expect("found");
        assert_eq!(row.status, TaskStatus::Done);

        std::fs::remove_dir_all(dir).ok();
    }

    /// Integration: a fresh `[x]` transition observed by `reconcile_tasks_md`,
    /// piped through `daily_note::append_did_bullet`, lands a bullet under
    /// `## Did` in today's daily note. This is the side-effect the indexer
    /// performs in production (`core/indexer.rs`); here we exercise it
    /// directly so the unit suite has end-to-end coverage without the watcher.
    #[tokio::test]
    async fn transition_event_drives_daily_note_append() {
        use crate::core::daily_note;

        let dir = tempdir();
        let store = MemoryStore::open(&dir).await.expect("open");

        // Seed one Open task with a stable id, so reconcile_tasks_md can
        // match the checklist line by `🆔` and detect the transition.
        let mut t = sample("Call the dentist");
        t.id = "task:call_dentist_seed02".into();
        put_task(&store, &t).await.expect("put");

        let md = "## Tasks\n\n\
            - [x] Call the dentist 🆔 call_dentist_seed02\n";
        let events = reconcile_tasks_md(&store, md).await.expect("reconcile");
        assert_eq!(events.len(), 1, "one transition observed");

        // Materialise today's daily note and append the bullet — the two
        // operations the indexer performs after reconcile fires its events.
        let today = daily_note::today_local();
        let abs = daily_note::ensure_daily_note(&dir, today).expect("ensure");
        let bullet = format!("- {}", events[0].title);
        daily_note::append_did_bullet(&abs, &bullet).expect("append");

        let body = std::fs::read_to_string(&abs).expect("read");
        assert!(body.contains("## Did"), "## Did exists");
        assert!(
            body.contains("- Call the dentist"),
            "bullet landed in the daily note"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    /// Regression: if the table row is already `Done` when `reconcile_tasks_md`
    /// runs, no transition fires — even if the markdown line is `[x]`. This is
    /// the trap the in-app `complete_task` flow used to fall into: it
    /// pre-updated the table before writing `Tasks.md`, so the reconcile saw
    /// `Done → Done` and skipped the daily-note append. The fix is to let
    /// reconcile own the table update, never pre-update from a command handler.
    #[tokio::test]
    async fn reconcile_does_not_fire_when_table_already_done() {
        let dir = tempdir();
        let store = MemoryStore::open(&dir).await.expect("open");

        // The pitfall: a Done row already in the table.
        let mut t = sample("Call the dentist");
        t.id = "task:call_dentist_pitfl".into();
        t.status = TaskStatus::Done;
        t.completed_at = Some(chrono::Utc::now());
        put_task(&store, &t).await.expect("put");

        let md = "## Tasks\n\n\
            - [x] Call the dentist 🆔 call_dentist_pitfl\n";
        let events = reconcile_tasks_md(&store, md).await.expect("reconcile");
        assert!(
            events.is_empty(),
            "no transition fires because the row was already Done — \
             commands::complete_task must NOT pre-update the table"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    /// due_reminders returns only open, un-notified tasks whose remind_at has
    /// passed — a future one, a notified one, and a done one are all excluded.
    #[tokio::test]
    async fn due_reminders_filters_by_time_status_and_notified() {
        let dir = tempdir();
        let store = MemoryStore::open(&dir).await.expect("open");
        let now = Utc::now();

        let mut overdue = sample("Call the dentist");
        overdue.remind_at = Some(now - Duration::hours(2));

        let mut future = sample("Book flights");
        future.remind_at = Some(now + Duration::hours(2));

        let mut already = sample("Water plants");
        already.remind_at = Some(now - Duration::hours(2));
        already.notified = true;

        let mut finished = sample("Submit report");
        finished.remind_at = Some(now - Duration::hours(2));
        finished.status = TaskStatus::Done;

        for t in [&overdue, &future, &already, &finished] {
            put_task(&store, t).await.expect("put");
        }

        let due = due_reminders(&store, now).await.expect("due");
        assert_eq!(
            due.len(),
            1,
            "only the overdue, open, un-notified task fires"
        );
        assert_eq!(due[0].title, "Call the dentist");

        // After firing, mark_notified takes it out of the due set.
        mark_notified(&store, &overdue.id).await.expect("notified");
        assert!(
            due_reminders(&store, now)
                .await
                .expect("due again")
                .is_empty(),
            "a notified task is not fired twice"
        );

        std::fs::remove_dir_all(dir).ok();
    }
}
