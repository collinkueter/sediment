//! The audit log + undo — ADR-0009 §6 (extended by ADR-0010 §8), plan M4.
//!
//! ADR-0009 makes recording conversational: the agent edits notes and records
//! Facts in the same turn it learns them, with no blocking review queue. The
//! audit log is the **browsable backstop** — every applied formation
//! modification, reversible with a quiet undo.
//!
//! Two kinds of entry live side-by-side in the log (ADR-0010 §8 extends
//! ADR-0009 §6 from "per-chat-turn" to "per-formation-modification"):
//!
//! - [`ChatTurnEntry`] — one per `chat_turn` call. Snapshot-and-diff drives
//!   the changed-note list; graph writes are stamped with the turn's
//!   `source_chat_id` so the recorded Fact ids are discovered from
//!   `MemoryStore::facts_by_source`. Undo restores the changed notes from
//!   the snapshot and deletes the recorded Facts.
//! - [`TaskCompletionEntry`] — one per `Tasks.md` open→done transition
//!   indexed (ADR-0010 §5). The indexer appends `- <title>` to today's
//!   `Daily Notes/<today>.md` `## Did` section; this entry records the
//!   verbatim bullet text. Undo removes that exact bullet from the daily
//!   note, refusing if the user has edited it since (ADR-0010 §8).
//!
//! Because Claude Code edits note files directly with its own native file
//! tools (ADR-0009 §5, Option B), the app does **not** see note writes as
//! discrete tool calls — that's why `chat_turn` snapshots the whole formation
//! *before* each turn into `.chat-notes/snapshots/<turn>/`. TaskCompletion
//! entries store a **bullet-text record**, not a snapshot — a per-event full
//! snapshot would be wasteful for a single-line append, and the bullet text
//! is the inverse operation.
//!
//! Persistence is `.chat-notes/audit/<entry_id>.json` (atomic-write,
//! corrupt-file-tolerant on listing), exercised by temp-dir tests.

use crate::commands::formation::APP_DIR;
use crate::core::daily_note;
use crate::core::formation_state::atomic_write;
use crate::core::memory::MemoryStore;
use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// How many recent audit entries + snapshots are retained (ADR-0009: full
/// pre-turn snapshots kept for the last 20 entries; older entries are
/// corrected conversationally).
pub const AUDIT_RETENTION: usize = 20;

// ──────────────────────────────────────────────────────────────────────────
// Audit-entry types
// ──────────────────────────────────────────────────────────────────────────

/// One note a turn changed, as learned by diffing the pre-turn snapshot
/// against the formation after the turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedNote {
    /// Formation-relative POSIX path of the note.
    pub path: String,
    /// `true` when the note did not exist in the snapshot — the turn created
    /// it. Undo deletes a created note rather than restoring a (nonexistent)
    /// snapshot of it.
    pub was_create: bool,
}

/// One chat-turn's applied change — ADR-0009 §6.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTurnEntry {
    /// Stable id, also the JSON filename stem: `turn_<utc-compact>_<rand>`.
    pub turn_id: String,
    pub created: chrono::DateTime<chrono::Utc>,
    /// First chars of the user message, for the audit-log panel header.
    pub user_excerpt: String,
    /// First chars of the assistant reply, for the panel.
    pub reply_excerpt: String,
    /// Formation-relative POSIX path of the pre-turn snapshot directory
    /// (`.chat-notes/snapshots/<turn_id>`), so undo can find it.
    pub snapshot_dir: String,
    /// Notes the turn changed (diff of snapshot-vs-after).
    pub changed_notes: Vec<ChangedNote>,
    /// `fact` record ids the turn recorded through the MCP server. Per-Fact
    /// undo removes one of these; whole-turn undo removes them all.
    pub recorded_fact_ids: Vec<String>,
}

/// One `Tasks.md` open→done transition that the indexer mirrored into
/// today's daily-note `## Did` section (ADR-0010 §5).
///
/// The persisted shape is **bullet-text**, not a snapshot: revert reads the
/// daily note and removes the exact `appended_bullet_text` line — if the
/// user has edited it since, revert refuses rather than destroying the edit
/// (ADR-0010 §8).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskCompletionEntry {
    /// Stable id, also the JSON filename stem:
    /// `task_completion_<utc-compact>_<rand>`. Distinct id space from
    /// `turn_id` so the two never collide in the audit dir.
    pub entry_id: String,
    pub created: chrono::DateTime<chrono::Utc>,
    /// The `task` record id whose open→done transition triggered this
    /// append, e.g. `task:call_dentist_d4e5f6`.
    pub task_id: String,
    /// Task title at the moment the box was checked — for the audit-log
    /// panel header. The current `task` row may have a different title if
    /// the user later edited the line, so this is the snapshot value.
    pub task_title: String,
    /// Formation-relative POSIX path of the daily note that was appended.
    pub daily_note_path: String,
    /// The verbatim line that was added, e.g. `- Called the dentist`. Used
    /// as the inverse operation by undo (and as the refuse-on-edit guard).
    pub appended_bullet_text: String,
}

/// One audit-log entry — chat turn OR task completion. The variants serialise
/// with a `kind` discriminator (`"chatTurn"` / `"taskCompletion"`) and the
/// rest of the entry as flat camelCase fields, so the on-disk JSON is
/// uniform across kinds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AuditEntry {
    ChatTurn(ChatTurnEntry),
    TaskCompletion(TaskCompletionEntry),
}

impl AuditEntry {
    /// The entry's stable id — also its JSON filename stem.
    pub fn entry_id(&self) -> &str {
        match self {
            AuditEntry::ChatTurn(e) => &e.turn_id,
            AuditEntry::TaskCompletion(e) => &e.entry_id,
        }
    }

    /// The entry's creation time — used for newest-first sort and pruning.
    pub fn created(&self) -> chrono::DateTime<chrono::Utc> {
        match self {
            AuditEntry::ChatTurn(e) => e.created,
            AuditEntry::TaskCompletion(e) => e.created,
        }
    }
}

/// First ~140 characters of `text`, for an audit-log panel header.
pub fn excerpt(text: &str) -> String {
    const MAX: usize = 140;
    let trimmed = text.trim();
    if trimmed.chars().count() <= MAX {
        trimmed.to_string()
    } else {
        let head: String = trimmed.chars().take(MAX).collect();
        format!("{head}…")
    }
}

/// Build a fresh turn id from the current time. Millisecond precision + a
/// short random suffix keeps two same-second turns from colliding; the compact
/// form avoids `:` so it is a safe filename on every platform.
pub fn new_turn_id() -> String {
    format!(
        "turn_{}_{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%3f"),
        &uuid::Uuid::new_v4().simple().to_string()[..6]
    )
}

/// Build a fresh `task_completion` entry id — distinct id space from
/// `turn_id` so the audit dir never confuses one kind with the other.
pub fn new_task_completion_id() -> String {
    format!(
        "task_completion_{}_{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%3f"),
        &uuid::Uuid::new_v4().simple().to_string()[..6]
    )
}

// ──────────────────────────────────────────────────────────────────────────
// Path helpers
// ──────────────────────────────────────────────────────────────────────────

/// `.chat-notes/snapshots/<turn_id>` under a formation root.
fn snapshot_dir_for(formation_root: &Path, turn_id: &str) -> PathBuf {
    formation_root.join(APP_DIR).join("snapshots").join(turn_id)
}

/// `.chat-notes/audit` under a formation root.
fn audit_dir(formation_root: &Path) -> PathBuf {
    formation_root.join(APP_DIR).join("audit")
}

/// `.chat-notes/audit/<entry_id>.json` under a formation root.
fn audit_file(formation_root: &Path, entry_id: &str) -> PathBuf {
    audit_dir(formation_root).join(format!("{entry_id}.json"))
}

// ──────────────────────────────────────────────────────────────────────────
// Snapshot + diff
// ──────────────────────────────────────────────────────────────────────────

/// Recursively copy every file under `formation_root` — except the
/// `.chat-notes/` app directory — into `.chat-notes/snapshots/<turn_id>/`,
/// preserving relative paths. Returns the absolute snapshot directory.
///
/// ADR-0009 §6: this is the pre-turn whole-formation snapshot. It captures
/// note content so a later [`diff_formation`] can learn which notes the turn
/// changed, and so [`undo_turn`] can restore them.
pub fn snapshot_formation(formation_root: &Path, turn_id: &str) -> AppResult<PathBuf> {
    let snapshot_dir = snapshot_dir_for(formation_root, turn_id);
    std::fs::create_dir_all(&snapshot_dir)?;

    for rel in walk_formation_files(formation_root)? {
        let src = formation_root.join(&rel);
        let dst = snapshot_dir.join(&rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&src, &dst).map_err(|e| AppError::other(format!("snapshot {rel}: {e}")))?;
    }
    Ok(snapshot_dir)
}

/// Diff the formation against a pre-turn `snapshot_dir`: return the notes that
/// differ from their snapshot or are new (the `.chat-notes/` app directory is
/// ignored). `was_create` is `true` when the file is absent from the snapshot.
///
/// A note present in the snapshot but now missing is *not* reported — the
/// conversational agent does not delete note files, and an undo has nothing to
/// restore-to for a deletion. Equal files are skipped.
pub fn diff_formation(formation_root: &Path, snapshot_dir: &Path) -> AppResult<Vec<ChangedNote>> {
    let mut changed = Vec::new();
    for rel in walk_formation_files(formation_root)? {
        let current = formation_root.join(&rel);
        let snapshot = snapshot_dir.join(&rel);
        if !snapshot.exists() {
            changed.push(ChangedNote {
                path: rel,
                was_create: true,
            });
            continue;
        }
        // Both exist — compare bytes; only a real change is recorded.
        let cur_bytes =
            std::fs::read(&current).map_err(|e| AppError::other(format!("read {rel}: {e}")))?;
        let snap_bytes = std::fs::read(&snapshot)
            .map_err(|e| AppError::other(format!("read snapshot {rel}: {e}")))?;
        if cur_bytes != snap_bytes {
            changed.push(ChangedNote {
                path: rel,
                was_create: false,
            });
        }
    }
    changed.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(changed)
}

/// Directories the snapshot/diff walker must skip wholesale: the app's own
/// state, plus per-turn agent-config trees that engines drop into the
/// formation root (e.g. `.gemini/settings.json` for the Gemini CLI engine).
/// Without this they would land in the snapshot and reappear as a change on
/// the next turn's diff.
const SNAPSHOT_SKIP_DIRS: &[&str] = &[APP_DIR, ".gemini"];

/// Every regular file under `root`, as formation-relative POSIX paths, with
/// the `.chat-notes/` app directory and agent-config dirs skipped entirely.
/// Mirrors `commands::formation::walk_notes` but keeps non-`.md` files too —
/// a turn could touch an attachment.
fn walk_formation_files(root: &Path) -> AppResult<Vec<String>> {
    let mut out = Vec::new();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            !(e.file_type().is_dir() && SNAPSHOT_SKIP_DIRS.iter().any(|d| e.file_name() == *d))
        })
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(root)
            .map_err(|_| AppError::other("walked outside formation"))?
            .to_string_lossy()
            .replace('\\', "/");
        out.push(rel);
    }
    Ok(out)
}

// ──────────────────────────────────────────────────────────────────────────
// Audit-entry persistence
// ──────────────────────────────────────────────────────────────────────────

/// Persist `entry` to `.chat-notes/audit/<entry_id>.json` (atomic write).
pub fn write_audit(formation_root: &Path, entry: &AuditEntry) -> AppResult<()> {
    std::fs::create_dir_all(audit_dir(formation_root))?;
    let bytes = serde_json::to_vec_pretty(entry)?;
    atomic_write(&audit_file(formation_root, entry.entry_id()), &bytes)
}

/// Record one `task_completion` audit entry — the indexer's hook after it
/// appends a bullet to today's daily note (ADR-0010 §5, §8). Returns the
/// newly-allocated `entry_id` so the caller can carry it in the
/// `daily-note-appended` Tauri event.
pub fn write_task_completion(
    formation_root: &Path,
    task_id: &str,
    task_title: &str,
    daily_note_path: &str,
    appended_bullet_text: &str,
) -> AppResult<String> {
    let entry_id = new_task_completion_id();
    let entry = AuditEntry::TaskCompletion(TaskCompletionEntry {
        entry_id: entry_id.clone(),
        created: chrono::Utc::now(),
        task_id: task_id.to_string(),
        task_title: task_title.to_string(),
        daily_note_path: daily_note_path.to_string(),
        appended_bullet_text: appended_bullet_text.to_string(),
    });
    write_audit(formation_root, &entry)?;
    Ok(entry_id)
}

/// Read one audit entry by id. Errors if the file is missing or corrupt.
pub fn read_audit(formation_root: &Path, entry_id: &str) -> AppResult<AuditEntry> {
    let path = audit_file(formation_root, entry_id);
    let bytes =
        std::fs::read(&path).map_err(|e| AppError::other(format!("read audit {entry_id}: {e}")))?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Every audit entry, **newest-first** for the audit-log panel. A corrupt file
/// is skipped with a warning rather than failing the whole listing.
pub fn read_all_audit(formation_root: &Path) -> AppResult<Vec<AuditEntry>> {
    let mut out: Vec<AuditEntry> = Vec::new();
    let Ok(dir) = std::fs::read_dir(audit_dir(formation_root)) else {
        return Ok(out); // dir absent — simply no audit entries
    };
    for entry in dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue; // skip `.json.tmp` atomic-write leftovers, etc.
        }
        match std::fs::read(&path)
            .map_err(AppError::from)
            .and_then(|b| serde_json::from_slice::<AuditEntry>(&b).map_err(AppError::from))
        {
            Ok(e) => out.push(e),
            Err(e) => tracing::warn!("skipping corrupt audit file {}: {e}", path.display()),
        }
    }
    out.sort_by_key(|e| std::cmp::Reverse(e.created())); // newest first
    Ok(out)
}

/// Remove one audit entry's JSON file. A missing file is not an error.
fn remove_audit(formation_root: &Path, entry_id: &str) -> AppResult<()> {
    match std::fs::remove_file(audit_file(formation_root, entry_id)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(AppError::other(format!("remove audit {entry_id}: {e}"))),
    }
}

/// Keep only the most recent `keep` audit entries (and the snapshot dirs of
/// ChatTurn entries beyond the window); delete everything older.
///
/// ADR-0009: full pre-turn snapshots are retained only for the recent window
/// (`keep` = [`AUDIT_RETENTION`]); older turns are corrected conversationally,
/// so their byte-revertable snapshots are dropped. TaskCompletion entries
/// have no snapshot dir — just their JSON file is removed.
pub fn prune_old(formation_root: &Path, keep: usize) -> AppResult<()> {
    let all = read_all_audit(formation_root)?; // newest-first
    for stale in all.into_iter().skip(keep) {
        match &stale {
            AuditEntry::ChatTurn(e) => {
                // Drop the snapshot tree, then the audit entry.
                let snap = formation_root.join(&e.snapshot_dir);
                std::fs::remove_dir_all(&snap).ok();
            }
            AuditEntry::TaskCompletion(_) => {
                // No snapshot to clean — just the audit json below.
            }
        }
        remove_audit(formation_root, stale.entry_id())?;
    }
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────
// Undo
// ──────────────────────────────────────────────────────────────────────────

/// Undo a whole turn: restore every changed note from the pre-turn snapshot,
/// delete every Fact the turn recorded, then remove the audit entry and its
/// snapshot.
///
/// A `was_create` note is *deleted* (it did not exist before the turn); any
/// other changed note is overwritten with its snapshot bytes. Fact deletion is
/// best-effort per id — a Fact the user already corrected conversationally may
/// be gone, which is not an error.
///
/// Refuses if the entry is not a `ChatTurn` — `TaskCompletion` entries have
/// their own undo path.
pub async fn undo_turn(formation_root: &Path, store: &MemoryStore, turn_id: &str) -> AppResult<()> {
    let entry = read_audit(formation_root, turn_id)?;
    let AuditEntry::ChatTurn(entry) = entry else {
        return Err(AppError::other(format!(
            "audit entry {turn_id} is not a chat-turn entry"
        )));
    };
    let snapshot_dir = formation_root.join(&entry.snapshot_dir);

    // Roll back the turn's notes + Facts from its pre-turn snapshot.
    revert_to_snapshot(
        formation_root,
        &snapshot_dir,
        &entry.changed_notes,
        &entry.recorded_fact_ids,
        store,
    )
    .await?;

    // Drop the audit entry and its snapshot — the turn is gone.
    std::fs::remove_dir_all(&snapshot_dir).ok();
    remove_audit(formation_root, &entry.turn_id)?;
    Ok(())
}

/// Roll back a turn's side-effects from its pre-turn snapshot: restore each
/// changed note (or delete it if the turn created it), then delete every Fact the
/// turn recorded. Shared by [`undo_turn`] (which reads them from a written audit
/// entry, then also drops the entry + snapshot) and by the interrupt **Redirect**
/// path in `chat_turn` (which has no audit entry — it passes the live
/// `diff_formation` result + `facts_by_source`). One body means the two callers
/// can't drift on revert semantics.
///
/// `snapshot_dir` is the absolute snapshot directory. Deleting a Fact is
/// best-effort (logged, not fatal); a missing created-note is ignored.
pub async fn revert_to_snapshot(
    formation_root: &Path,
    snapshot_dir: &Path,
    changed_notes: &[ChangedNote],
    fact_ids: &[String],
    store: &MemoryStore,
) -> AppResult<()> {
    // 1. Notes — restore from snapshot, or delete if the turn created them.
    for note in changed_notes {
        let target = formation_root.join(&note.path);
        if note.was_create {
            match std::fs::remove_file(&target) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(AppError::other(format!(
                        "revert: remove created note {}: {e}",
                        note.path
                    )))
                }
            }
        } else {
            let bytes = std::fs::read(snapshot_dir.join(&note.path)).map_err(|e| {
                AppError::other(format!("revert: read snapshot {}: {e}", note.path))
            })?;
            atomic_write(&target, &bytes)?;
        }
    }

    // 2. Graph — delete every Fact the turn recorded.
    for fact_id in fact_ids {
        if let Err(e) = store.delete_fact(fact_id).await {
            tracing::warn!("revert: delete fact {fact_id} failed: {e}");
        }
    }
    Ok(())
}

/// Undo one Fact from a turn: delete the Fact edge and drop it from the audit
/// entry's `recorded_fact_ids`. The notes and the rest of the turn's Facts are
/// untouched (ADR-0009 §6: per-Fact revert granularity).
///
/// Removing the last Fact does **not** delete the audit entry — the turn may
/// still have changed notes worth keeping in the log.
///
/// Refuses if the entry is not a `ChatTurn`.
pub async fn undo_fact(
    formation_root: &Path,
    store: &MemoryStore,
    turn_id: &str,
    fact_id: &str,
) -> AppResult<()> {
    let entry = read_audit(formation_root, turn_id)?;
    let AuditEntry::ChatTurn(mut entry) = entry else {
        return Err(AppError::other(format!(
            "audit entry {turn_id} is not a chat-turn entry"
        )));
    };
    if !entry.recorded_fact_ids.iter().any(|f| f == fact_id) {
        return Err(AppError::other(format!(
            "fact {fact_id} is not in turn {turn_id}"
        )));
    }
    store.delete_fact(fact_id).await?;
    entry.recorded_fact_ids.retain(|f| f != fact_id);
    write_audit(formation_root, &AuditEntry::ChatTurn(entry))
}

/// Outcome of [`undo_task_completion`] — the audit-log panel uses this to
/// tell the user whether the bullet was removed cleanly, the file is gone,
/// or the user has edited the line since logging (refuse-on-edit).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UndoTaskCompletionResult {
    /// The bullet was found verbatim in `## Did` and removed; the audit
    /// entry's JSON file is also deleted.
    Removed,
    /// The bullet has been edited since the indexer appended it — the audit
    /// JSON is kept in place so the panel can show the failed-revert state.
    EditedSinceAppended,
    /// The daily note no longer exists; the audit JSON is deleted, since
    /// there is nothing left to revert.
    FileMissing,
}

/// Undo one task-completion append: remove the exact `appended_bullet_text`
/// from `daily_note_path`'s `## Did` section. Refuses (returns
/// `EditedSinceAppended`) if the user has edited the bullet since logging,
/// preserving their edit (ADR-0010 §8).
///
/// On `Removed` and `FileMissing` the audit entry's JSON is deleted — the
/// turn is gone from the log. On `EditedSinceAppended` the entry stays so
/// the audit-log panel can render the failed-revert state.
pub fn undo_task_completion(
    formation_root: &Path,
    entry_id: &str,
) -> AppResult<UndoTaskCompletionResult> {
    let entry = read_audit(formation_root, entry_id)?;
    let AuditEntry::TaskCompletion(entry) = entry else {
        return Err(AppError::other(format!(
            "audit entry {entry_id} is not a task-completion entry"
        )));
    };
    let daily_note_abs = formation_root.join(&entry.daily_note_path);
    let outcome = daily_note::remove_did_bullet(&daily_note_abs, &entry.appended_bullet_text)?;
    let mapped = match outcome {
        daily_note::RemoveResult::Removed => UndoTaskCompletionResult::Removed,
        daily_note::RemoveResult::EditedSinceAppended => {
            UndoTaskCompletionResult::EditedSinceAppended
        }
        daily_note::RemoveResult::FileMissing => UndoTaskCompletionResult::FileMissing,
    };
    if matches!(
        mapped,
        UndoTaskCompletionResult::Removed | UndoTaskCompletionResult::FileMissing
    ) {
        remove_audit(formation_root, entry_id)?;
    }
    Ok(mapped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir_for_test() -> PathBuf {
        let p = std::env::temp_dir()
            .join("sediment-test-audit")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&p).expect("tempdir");
        p
    }

    /// Write a note at a formation-relative path, creating parent dirs.
    fn write_note(root: &Path, rel: &str, content: &str) {
        let abs = root.join(rel);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, content).unwrap();
    }

    /// snapshot → modify one note + create another → diff detects exactly the
    /// modified and the created note, and ignores an untouched one and the
    /// `.chat-notes/` app directory.
    #[test]
    fn snapshot_then_diff_detects_created_and_modified_notes() {
        let root = tempdir_for_test();
        write_note(&root, "People/Josh.md", "## Facts\n\n- Likes Rust\n");
        write_note(&root, "People/Maria.md", "## Facts\n\n- Likes hiking\n");
        // A file inside the app dir must never be snapshotted or diffed.
        write_note(&root, ".chat-notes/config.json", "{}");

        let turn_id = new_turn_id();
        let snap = snapshot_formation(&root, &turn_id).expect("snapshot");
        assert!(snap.join("People/Josh.md").is_file(), "note snapshotted");
        assert!(
            !snap.join(".chat-notes").exists(),
            "the app dir is excluded from the snapshot"
        );

        // The turn modifies Josh, creates Devon, and leaves Maria alone.
        write_note(
            &root,
            "People/Josh.md",
            "## Facts\n\n- Likes Rust\n- Works at Cloudflare\n",
        );
        write_note(&root, "People/Devon.md", "## Facts\n\n- New hire\n");

        let changed = diff_formation(&root, &snap).expect("diff");
        assert_eq!(changed.len(), 2, "exactly Josh (modified) + Devon (new)");

        let devon = changed
            .iter()
            .find(|c| c.path == "People/Devon.md")
            .expect("Devon changed");
        assert!(devon.was_create, "Devon is a creation");

        let josh = changed
            .iter()
            .find(|c| c.path == "People/Josh.md")
            .expect("Josh changed");
        assert!(!josh.was_create, "Josh is a modification, not a creation");

        assert!(
            !changed.iter().any(|c| c.path == "People/Maria.md"),
            "an untouched note is not reported"
        );

        std::fs::remove_dir_all(root).ok();
    }

    /// A turn that touches nothing diffs to an empty change set.
    #[test]
    fn diff_of_an_untouched_formation_is_empty() {
        let root = tempdir_for_test();
        write_note(&root, "People/Josh.md", "- Likes Rust\n");
        let turn_id = new_turn_id();
        let snap = snapshot_formation(&root, &turn_id).expect("snapshot");
        assert!(
            diff_formation(&root, &snap).expect("diff").is_empty(),
            "no change → no ChangedNote"
        );
        std::fs::remove_dir_all(root).ok();
    }

    /// An audit entry — chat-turn AND task-completion — survives write → read
    /// → read_all unchanged; read_all is newest-first across kinds and
    /// tolerates a corrupt file.
    #[test]
    fn audit_entry_round_trip_and_newest_first() {
        let root = tempdir_for_test();

        let mut older = sample_chat_turn("turn_older");
        if let AuditEntry::ChatTurn(e) = &mut older {
            e.created = chrono::Utc::now() - chrono::Duration::hours(2);
        }
        let mut middle = sample_task_completion("task_completion_middle");
        if let AuditEntry::TaskCompletion(e) = &mut middle {
            e.created = chrono::Utc::now() - chrono::Duration::hours(1);
        }
        let newer = sample_chat_turn("turn_newer");
        write_audit(&root, &older).expect("write older");
        write_audit(&root, &middle).expect("write middle");
        write_audit(&root, &newer).expect("write newer");
        // A corrupt JSON file must not break the listing.
        atomic_write(&audit_dir(&root).join("turn_broken.json"), b"{ not json")
            .expect("write broken");

        let one = read_audit(&root, "turn_newer").expect("read_audit");
        assert_eq!(one.entry_id(), "turn_newer");

        let all = read_all_audit(&root).expect("read_all");
        assert_eq!(all.len(), 3, "corrupt file skipped, three valid entries");
        assert_eq!(all[0].entry_id(), "turn_newer", "newest first");
        assert_eq!(all[1].entry_id(), "task_completion_middle");
        assert_eq!(all[2].entry_id(), "turn_older");

        // The middle entry deserialises as the TaskCompletion variant.
        assert!(
            matches!(all[1], AuditEntry::TaskCompletion(_)),
            "kind discriminator round-trips"
        );

        std::fs::remove_dir_all(root).ok();
    }

    /// `prune_old` keeps exactly the N most recent entries and deletes the
    /// older ones' audit files and snapshot directories (for ChatTurn).
    #[test]
    fn prune_old_keeps_exactly_n_recent_turns() {
        let root = tempdir_for_test();
        const TOTAL: usize = 5;
        const KEEP: usize = 3;

        let mut ids = Vec::new();
        for i in 0..TOTAL {
            let id = format!("turn_{i:03}");
            ids.push(id.clone());
            // A real snapshot dir per turn, so prune has something to delete.
            let snap = snapshot_dir_for(&root, &id);
            std::fs::create_dir_all(&snap).unwrap();
            std::fs::write(snap.join("marker"), b"x").unwrap();

            let mut e = sample_chat_turn(&id);
            // Strictly increasing timestamps so newest-first is deterministic.
            if let AuditEntry::ChatTurn(c) = &mut e {
                c.created = chrono::Utc::now() + chrono::Duration::seconds(i as i64);
            }
            write_audit(&root, &e).expect("write");
        }

        prune_old(&root, KEEP).expect("prune");

        let remaining = read_all_audit(&root).expect("read_all");
        assert_eq!(remaining.len(), KEEP, "exactly KEEP entries survive");
        // The newest KEEP turns (indices 2,3,4) are kept; 0,1 are pruned.
        let kept: Vec<&str> = remaining.iter().map(|e| e.entry_id()).collect();
        assert_eq!(kept, vec!["turn_004", "turn_003", "turn_002"]);

        // The pruned turns' snapshot dirs are gone; the kept ones remain.
        assert!(!snapshot_dir_for(&root, "turn_000").exists());
        assert!(!snapshot_dir_for(&root, "turn_001").exists());
        assert!(snapshot_dir_for(&root, "turn_002").exists());
        assert!(snapshot_dir_for(&root, "turn_004").exists());

        std::fs::remove_dir_all(root).ok();
    }

    /// `undo_turn` restores a modified note, deletes a created note, removes
    /// the recorded Facts from the graph, and drops the audit entry + snapshot.
    #[tokio::test]
    async fn undo_turn_restores_notes_and_deletes_facts() {
        use crate::core::memory::{FactWriteInput, MemoryStore};

        let root = tempdir_for_test();
        let store = MemoryStore::open(&root.join(APP_DIR).join("memory"))
            .await
            .expect("open store");

        // Pre-turn state: Josh exists with one bullet.
        write_note(&root, "People/Josh.md", "## Facts\n\n- Likes Rust\n");
        let turn_id = new_turn_id();
        let snap = snapshot_formation(&root, &turn_id).expect("snapshot");

        // The turn modifies Josh, creates Devon, and records two graph Facts.
        write_note(
            &root,
            "People/Josh.md",
            "## Facts\n\n- Likes Rust\n- Works at Cloudflare\n",
        );
        write_note(&root, "People/Devon.md", "## Facts\n\n- New hire\n");

        let josh = store
            .upsert_entity("Josh", "person", vec![])
            .await
            .expect("josh")
            .id;
        let cloudflare = store
            .upsert_entity("Cloudflare", "organization", vec![])
            .await
            .expect("cf")
            .id;
        let devon = store
            .upsert_entity("Devon", "person", vec![])
            .await
            .expect("devon")
            .id;
        let f1 = store
            .relate_fact(FactWriteInput {
                subject_id: josh.clone(),
                predicate: "works_at".into(),
                object_id: cloudflare.clone(),
                valid_from: chrono::Utc::now(),
                valid_to: None,
                source_chat_id: "chat_message:turn".into(),
                confidence: 0.9,
            })
            .await
            .expect("f1");
        let f2 = store
            .relate_fact(FactWriteInput {
                subject_id: josh.clone(),
                predicate: "reports_to".into(),
                object_id: devon.clone(),
                valid_from: chrono::Utc::now(),
                valid_to: None,
                source_chat_id: "chat_message:turn".into(),
                confidence: 0.9,
            })
            .await
            .expect("f2");

        let changed = diff_formation(&root, &snap).expect("diff");
        let entry = AuditEntry::ChatTurn(ChatTurnEntry {
            turn_id: turn_id.clone(),
            created: chrono::Utc::now(),
            user_excerpt: "Josh moved to Cloudflare under Devon".into(),
            reply_excerpt: "Filed it.".into(),
            snapshot_dir: format!(".chat-notes/snapshots/{turn_id}"),
            changed_notes: changed,
            recorded_fact_ids: vec![f1.clone(), f2.clone()],
        });
        write_audit(&root, &entry).expect("write audit");
        assert_eq!(
            store.current_facts(&josh).await.expect("pre").len(),
            2,
            "two facts before undo"
        );

        undo_turn(&root, &store, &turn_id).await.expect("undo");

        // Josh is restored to the pre-turn bullet; Devon's note is gone.
        let josh_after = std::fs::read_to_string(root.join("People/Josh.md")).expect("Josh note");
        assert_eq!(josh_after, "## Facts\n\n- Likes Rust\n", "Josh restored");
        assert!(
            !root.join("People/Devon.md").exists(),
            "the created Devon note is deleted by undo"
        );
        // Both Facts removed from the graph.
        assert!(
            store.current_facts(&josh).await.expect("post").is_empty(),
            "undo deleted both recorded facts"
        );
        // The audit entry and snapshot are gone.
        assert!(read_audit(&root, &turn_id).is_err(), "audit entry removed");
        assert!(!snap.exists(), "snapshot dir removed");

        std::fs::remove_dir_all(root).ok();
    }

    /// `undo_fact` deletes one Fact and drops it from the audit entry, leaving
    /// the other Fact, the notes, and the entry itself intact.
    #[tokio::test]
    async fn undo_fact_reverts_one_fact_and_keeps_the_rest() {
        use crate::core::memory::{FactWriteInput, MemoryStore};

        let root = tempdir_for_test();
        let store = MemoryStore::open(&root.join(APP_DIR).join("memory"))
            .await
            .expect("open store");

        let josh = store
            .upsert_entity("Josh", "person", vec![])
            .await
            .expect("josh")
            .id;
        let cloudflare = store
            .upsert_entity("Cloudflare", "organization", vec![])
            .await
            .expect("cf")
            .id;
        let rust = store
            .upsert_entity("Rust", "topic", vec![])
            .await
            .expect("rust")
            .id;
        let f1 = store
            .relate_fact(FactWriteInput {
                subject_id: josh.clone(),
                predicate: "works_at".into(),
                object_id: cloudflare.clone(),
                valid_from: chrono::Utc::now(),
                valid_to: None,
                source_chat_id: "chat_message:turn".into(),
                confidence: 0.9,
            })
            .await
            .expect("f1");
        let f2 = store
            .relate_fact(FactWriteInput {
                subject_id: josh.clone(),
                predicate: "interested_in".into(),
                object_id: rust.clone(),
                valid_from: chrono::Utc::now(),
                valid_to: None,
                source_chat_id: "chat_message:turn".into(),
                confidence: 0.9,
            })
            .await
            .expect("f2");

        let turn_id = new_turn_id();
        let entry = AuditEntry::ChatTurn(ChatTurnEntry {
            turn_id: turn_id.clone(),
            created: chrono::Utc::now(),
            user_excerpt: "Josh works at Cloudflare and likes Rust".into(),
            reply_excerpt: "Recorded both.".into(),
            snapshot_dir: format!(".chat-notes/snapshots/{turn_id}"),
            changed_notes: vec![],
            recorded_fact_ids: vec![f1.clone(), f2.clone()],
        });
        write_audit(&root, &entry).expect("write audit");

        // Revert only the works_at fact.
        undo_fact(&root, &store, &turn_id, &f1)
            .await
            .expect("undo_fact");

        // f1 is gone; f2 (interested_in Rust) remains.
        let current = store.current_facts(&josh).await.expect("current");
        assert_eq!(current.len(), 1, "exactly one fact left");
        assert_eq!(current[0].predicate, "interested_in");

        // The audit entry still exists, now listing only f2.
        let after = read_audit(&root, &turn_id).expect("audit still present");
        let AuditEntry::ChatTurn(after) = after else {
            panic!("expected ChatTurn variant after undo_fact")
        };
        assert_eq!(after.recorded_fact_ids, vec![f2], "f1 dropped from audit");

        // Reverting a fact that is not in the turn is an error.
        assert!(
            undo_fact(&root, &store, &turn_id, "fact:not_here")
                .await
                .is_err(),
            "an unknown fact id is rejected"
        );

        std::fs::remove_dir_all(root).ok();
    }

    /// `write_task_completion` + `undo_task_completion` happy path: the
    /// bullet is written to the daily note, the audit entry exists, and
    /// undo removes the bullet cleanly and deletes the audit JSON.
    #[test]
    fn task_completion_round_trip_and_undo_removes_bullet() {
        let root = tempdir_for_test();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 5, 22).unwrap();
        let daily_abs = daily_note::ensure_daily_note(&root, date).expect("ensure");
        let daily_rel = daily_note::daily_note_relative_path(date);
        let bullet = "- Called the dentist";

        daily_note::append_did_bullet(&daily_abs, bullet).expect("append");
        let entry_id = write_task_completion(
            &root,
            "task:call_dentist_d4e5f6",
            "Call the dentist",
            &daily_rel,
            bullet,
        )
        .expect("write_task_completion");

        // The entry round-trips through the typed reader.
        let entry = read_audit(&root, &entry_id).expect("read");
        let AuditEntry::TaskCompletion(tc) = entry else {
            panic!("expected TaskCompletion variant")
        };
        assert_eq!(tc.task_id, "task:call_dentist_d4e5f6");
        assert_eq!(tc.daily_note_path, daily_rel);
        assert_eq!(tc.appended_bullet_text, bullet);

        // Undo removes the bullet and the audit entry.
        let outcome = undo_task_completion(&root, &entry_id).expect("undo");
        assert_eq!(outcome, UndoTaskCompletionResult::Removed);
        assert!(read_audit(&root, &entry_id).is_err(), "audit json deleted");
        let body = std::fs::read_to_string(&daily_abs).unwrap();
        assert!(!body.contains(bullet), "bullet gone from daily note");

        std::fs::remove_dir_all(root).ok();
    }

    /// `undo_task_completion` refuses (returns `EditedSinceAppended`) when
    /// the user has edited the appended bullet — the audit entry stays in
    /// place so the panel can show the failed-revert state.
    #[test]
    fn task_completion_undo_refuses_when_bullet_edited() {
        let root = tempdir_for_test();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 5, 22).unwrap();
        let daily_abs = daily_note::ensure_daily_note(&root, date).expect("ensure");
        let daily_rel = daily_note::daily_note_relative_path(date);
        let bullet = "- Watched a youtube video";

        daily_note::append_did_bullet(&daily_abs, bullet).unwrap();
        let entry_id = write_task_completion(
            &root,
            "task:watched_video_abc",
            "Watch a youtube video",
            &daily_rel,
            bullet,
        )
        .expect("write_task_completion");

        // User edits the bullet to add detail.
        let body = std::fs::read_to_string(&daily_abs).unwrap();
        let edited = body.replace(bullet, "- Watched a youtube video about Rust");
        std::fs::write(&daily_abs, edited).unwrap();

        let outcome = undo_task_completion(&root, &entry_id).expect("undo");
        assert_eq!(outcome, UndoTaskCompletionResult::EditedSinceAppended);
        assert!(
            read_audit(&root, &entry_id).is_ok(),
            "audit entry survives a refused revert so the panel can show it"
        );
        let body = std::fs::read_to_string(&daily_abs).unwrap();
        assert!(
            body.contains("- Watched a youtube video about Rust"),
            "user's edit is preserved"
        );

        std::fs::remove_dir_all(root).ok();
    }

    /// `undo_turn` refuses a TaskCompletion entry (and vice versa) — the two
    /// undo paths are kind-specific to keep their contracts sharp.
    #[tokio::test]
    async fn undo_turn_and_undo_fact_reject_task_completion_entries() {
        let root = tempdir_for_test();
        let store = MemoryStore::open(&root.join(APP_DIR).join("memory"))
            .await
            .expect("open");

        let entry_id = write_task_completion(
            &root,
            "task:foo",
            "Foo",
            "Daily Notes/2026-05-22.md",
            "- Foo",
        )
        .expect("write tc");

        let err = undo_turn(&root, &store, &entry_id).await.unwrap_err();
        assert!(
            format!("{err}").contains("not a chat-turn"),
            "undo_turn rejects task-completion: {err}"
        );
        let err = undo_fact(&root, &store, &entry_id, "fact:bar")
            .await
            .unwrap_err();
        assert!(
            format!("{err}").contains("not a chat-turn"),
            "undo_fact rejects task-completion: {err}"
        );

        std::fs::remove_dir_all(root).ok();
    }

    // ──────────────────────────────────────────────────────────────────────
    // Fixtures
    // ──────────────────────────────────────────────────────────────────────

    /// Build a minimal ChatTurn audit entry for the persistence tests.
    fn sample_chat_turn(turn_id: &str) -> AuditEntry {
        AuditEntry::ChatTurn(ChatTurnEntry {
            turn_id: turn_id.to_string(),
            created: chrono::Utc::now(),
            user_excerpt: "Josh moved to Cloudflare.".into(),
            reply_excerpt: "Filed it under People/Josh.md.".into(),
            snapshot_dir: format!(".chat-notes/snapshots/{turn_id}"),
            changed_notes: vec![ChangedNote {
                path: "People/Josh.md".into(),
                was_create: false,
            }],
            recorded_fact_ids: vec!["fact:abc".into()],
        })
    }

    /// Build a minimal TaskCompletion audit entry.
    fn sample_task_completion(entry_id: &str) -> AuditEntry {
        AuditEntry::TaskCompletion(TaskCompletionEntry {
            entry_id: entry_id.to_string(),
            created: chrono::Utc::now(),
            task_id: "task:call_dentist_d4e5f6".into(),
            task_title: "Call the dentist".into(),
            daily_note_path: "Daily Notes/2026-05-22.md".into(),
            appended_bullet_text: "- Call the dentist".into(),
        })
    }
}
