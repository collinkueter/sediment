//! Tauri commands for the staging tray: list / inspect / discard pre-commit
//! entries, and commit (`keep_staging`) or revert (`undo_commit`) them.
//!
//! A Keep is the only path from staged facts to the formation: it snapshots
//! the affected notes, writes the new markdown, upserts entities, writes the
//! bi-temporal fact edges, and re-indexes. An undo record captures everything
//! needed to reverse it within the UI's 10-second window.

use crate::commands::formation::APP_DIR;
use crate::core::diff_gen::apply_facts_to_note;
use crate::core::formation_state::{atomic_write, FormationState};
use crate::core::indexer::index_note_path;
use crate::core::memory::{FactWriteInput, MemoryHandle, MemoryStore};
use crate::core::ollama_sidecar::OllamaSidecar;
use crate::core::staging::{
    self, Conflict, NoteChange, StagedFact, StagingEntry, UndoNote, UndoRecord,
};
use crate::core::watcher::FormationWatcher;
use crate::error::{AppError, AppResult};
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tauri::{Emitter, State};

/// `.chat-notes/staging/` inside the open formation.
pub(crate) fn staging_dir(formation_root: &Path) -> PathBuf {
    formation_root.join(APP_DIR).join("staging")
}

/// `.chat-notes/snapshots/<commit_id>/` — the per-commit undo tree.
fn snapshot_dir(formation_root: &Path, commit_id: &str) -> PathBuf {
    formation_root
        .join(APP_DIR)
        .join("snapshots")
        .join(commit_id)
}

/// Build the `NoteChange` for `facts` targeting `note_path`: render the diff
/// against the on-disk note, then flag every current fact a new fact would
/// contradict (Phase 3 decision, P3-M7). Returns `None` when idempotence
/// filtering leaves no fact to stage. Shared by `chat_write` and the
/// conflict-resolution re-render so both produce identical changes.
pub(crate) async fn assemble_note_change(
    store: &MemoryStore,
    formation_root: &Path,
    note_path: &str,
    facts: &[StagedFact],
    source_chat_id: &str,
) -> AppResult<Option<NoteChange>> {
    let existing = std::fs::read_to_string(formation_root.join(note_path)).ok();
    let mut change = apply_facts_to_note(note_path, existing.as_deref(), facts, source_chat_id);
    if change.facts.is_empty() {
        return Ok(None);
    }
    for idx in 0..change.facts.len() {
        let fact = &change.facts[idx];
        // A fact the user already resolved as "Keep both" needs no banner.
        if fact.explicit_coexist {
            continue;
        }
        for c in store
            .find_conflicts(&fact.subject_id, &fact.predicate, &fact.object_id)
            .await?
        {
            change.conflicts.push(Conflict {
                staged_fact_index: idx,
                predicate: c.predicate,
                existing_object_id: c.object_id,
                existing_object_name: c.object_name,
                existing_valid_from: c.valid_from,
                existing_source_chat_id: c.source_chat_id,
            });
        }
    }
    Ok(Some(change))
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

/// Overwrite a staging entry's JSON with `entry`. The tray uses this to drop an
/// individual note change, persist a reviewer's diff edits, or record a
/// conflict resolution. Still pre-commit — nothing touches the graph or notes.
#[tauri::command]
pub fn update_staging(entry: StagingEntry, formation: State<'_, FormationState>) -> AppResult<()> {
    let root = formation.require()?;
    staging::write(&staging_dir(&root), &entry)
}

/// Resolve a conflict on a staged fact (P3-M7). `resolution` is one of:
///   - `"update"`  — keep the new fact; the commit supersedes the old one (the
///                   default behaviour), so this just clears the banner.
///   - `"coexist"` — set `explicit_coexist` so the commit keeps both facts
///                   (the consultant / concurrent-employment case).
///   - `"discard"` — drop the new fact entirely; the note diff is re-rendered
///                   without it.
/// Still pre-commit — the graph is untouched.
#[tauri::command]
pub async fn resolve_conflict(
    staging_id: String,
    note_path: String,
    staged_fact_index: usize,
    resolution: String,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
) -> AppResult<()> {
    let root = formation.require()?;
    let sdir = staging_dir(&root);
    let mut entry = staging::read_one(&sdir, &staging_id)?;
    let store = memory
        .get_or_init(&root.join(APP_DIR).join("memory"))
        .await?;

    let Some(pos) = entry.changes.iter().position(|c| c.note_path == note_path) else {
        return Err(AppError::other(format!("no staged change for {note_path}")));
    };

    match resolution.as_str() {
        "update" => {
            entry.changes[pos]
                .conflicts
                .retain(|c| c.staged_fact_index != staged_fact_index);
        }
        "coexist" => {
            let change = &mut entry.changes[pos];
            if let Some(fact) = change.facts.get_mut(staged_fact_index) {
                fact.explicit_coexist = true;
            }
            change
                .conflicts
                .retain(|c| c.staged_fact_index != staged_fact_index);
        }
        "discard" => {
            let change = &entry.changes[pos];
            if staged_fact_index >= change.facts.len() {
                return Err(AppError::other("staged_fact_index out of range"));
            }
            let mut remaining = change.facts.clone();
            remaining.remove(staged_fact_index);
            match assemble_note_change(store, &root, &note_path, &remaining, &entry.chat_message_id)
                .await?
            {
                Some(rebuilt) => entry.changes[pos] = rebuilt,
                None => {
                    entry.changes.remove(pos);
                }
            }
        }
        other => return Err(AppError::other(format!("unknown resolution: {other}"))),
    }

    if entry.changes.is_empty() {
        staging::remove(&sdir, &staging_id)?;
    } else {
        staging::write(&sdir, &entry)?;
    }
    Ok(())
}

/// Outcome of a Keep — enough for the UI to show an undo toast and refresh.
#[derive(Debug, Serialize)]
pub struct CommitResult {
    /// Id of this commit; pass it to `undo_commit` to revert.
    pub commit_id: String,
    pub staging_id: String,
    /// Formation-relative paths of the notes written to disk.
    pub committed_notes: Vec<String>,
    /// Record ids of the facts written — `undo_commit` deletes exactly these.
    pub new_fact_ids: Vec<String>,
    /// The still-staged entry when only some notes were kept, else `None`.
    pub remaining: Option<StagingEntry>,
}

/// Commit a staging entry. With `note_paths` set, only those note changes are
/// committed and the rest stay staged (individual Keep). Thin wrapper over
/// `commit_changes` that resolves Tauri state and emits the result.
#[tauri::command]
pub async fn keep_staging(
    id: String,
    note_paths: Option<Vec<String>>,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
    sidecar: State<'_, OllamaSidecar>,
    watcher: State<'_, FormationWatcher>,
    app: tauri::AppHandle,
) -> AppResult<CommitResult> {
    let root = formation.require()?;
    let entry = staging::read_one(&staging_dir(&root), &id)?;
    let store = memory
        .get_or_init(&root.join(APP_DIR).join("memory"))
        .await?;
    let result = commit_changes(&root, store, &sidecar, &watcher, entry, note_paths).await?;
    if let Err(e) = app.emit("staging-committed", &result) {
        tracing::warn!("emit staging-committed failed: {e}");
    }
    Ok(result)
}

/// Core of a Keep, free of Tauri state so it is directly testable. Each commit:
///   1. snapshots the affected notes for undo,
///   2. writes the new markdown content,
///   3. upserts entities and writes the bi-temporal fact edges,
///   4. re-indexes the changed notes,
///   5. removes (or trims) the staging entry, recording an undo record.
pub(crate) async fn commit_changes(
    root: &Path,
    store: &MemoryStore,
    sidecar: &OllamaSidecar,
    watcher: &FormationWatcher,
    entry: StagingEntry,
    note_paths: Option<Vec<String>>,
) -> AppResult<CommitResult> {
    let sdir = staging_dir(root);
    let id = entry.id.clone();

    // Split the entry's changes into the ones to commit now and the rest.
    let select: Option<HashSet<String>> = note_paths.map(|v| v.into_iter().collect());
    let (to_commit, to_keep): (Vec<NoteChange>, Vec<NoteChange>) =
        entry.changes.iter().cloned().partition(|c| match &select {
            Some(set) => set.contains(&c.note_path),
            None => true,
        });
    if to_commit.is_empty() {
        return Err(AppError::other("no matching note changes to keep"));
    }

    let commit_id = format!(
        "commit_{}_{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%3f"),
        &uuid::Uuid::new_v4().simple().to_string()[..6]
    );
    let snap_dir = snapshot_dir(root, &commit_id);

    let mut committed_notes = Vec::new();
    let mut undo_notes = Vec::new();
    let mut new_fact_ids = Vec::new();

    for change in &to_commit {
        // 1. Snapshot the pre-commit note (a "create" has no prior content).
        let was_create = !staging::snapshot_note(&snap_dir, root, &change.note_path)?;

        // 2. Write the new content. Mark the path first so the watcher's
        //    debounced event is dropped — step 4 re-indexes synchronously.
        watcher.mark_self_write(&change.note_path);
        atomic_write(&root.join(&change.note_path), change.new_content.as_bytes())?;

        // 3. Commit each fact: upsert both entities, link the subject's note,
        //    write the bi-temporal edge with the chat message as provenance.
        for fact in &change.facts {
            let subject = store
                .upsert_entity(&fact.subject_name, &fact.subject_type, vec![])
                .await?;
            let object = store
                .upsert_entity(&fact.object_name, &fact.object_type, vec![])
                .await?;
            store
                .set_entity_note_path(&subject.id, &change.note_path)
                .await?;
            // "Keep both" (explicit_coexist) skips supersession so a
            // contradicting current fact survives — see ADR-0004 / R3.
            let fact_id = store
                .relate_fact_with(
                    FactWriteInput {
                        subject_id: subject.id,
                        predicate: fact.predicate.clone(),
                        object_id: object.id,
                        valid_from: fact.valid_from,
                        source_chat_id: entry.chat_message_id.clone(),
                        confidence: fact.confidence,
                    },
                    !fact.explicit_coexist,
                )
                .await?;
            new_fact_ids.push(fact_id);
        }

        // 4. Re-index the changed note. Best-effort — a down Ollama must not
        //    fail the commit; the note + facts are already persisted.
        if let Err(e) = index_note_path(root, store, sidecar, &change.note_path).await {
            tracing::warn!("re-index {} after commit failed: {e}", change.note_path);
        }

        committed_notes.push(change.note_path.clone());
        undo_notes.push(UndoNote {
            note_path: change.note_path.clone(),
            was_create,
        });
    }

    // 5. Persist the undo record before mutating the staging entry.
    staging::write_undo(
        &snap_dir,
        &UndoRecord {
            commit_id: commit_id.clone(),
            staging_id: id.clone(),
            notes: undo_notes,
            new_fact_ids: new_fact_ids.clone(),
            entry_snapshot: entry.clone(),
        },
    )?;

    // 6. Drop the staging entry, or trim it to the changes left for review.
    let remaining = if to_keep.is_empty() {
        staging::remove(&sdir, &id)?;
        None
    } else {
        let trimmed = StagingEntry {
            changes: to_keep,
            ..entry
        };
        staging::write(&sdir, &trimmed)?;
        Some(trimmed)
    };

    Ok(CommitResult {
        commit_id,
        staging_id: id,
        committed_notes,
        new_fact_ids,
        remaining,
    })
}

/// Revert a commit: restore the snapshotted notes, delete exactly the facts the
/// commit wrote, re-index, and put the staging entry back for re-review. The
/// 10-second window is enforced by the UI; the snapshot tree is what makes the
/// reversal possible.
#[tauri::command]
pub async fn undo_commit(
    commit_id: String,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
    sidecar: State<'_, OllamaSidecar>,
    watcher: State<'_, FormationWatcher>,
    app: tauri::AppHandle,
) -> AppResult<()> {
    let root = formation.require()?;
    let snap_dir = snapshot_dir(&root, &commit_id);
    let undo = staging::read_undo(&snap_dir)?;

    let store = memory
        .get_or_init(&root.join(APP_DIR).join("memory"))
        .await?;

    // 1. Revert the note files — a "create" is deleted, an update is restored.
    for note in &undo.notes {
        watcher.mark_self_write(&note.note_path);
        if note.was_create {
            std::fs::remove_file(root.join(&note.note_path)).ok();
        } else {
            staging::restore_note(&snap_dir, &root, &note.note_path)?;
        }
    }

    // 2. Delete exactly the facts the commit wrote.
    for fact_id in &undo.new_fact_ids {
        if let Err(e) = store.delete_fact(fact_id).await {
            tracing::warn!("undo: delete fact {fact_id} failed: {e}");
        }
    }

    // 3. Re-index: a deleted note loses its chunks, a restored note is re-embedded.
    for note in &undo.notes {
        if note.was_create {
            store
                .replace_note_chunks(&note.note_path, vec![])
                .await
                .ok();
        } else if let Err(e) = index_note_path(&root, store, &sidecar, &note.note_path).await {
            tracing::warn!("undo: re-index {} failed: {e}", note.note_path);
        }
    }

    // 4. Put the staging entry back so the user can re-review the proposal.
    staging::write(&staging_dir(&root), &undo.entry_snapshot)?;

    // 5. The commit is fully reverted — drop its snapshot tree.
    staging::remove_snapshot(&snap_dir);

    if let Err(e) = app.emit("staging-undone", &commit_id) {
        tracing::warn!("emit staging-undone failed: {e}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::memory::MemoryStore;

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir()
            .join("sediment-test-commit")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&p).expect("tempdir");
        p
    }

    fn sample_entry() -> StagingEntry {
        StagingEntry {
            id: StagingEntry::new_id(),
            created: chrono::Utc::now(),
            chat_message_id: "chat_message:test".into(),
            chat_excerpt: "Alice founded Acme.".into(),
            status: "pending".into(),
            changes: vec![NoteChange {
                kind: staging::ChangeKind::Create,
                note_path: "People/Alice.md".into(),
                diff: "+- Founded Acme".into(),
                new_content: "---\nchat-notes:\n  facts:\n    \"founded:acme\": \"chat_message:test\"\n---\n\n## Facts\n\n- Founded Acme\n".into(),
                confidence: 0.95,
                conflicts: vec![],
                facts: vec![StagedFact {
                    subject_id: "entity:alice".into(),
                    subject_name: "Alice".into(),
                    subject_type: "person".into(),
                    predicate: "founded".into(),
                    object_id: "entity:acme".into(),
                    object_name: "Acme".into(),
                    object_type: "organization".into(),
                    valid_from: chrono::Utc::now(),
                    valid_from_explicit: false,
                    confidence: 0.95,
                    explicit_coexist: false,
                }],
            }],
        }
    }

    /// stage → keep: the note lands on disk, the fact lands in the graph, the
    /// staging entry is consumed, and an undo record is left behind. Needs no
    /// extraction model — the StagingEntry is hand-built.
    #[tokio::test]
    async fn stage_then_keep_writes_note_and_graph() {
        let root = tempdir();
        let sdir = staging_dir(&root);
        let entry = sample_entry();
        staging::write(&sdir, &entry).expect("write staging");

        let store = MemoryStore::open(&root.join(APP_DIR).join("memory"))
            .await
            .expect("open store");
        let sidecar = OllamaSidecar::default();
        let watcher = FormationWatcher::default();

        let result = commit_changes(&root, &store, &sidecar, &watcher, entry, None)
            .await
            .expect("commit");

        // The note file exists on disk with the fact bullet.
        let note = std::fs::read_to_string(root.join("People/Alice.md")).expect("note written");
        assert!(note.contains("- Founded Acme"), "note has the fact bullet");

        // The fact is a current edge about Alice in the graph.
        let facts = store.current_facts("entity:alice").await.expect("facts");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].predicate, "founded");

        // The staging entry is consumed; an undo record was recorded.
        assert!(staging::read_all(&sdir).expect("read_all").is_empty());
        assert_eq!(result.committed_notes, vec!["People/Alice.md".to_string()]);
        assert_eq!(result.new_fact_ids.len(), 1);
        let snap = root.join(APP_DIR).join("snapshots").join(&result.commit_id);
        assert!(staging::read_undo(&snap).is_ok(), "undo record persisted");

        std::fs::remove_dir_all(root).ok();
    }

    /// stage → discard: removing the entry touches nothing else — no note file
    /// is written, the graph is never opened.
    #[tokio::test]
    async fn stage_then_discard_is_a_noop() {
        let root = tempdir();
        let sdir = staging_dir(&root);
        let entry = sample_entry();
        staging::write(&sdir, &entry).expect("write staging");
        assert_eq!(staging::read_all(&sdir).expect("read_all").len(), 1);

        staging::remove(&sdir, &entry.id).expect("discard");

        assert!(staging::read_all(&sdir).expect("read_all after").is_empty());
        assert!(
            !root.join("People/Alice.md").exists(),
            "discard must not write the note"
        );

        std::fs::remove_dir_all(root).ok();
    }
}
