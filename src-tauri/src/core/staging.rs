//! Staging entries: extracted facts awaiting human review before they touch
//! the formation. Each `chat_write` batch produces one `StagingEntry`,
//! persisted as a JSON file under `.chat-notes/staging/`. Nothing in this
//! module writes to SurrealDB or to user notes — that happens only on an
//! explicit Keep (see `commands::staging::keep_staging`). Keeping staging
//! pre-commit and on-disk means it survives a graph rebuild (spec §6).

use crate::core::formation_state::atomic_write;
use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Whether a `NoteChange` creates a brand-new note or edits an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    Create,
    Update,
}

/// A single extracted relation, resolved enough that committing it needs no
/// re-extraction: subject/object names + types drive `upsert_entity`, and the
/// predicate + validity drive `relate_fact`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedFact {
    /// Locally-computed `entity:<slug>` id for the subject (the note's entity).
    pub subject_id: String,
    pub subject_name: String,
    pub subject_type: String,
    pub predicate: String,
    pub object_id: String,
    pub object_name: String,
    pub object_type: String,
    pub valid_from: chrono::DateTime<chrono::Utc>,
    /// True when `valid_from` was parsed from temporal phrasing in the message
    /// rather than defaulting to the message time — drives bullet rendering.
    #[serde(default)]
    pub valid_from_explicit: bool,
    pub confidence: f64,
    /// When set, the commit's `relate_fact` must NOT supersede a conflicting
    /// current fact — the user chose "Keep both" (concurrent-employment case,
    /// see ADR-0004 / refinement R3).
    #[serde(default)]
    pub explicit_coexist: bool,
}

/// An existing current fact that a staged fact would contradict.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conflict {
    /// Index into the owning `NoteChange.facts` of the staged fact in conflict.
    pub staged_fact_index: usize,
    pub predicate: String,
    pub existing_object_id: String,
    pub existing_object_name: String,
    pub existing_valid_from: chrono::DateTime<chrono::Utc>,
    pub existing_source_chat_id: String,
}

/// One note's worth of pending change within a staging entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteChange {
    pub kind: ChangeKind,
    /// Formation-relative POSIX path of the target note.
    pub note_path: String,
    /// Human-readable additive diff for the tray. The real per-chunk merge
    /// view (P3-M5) diffs `new_content` against the on-disk note directly.
    pub diff: String,
    /// Full note text after the change — what a Keep writes to disk.
    pub new_content: String,
    pub facts: Vec<StagedFact>,
    /// Lowest fact confidence in this change (most conservative summary).
    pub confidence: f64,
    /// Contradictions detected pre-commit (P3-M7). Empty when none.
    #[serde(default)]
    pub conflicts: Vec<Conflict>,
}

/// One `chat_write` batch awaiting review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagingEntry {
    /// Stable id, also the JSON filename stem: `stage_<utc-compact>_<rand>`.
    pub id: String,
    pub created: chrono::DateTime<chrono::Utc>,
    /// `chat_message:<...>` record id that produced these facts.
    pub chat_message_id: String,
    /// First chars of the originating message, for the tray header.
    pub chat_excerpt: String,
    /// "pending" — kept as a string for forward-compatibility.
    pub status: String,
    pub changes: Vec<NoteChange>,
}

impl StagingEntry {
    /// Build a fresh entry id from the current time. Millisecond precision +
    /// a short random suffix keeps two same-second batches from colliding.
    /// The compact form avoids `:` so it is a safe filename on every platform.
    pub fn new_id() -> String {
        format!(
            "stage_{}_{}",
            chrono::Utc::now().format("%Y%m%dT%H%M%S%3f"),
            &uuid::Uuid::new_v4().simple().to_string()[..6]
        )
    }
}

fn entry_file(staging_dir: &Path, id: &str) -> PathBuf {
    staging_dir.join(format!("{id}.json"))
}

/// Persist `entry` to `<staging_dir>/<id>.json` (atomic temp-file + rename).
pub fn write(staging_dir: &Path, entry: &StagingEntry) -> AppResult<()> {
    std::fs::create_dir_all(staging_dir)?;
    let bytes = serde_json::to_vec_pretty(entry)?;
    atomic_write(&entry_file(staging_dir, &entry.id), &bytes)
}

/// Read one entry by id. Errors if the file is missing or corrupt.
pub fn read_one(staging_dir: &Path, id: &str) -> AppResult<StagingEntry> {
    let path = entry_file(staging_dir, id);
    let bytes =
        std::fs::read(&path).map_err(|e| AppError::other(format!("read staging {id}: {e}")))?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Read every staging entry in `staging_dir`, oldest first. A corrupt file is
/// skipped with a warning rather than failing the whole listing.
pub fn read_all(staging_dir: &Path) -> AppResult<Vec<StagingEntry>> {
    let mut out: Vec<StagingEntry> = Vec::new();
    let Ok(dir) = std::fs::read_dir(staging_dir) else {
        return Ok(out); // dir absent — simply no staged entries
    };
    for entry in dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue; // skip `.json.tmp` atomic-write leftovers, etc.
        }
        let parsed = std::fs::read(&path)
            .map_err(AppError::from)
            .and_then(|b| serde_json::from_slice::<StagingEntry>(&b).map_err(AppError::from));
        match parsed {
            Ok(e) => out.push(e),
            Err(e) => tracing::warn!("skipping corrupt staging file {}: {e}", path.display()),
        }
    }
    out.sort_by_key(|e| e.created);
    Ok(out)
}

/// Delete one staging entry's JSON file. A missing file is not an error —
/// discard is idempotent.
pub fn remove(staging_dir: &Path, id: &str) -> AppResult<()> {
    match std::fs::remove_file(entry_file(staging_dir, id)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(AppError::other(format!("remove staging {id}: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir_for_test() -> PathBuf {
        let p = std::env::temp_dir()
            .join("sediment-test-staging")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&p).expect("tempdir");
        p
    }

    fn sample_entry(id: &str) -> StagingEntry {
        StagingEntry {
            id: id.to_string(),
            created: chrono::Utc::now(),
            chat_message_id: "chat_message:abc".into(),
            chat_excerpt: "Bill Gates founded Microsoft.".into(),
            status: "pending".into(),
            changes: vec![NoteChange {
                kind: ChangeKind::Create,
                note_path: "People/Bill Gates.md".into(),
                diff: "+ Founded Microsoft".into(),
                new_content: "## Facts\n\n- Founded Microsoft\n".into(),
                confidence: 0.91,
                conflicts: vec![],
                facts: vec![StagedFact {
                    subject_id: "entity:bill_gates".into(),
                    subject_name: "Bill Gates".into(),
                    subject_type: "person".into(),
                    predicate: "founded".into(),
                    object_id: "entity:microsoft".into(),
                    object_name: "Microsoft".into(),
                    object_type: "organization".into(),
                    valid_from: chrono::Utc::now(),
                    valid_from_explicit: false,
                    confidence: 0.91,
                    explicit_coexist: false,
                }],
            }],
        }
    }

    /// A staging entry survives write → read_one → read_all unchanged, and
    /// `remove` deletes only that file (no other effect).
    #[test]
    fn staging_entry_round_trip() {
        let dir = tempdir_for_test();
        let entry = sample_entry(&StagingEntry::new_id());
        write(&dir, &entry).expect("write");

        let one = read_one(&dir, &entry.id).expect("read_one");
        assert_eq!(one.id, entry.id);
        assert_eq!(one.changes.len(), 1);
        assert_eq!(one.changes[0].note_path, "People/Bill Gates.md");
        assert_eq!(one.changes[0].facts[0].predicate, "founded");

        let all = read_all(&dir).expect("read_all");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, entry.id);

        remove(&dir, &entry.id).expect("remove");
        assert!(read_all(&dir).expect("read_all after remove").is_empty());
        // Removing an already-gone entry is a no-op, not an error.
        remove(&dir, &entry.id).expect("idempotent remove");

        std::fs::remove_dir_all(dir).ok();
    }

    /// read_all returns entries oldest-first and tolerates corrupt files.
    #[test]
    fn read_all_orders_and_skips_corrupt() {
        let dir = tempdir_for_test();

        let mut older = sample_entry("stage_older");
        older.created = chrono::Utc::now() - chrono::Duration::hours(1);
        let newer = sample_entry("stage_newer");
        write(&dir, &older).expect("write older");
        write(&dir, &newer).expect("write newer");

        // A corrupt JSON file must not break the listing.
        atomic_write(&dir.join("stage_broken.json"), b"{ not json").expect("write broken");

        let all = read_all(&dir).expect("read_all");
        assert_eq!(all.len(), 2, "corrupt file skipped, two valid entries kept");
        assert_eq!(all[0].id, "stage_older", "oldest first");
        assert_eq!(all[1].id, "stage_newer");

        std::fs::remove_dir_all(dir).ok();
    }

    /// read_all on a never-created staging dir yields an empty list, not an error.
    #[test]
    fn read_all_missing_dir_is_empty() {
        let missing = tempdir_for_test().join("never-made");
        assert!(read_all(&missing).expect("read_all missing").is_empty());
    }
}
