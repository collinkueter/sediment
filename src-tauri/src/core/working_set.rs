//! The Working Set — ADR-0011 §3.
//!
//! A deterministically *derived* view of what is currently "in play": the people
//! and things recently touched, the notes recently edited, and the open tasks.
//! Recomputed each turn from recency signals already in the store — never
//! authored by the agent, never stored. It is pushed into the prompt (so the
//! agent is never an amnesiac) and surfaced in the UI ("what's in play").
//!
//! Cheap *because* it is a view: a few ordered queries, recomputed each turn.
//! Best-effort — a failing signal degrades to less context, never a failed turn.

use crate::core::memory::{MemoryStore, OpenLoop};
use crate::core::tasks::{list_tasks, TaskStatus};
use std::collections::HashSet;

const ENTITIES_K: usize = 8;
const NOTES_K: usize = 8;
const TASKS_K: usize = 8;
const LOOPS_K: usize = 5;
/// Open Loops older than this (and never resolved) stop surfacing (ADR-0011 §5).
const LOOP_DECAY_DAYS: i64 = 14;

/// What is currently in play. Serialized to the UI as the "what's in play" panel
/// (ADR-0011 §3); also rendered to Markdown and pushed into the turn.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkingSet {
    pub active_entities: Vec<ActiveEntity>,
    pub recent_notes: Vec<String>,
    pub open_tasks: Vec<OpenTask>,
    pub open_loops: Vec<OpenLoop>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveEntity {
    pub name: String,
    pub entity_type: String,
    pub note_path: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenTask {
    pub title: String,
    /// Due date, `YYYY-MM-DD`, when set.
    pub due: Option<String>,
}

/// Derive the Working Set from current store state. Best-effort: each signal
/// swallows its own error (logged) and contributes nothing on failure.
pub async fn derive_working_set(store: &MemoryStore) -> WorkingSet {
    let active_entities: Vec<ActiveEntity> = match store.recent_entities(ENTITIES_K).await {
        Ok(es) => es
            .into_iter()
            .map(|e| ActiveEntity {
                name: e.canonical_name,
                entity_type: e.entity_type,
                note_path: e.note_path,
            })
            .collect(),
        Err(e) => {
            tracing::warn!("working_set: recent_entities failed: {e}");
            Vec::new()
        }
    };

    // Recently edited notes, minus those already shown as an entity's own note.
    let entity_notes: HashSet<&str> = active_entities
        .iter()
        .filter_map(|e| e.note_path.as_deref())
        .collect();
    let recent_notes: Vec<String> = match store.recent_notes(NOTES_K).await {
        Ok(ns) => ns
            .into_iter()
            .filter(|p| !entity_notes.contains(p.as_str()))
            .collect(),
        Err(e) => {
            tracing::warn!("working_set: recent_notes failed: {e}");
            Vec::new()
        }
    };

    let open_tasks: Vec<OpenTask> = match list_tasks(store).await {
        Ok(ts) => ts
            .into_iter()
            .filter(|t| t.status == TaskStatus::Open)
            .take(TASKS_K)
            .map(|t| OpenTask {
                title: t.title,
                due: t.due.map(|d| d.format("%Y-%m-%d").to_string()),
            })
            .collect(),
        Err(e) => {
            tracing::warn!("working_set: list_tasks failed: {e}");
            Vec::new()
        }
    };

    let open_loops = match store.list_active_open_loops(LOOPS_K, LOOP_DECAY_DAYS).await {
        Ok(ls) => ls,
        Err(e) => {
            tracing::warn!("working_set: list_active_open_loops failed: {e}");
            Vec::new()
        }
    };

    WorkingSet {
        active_entities,
        recent_notes,
        open_tasks,
        open_loops,
    }
}

impl WorkingSet {
    pub fn is_empty(&self) -> bool {
        self.active_entities.is_empty()
            && self.recent_notes.is_empty()
            && self.open_tasks.is_empty()
            && self.open_loops.is_empty()
    }

    /// Render to the Markdown block pushed into the prompt under
    /// `# What you already know`, or `None` when nothing is in play.
    pub fn render_markdown(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut s = String::new();
        s.push_str("## Currently in play\n");
        s.push_str("Recent activity in this formation — background for the conversation.\n");
        if !self.active_entities.is_empty() {
            s.push_str("\n### Active people & things\n");
            for e in &self.active_entities {
                match &e.note_path {
                    Some(p) => s.push_str(&format!("- {} ({}) — {}\n", e.name, e.entity_type, p)),
                    None => s.push_str(&format!("- {} ({})\n", e.name, e.entity_type)),
                }
            }
        }
        if !self.recent_notes.is_empty() {
            s.push_str("\n### Recently edited notes\n");
            for p in &self.recent_notes {
                s.push_str(&format!("- {p}\n"));
            }
        }
        if !self.open_tasks.is_empty() {
            s.push_str("\n### Open tasks\n");
            for t in &self.open_tasks {
                match &t.due {
                    Some(d) => s.push_str(&format!("- {} (due {})\n", t.title, d)),
                    None => s.push_str(&format!("- {}\n", t.title)),
                }
            }
        }
        if !self.open_loops.is_empty() {
            s.push_str("\n### Open loops (unresolved — you may surface one)\n");
            for l in &self.open_loops {
                s.push_str(&format!("- {} [loop {}]\n", l.title, l.id));
            }
        }
        Some(s.trim_end().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::memory::MemoryStore;
    use crate::core::tasks::{put_task, Task, TaskStatus};
    use chrono::Utc;
    use std::path::PathBuf;

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir()
            .join("sediment-test-working-set")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&p).expect("tempdir");
        p
    }

    #[tokio::test]
    async fn derive_collects_entities_notes_and_open_tasks() {
        let root = tempdir();
        let store = MemoryStore::open(&root.join(".chat-notes").join("memory"))
            .await
            .expect("open store");

        store.upsert_entity("Josh", "person", vec![]).await.unwrap();
        store
            .upsert_entity("Q2 Planning", "project", vec![])
            .await
            .unwrap();
        store
            .record_index_state("Daily Notes/2026-06-15.md", 1)
            .await
            .unwrap();
        store
            .record_open_loop("Decide on vendor", None, "chat_message:x")
            .await
            .unwrap();
        let now = Utc::now();
        put_task(
            &store,
            &Task {
                id: "task:renew_passport_abc123".to_string(),
                title: "Renew passport".to_string(),
                status: TaskStatus::Open,
                due: None,
                remind_at: None,
                notified: false,
                created: now,
                completed_at: None,
                source_chat_id: None,
            },
        )
        .await
        .unwrap();

        let ws = derive_working_set(&store).await;

        assert!(ws.active_entities.iter().any(|e| e.name == "Josh"));
        assert!(ws
            .recent_notes
            .iter()
            .any(|p| p == "Daily Notes/2026-06-15.md"));
        assert!(ws.open_tasks.iter().any(|t| t.title == "Renew passport"));
        assert!(ws.open_loops.iter().any(|l| l.title == "Decide on vendor"));

        let md = ws.render_markdown().expect("non-empty working set renders");
        assert!(md.contains("## Currently in play"));
        assert!(md.contains("Josh"));
        assert!(md.contains("Renew passport"));
        assert!(md.contains("Decide on vendor"));
    }

    #[test]
    fn empty_working_set_renders_none() {
        assert!(WorkingSet::default().render_markdown().is_none());
    }
}
