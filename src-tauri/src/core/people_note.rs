//! People notes — `People/<Name>.md`, the per-person file (ADR-0009/0015).
//!
//! A **People note** is the Note of a `person` Entity: where the agent records what
//! it learns about someone (their `## Facts`, history, connections). The agent
//! normally authors these in the flow of a turn, but some flows need to *guarantee*
//! one exists before handing off — notably assigning a meeting speaker to a person
//! (ADR-0017 §6): naming "Unknown speaker 2" as Sarah should give Sarah a file the
//! `[[Sarah Chen]]` attendee link resolves to, even if the agent hasn't written
//! about her yet.
//!
//! This is the small, pure helper for that: materialise `People/<Name>.md` with an
//! empty `## Facts` frame if it is absent, idempotently. It mirrors
//! `daily_note`/`meeting_note` — same `atomic_write`, same ensure-if-absent idiom.

use crate::core::formation_state::atomic_write;
use crate::core::meeting_note::sanitize_title;
use crate::error::AppResult;
use std::path::{Path, PathBuf};

/// Folder under the formation root holding `People/*.md`. Matches the convention
/// the agent already uses (`People/Josh.md`, …) and the `noteTypeLabel` mapping.
pub const PEOPLE_DIR: &str = "People";

/// `People/<Name>.md` for `name`, as a formation-relative POSIX path. The name is
/// sanitised into a safe filename component (path separators dropped, whitespace
/// collapsed) but otherwise preserved — these are real Obsidian filenames, so the
/// `[[Name]]` wiki-link resolves to this file.
pub fn person_note_relative_path(name: &str) -> String {
    format!("{PEOPLE_DIR}/{}.md", sanitize_title(name))
}

/// Create the People note for `name` if it does not exist, returning its
/// formation-relative path either way. Idempotent — a second call (or a note the
/// agent already wrote) is a no-op; the existing file is never rewritten.
pub fn ensure_person_note(formation_root: &Path, name: &str) -> AppResult<String> {
    let rel = person_note_relative_path(name);
    let abs: PathBuf = formation_root.join(&rel);
    if abs.is_file() {
        return Ok(rel);
    }
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent)?;
    }
    atomic_write(&abs, render_initial_person_note(name).as_bytes())?;
    Ok(rel)
}

/// The empty frame for a fresh People note: a title and an empty `## Facts`
/// section, matching where the agent records Facts about a person.
fn render_initial_person_note(name: &str) -> String {
    let title = sanitize_title(name);
    format!("# {title}\n\n## Facts\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir()
            .join("sediment-test-people-note")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn relative_path_is_sanitised() {
        assert_eq!(
            person_note_relative_path("Sarah Chen"),
            "People/Sarah Chen.md"
        );
        assert_eq!(person_note_relative_path("a/b:c"), "People/a b c.md");
    }

    #[test]
    fn ensure_creates_once_and_is_idempotent() {
        let root = tempdir();
        let rel = ensure_person_note(&root, "Sarah Chen").unwrap();
        assert_eq!(rel, "People/Sarah Chen.md");
        let abs = root.join(&rel);
        assert!(abs.is_file());
        let body = std::fs::read_to_string(&abs).unwrap();
        assert!(body.starts_with("# Sarah Chen"));
        assert!(body.contains("## Facts"));

        // A second call does not overwrite hand/agent edits.
        std::fs::write(&abs, "# edited\n").unwrap();
        let rel2 = ensure_person_note(&root, "Sarah Chen").unwrap();
        assert_eq!(rel2, rel);
        assert_eq!(std::fs::read_to_string(&abs).unwrap(), "# edited\n");
        std::fs::remove_dir_all(root).ok();
    }
}
