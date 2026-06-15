//! Debounced filesystem watcher for the active formation. Emits a
//! `formation-change` Tauri event when markdown files change, with a
//! configurable debounce window so a flurry of editor saves coalesces into
//! a single notification.

use crate::commands::formation::APP_DIR;
use crate::error::{AppError, AppResult};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use notify_debouncer_full::{
    new_debouncer, DebounceEventResult, DebouncedEvent, Debouncer, FileIdMap,
};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

const DEBOUNCE_MS: u64 = 500;
pub const EVENT_NAME: &str = "formation-change";
/// How long a self-write mark suppresses watcher events for a path. Long
/// enough to outlast the debounce window, short enough that a genuine edit
/// soon after a commit is not swallowed.
const SELF_WRITE_TTL: Duration = Duration::from_secs(5);

/// One coalesced change emitted to the React side.
#[derive(Debug, Clone, Serialize)]
pub struct FormationChange {
    /// "created" | "modified" | "removed" | "other"
    pub kind: String,
    /// Formation-relative POSIX paths (forward-slash) for the affected files.
    pub paths: Vec<String>,
}

/// Owns the debouncer behind a mutex so we can swap the watched root atomically.
#[derive(Default)]
pub struct FormationWatcher {
    inner: Mutex<Option<Debouncer<RecommendedWatcher, FileIdMap>>>,
    /// Paths the app itself just wrote, with the write time. Watcher events
    /// for these are dropped so an internal write does not trigger a redundant
    /// re-index — only external edits (Obsidian, cloud-sync, …) need to flow
    /// through the watcher.
    self_writes: Arc<Mutex<HashMap<String, Instant>>>,
}

impl FormationWatcher {
    /// Watch `formation_root` recursively. Any prior watcher is dropped.
    pub fn start(&self, formation_root: PathBuf, app: AppHandle) -> AppResult<()> {
        let root_for_cb = formation_root.clone();
        let app_for_cb = app.clone();
        let self_writes = Arc::clone(&self.self_writes);
        let mut debouncer = new_debouncer(
            Duration::from_millis(DEBOUNCE_MS),
            None,
            move |result: DebounceEventResult| {
                emit_changes(result, &root_for_cb, &app_for_cb, &self_writes);
            },
        )
        .map_err(|e| AppError::other(format!("init debouncer: {e}")))?;

        debouncer
            .watcher()
            .watch(&formation_root, RecursiveMode::Recursive)
            .map_err(|e| AppError::other(format!("watch path: {e}")))?;
        debouncer
            .cache()
            .add_root(&formation_root, RecursiveMode::Recursive);

        // Replace the previous debouncer; dropping it stops the underlying watcher.
        *self
            .inner
            .lock()
            .map_err(|_| AppError::other("watcher mutex poisoned"))? = Some(debouncer);
        tracing::info!("watching formation: {}", formation_root.display());
        Ok(())
    }

    /// Stop watching. Safe to call when nothing is currently watched.
    #[allow(dead_code)]
    pub fn stop(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            *guard = None;
        }
    }

    /// Record that the app itself just wrote `rel_path`. The next watcher
    /// event for that path is suppressed — the writer (e.g. the agent's note
    /// edit) handles indexing directly; the watcher only services external
    /// edits.
    pub fn mark_self_write(&self, rel_path: &str) {
        if let Ok(mut marks) = self.self_writes.lock() {
            marks.insert(rel_path.to_string(), Instant::now());
        }
    }
}

fn emit_changes(
    result: DebounceEventResult,
    root: &Path,
    app: &AppHandle,
    self_writes: &Mutex<HashMap<String, Instant>>,
) {
    let events = match result {
        Ok(events) => events,
        Err(errors) => {
            for e in errors {
                tracing::warn!("watcher error: {e:?}");
            }
            return;
        }
    };
    for mut change in filter_events(events, root) {
        // Drop paths the app itself just wrote — the writer already indexed
        // them; re-emitting would double the work.
        change.paths = {
            let mut marks = self_writes.lock().expect("self-write set poisoned");
            filter_self_writes(&mut marks, change.paths, Instant::now())
        };
        if change.paths.is_empty() {
            continue;
        }
        // Queue created/modified notes for background re-indexing so external
        // edits stay searchable. Deletions are left for a later cleanup pass.
        if change.kind == "created" || change.kind == "modified" {
            if let Some(indexer) = app.try_state::<crate::core::indexer::Indexer>() {
                for path in &change.paths {
                    indexer.request(path.clone());
                }
            }
        }
        if let Err(e) = app.emit(EVENT_NAME, &change) {
            tracing::warn!("emit {EVENT_NAME} failed: {e}");
        }
    }
}

/// Remove app-originated paths from `paths` and prune stale marks. Returns the
/// paths whose changes came from outside the app — the ones worth reacting to.
fn filter_self_writes(
    marks: &mut HashMap<String, Instant>,
    paths: Vec<String>,
    now: Instant,
) -> Vec<String> {
    marks.retain(|_, t| now.duration_since(*t) < SELF_WRITE_TTL);
    paths
        .into_iter()
        .filter(|p| marks.remove(p).is_none())
        .collect()
}

/// Pure filter so we can unit-test the event-shaping logic without spawning a watcher.
fn filter_events(events: Vec<DebouncedEvent>, root: &Path) -> Vec<FormationChange> {
    let app_dir = root.join(APP_DIR);
    let mut out = Vec::new();
    for event in events {
        let kind_label = match event.event.kind {
            EventKind::Create(_) => "created",
            EventKind::Modify(_) => "modified",
            EventKind::Remove(_) => "removed",
            EventKind::Other => "other",
            _ => continue, // skip Access events and anything we don't care about
        };
        let mut rel_paths = Vec::new();
        for p in &event.event.paths {
            if p.starts_with(&app_dir) {
                continue;
            }
            let rel = match p.strip_prefix(root) {
                Ok(rel) => rel,
                Err(_) => continue,
            };
            // Markdown only — keeps .DS_Store, lock files, etc. out of the stream.
            if rel.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            rel_paths.push(rel.to_string_lossy().replace('\\', "/"));
        }
        if rel_paths.is_empty() {
            continue;
        }
        out.push(FormationChange {
            kind: kind_label.to_string(),
            paths: rel_paths,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, ModifyKind};
    use notify::Event;
    use std::time::Instant;

    fn debounced(kind: EventKind, paths: Vec<PathBuf>) -> DebouncedEvent {
        DebouncedEvent::new(
            Event {
                kind,
                paths,
                attrs: Default::default(),
            },
            Instant::now(),
        )
    }

    #[test]
    fn excludes_app_dir_and_non_markdown() {
        let root = PathBuf::from("/tmp/formation");
        let events = vec![
            // Should pass through.
            debounced(
                EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Content)),
                vec![root.join("People").join("John.md")],
            ),
            // Should be filtered (inside .chat-notes/).
            debounced(
                EventKind::Create(CreateKind::File),
                vec![root.join(APP_DIR).join("audit").join("foo.json")],
            ),
            // Should be filtered (non-markdown).
            debounced(
                EventKind::Create(CreateKind::File),
                vec![root.join(".DS_Store")],
            ),
        ];
        let out = filter_events(events, &root);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, "modified");
        assert_eq!(out[0].paths, vec!["People/John.md".to_string()]);
    }

    #[test]
    fn drops_events_with_no_remaining_paths() {
        let root = PathBuf::from("/tmp/formation");
        let events = vec![debounced(
            EventKind::Modify(ModifyKind::Any),
            vec![root.join(APP_DIR).join("memory").join("data")],
        )];
        assert!(filter_events(events, &root).is_empty());
    }

    #[test]
    fn self_writes_are_suppressed_then_consumed() {
        let mut marks = HashMap::new();
        let now = Instant::now();
        marks.insert("People/John.md".to_string(), now);

        // The marked path is dropped; an unmarked one passes through.
        let kept = filter_self_writes(
            &mut marks,
            vec!["People/John.md".into(), "People/Jane.md".into()],
            now,
        );
        assert_eq!(kept, vec!["People/Jane.md".to_string()]);

        // The mark is consumed — a genuine later edit is NOT suppressed.
        let kept_again = filter_self_writes(&mut marks, vec!["People/John.md".into()], now);
        assert_eq!(kept_again, vec!["People/John.md".to_string()]);
    }

    #[test]
    fn stale_self_write_marks_are_pruned() {
        let mut marks = HashMap::new();
        let written = Instant::now();
        marks.insert("People/John.md".to_string(), written);
        // Long after the TTL — the mark has expired, so the path passes through.
        let now = written + Duration::from_secs(10);
        let kept = filter_self_writes(&mut marks, vec!["People/John.md".into()], now);
        assert_eq!(kept, vec!["People/John.md".to_string()]);
        assert!(marks.is_empty(), "stale mark pruned");
    }
}
