//! Fact routing: which note does a fact belong in?
//!
//! Phase 3 decision #2 — a fact `(subject, predicate, object)` is filed in the
//! *subject* entity's note. If the subject already has a `note_path`, the fact
//! updates that note; otherwise it creates `<Folder>/<Name>.md`, where the
//! folder is derived from the entity type.

use crate::core::staging::ChangeKind;

/// Folder for a new note, by entity type. Unknown types fall back to `Notes`.
pub fn entity_type_folder(entity_type: &str) -> &'static str {
    match entity_type {
        "person" => "People",
        "organization" => "Organizations",
        "project" => "Projects",
        "meeting" => "Meetings",
        "task" => "Tasks",
        _ => "Notes",
    }
}

/// Decide the target note for a fact whose subject is the given entity.
///
/// `existing_note_path` is the subject entity's `note_path` field, if the
/// entity is already in the graph and linked to a note. When present the fact
/// updates that note; otherwise it creates `<Folder>/<Name>.md`.
pub fn route_fact(
    entity_type: &str,
    canonical_name: &str,
    existing_note_path: Option<&str>,
) -> (String, ChangeKind) {
    match existing_note_path {
        Some(p) if !p.trim().is_empty() => (p.to_string(), ChangeKind::Update),
        _ => {
            let folder = entity_type_folder(entity_type);
            let name = sanitize_note_name(canonical_name);
            (format!("{folder}/{name}.md"), ChangeKind::Create)
        }
    }
}

/// Make an entity's canonical name safe as a note file name. Obsidian allows
/// spaces and most punctuation in note names; only the path-structural and
/// OS-reserved characters are replaced. Whitespace is collapsed and trimmed.
pub fn sanitize_note_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_matches(['.', '-', ' ']).to_string();
    if trimmed.is_empty() {
        "Untitled".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_mapping_covers_known_types_and_falls_back() {
        assert_eq!(entity_type_folder("person"), "People");
        assert_eq!(entity_type_folder("organization"), "Organizations");
        assert_eq!(entity_type_folder("project"), "Projects");
        assert_eq!(entity_type_folder("meeting"), "Meetings");
        assert_eq!(entity_type_folder("task"), "Tasks");
        assert_eq!(entity_type_folder("topic"), "Notes");
        assert_eq!(entity_type_folder("location"), "Notes");
    }

    #[test]
    fn route_to_existing_note_is_an_update() {
        let (path, kind) = route_fact("person", "Bill Gates", Some("People/Bill Gates.md"));
        assert_eq!(path, "People/Bill Gates.md");
        assert_eq!(kind, ChangeKind::Update);
    }

    #[test]
    fn route_without_note_path_is_a_create_under_the_type_folder() {
        let (path, kind) = route_fact("person", "Bill Gates", None);
        assert_eq!(path, "People/Bill Gates.md");
        assert_eq!(kind, ChangeKind::Create);

        let (path, _) = route_fact("organization", "Acme Corp", None);
        assert_eq!(path, "Organizations/Acme Corp.md");

        // An empty/whitespace note_path is treated as unset.
        let (_, kind) = route_fact("person", "X", Some("  "));
        assert_eq!(kind, ChangeKind::Create);
    }

    #[test]
    fn sanitize_note_name_strips_path_unsafe_characters() {
        assert_eq!(sanitize_note_name("Bill Gates"), "Bill Gates");
        assert_eq!(sanitize_note_name("J.P. Morgan & Co."), "J.P. Morgan & Co");
        assert_eq!(sanitize_note_name("A/B  Testing"), "A-B Testing");
        assert_eq!(sanitize_note_name("re: budget"), "re- budget");
        assert_eq!(sanitize_note_name("   "), "Untitled");
    }
}
