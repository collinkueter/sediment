//! The **Self** — the user's own durable model (ADR-0015).
//!
//! The user is modeled as the reserved `Self` Entity with a Note `Self.md`. That
//! note carries a `## Summary` region — a handful of lines of stable identity
//! (core preferences, working style, standing goals) the agent keeps current as
//! part of normal authoring. The deterministic pre-pass injects that section
//! verbatim as the **highest-priority** grounding slot, above the Working Set, so
//! the agent never starts a turn without knowing who it is talking to (ADR-0015
//! §3, amending ADR-0011 §2).
//!
//! Read-only here: the agent authors `Self.md` with its own file tools — lazily,
//! in-turn (ADR-0015 §2/§5). This module only *extracts* the summary for
//! grounding. Best-effort: a missing file, a missing `## Summary`, or an empty
//! section yields no section, never an error.

use std::path::Path;

/// The user's Self note, a formation-relative path. Plain Markdown,
/// Obsidian-compatible, user-editable (ADR-0015 §5).
pub const SELF_NOTE_PATH: &str = "Self.md";

/// The always-injected region of `Self.md` (ADR-0015 §3).
pub const SUMMARY_HEADING: &str = "## Summary";

/// Cap on the injected Self summary, in characters. Small and fixed so the slot —
/// ranked first under `INJECTED_CONTEXT_BUDGET` — can never be the section that
/// crowds out the rest of the grounding (ADR-0011 open Q3; ADR-0015 §3).
const SELF_SUMMARY_BUDGET: usize = 1200;

/// The Self grounding section: the `## Summary` region of `Self.md`, rendered as a
/// top-priority `## About you` block to push into the turn. `None` when there is
/// no `Self.md`, no `## Summary`, or the section is empty — i.e. the agent has not
/// yet learned anything durable about the user.
pub fn summary_for_grounding(formation_root: &Path) -> Option<String> {
    let summary = truncate_chars(&summary_text(formation_root)?, SELF_SUMMARY_BUDGET);
    Some(format!(
        "## About you\n\
         The durable model of the person you are talking to — treat it as current.\n\n\
         {summary}"
    ))
}

/// The raw `## Summary` body of `Self.md` — the agent's stated identity, trimmed.
/// `None` when there is no Self note, no `## Summary`, or the section is empty. For
/// *display* (the "in focus" panel, ADR-0015 §5); the grounding form is
/// [`summary_for_grounding`].
pub fn summary_text(formation_root: &Path) -> Option<String> {
    let content = std::fs::read_to_string(formation_root.join(SELF_NOTE_PATH)).ok()?;
    let summary = extract_section(&content, SUMMARY_HEADING)?;
    let summary = summary.trim();
    if summary.is_empty() {
        None
    } else {
        Some(summary.to_string())
    }
}

/// The body of the `## <heading>` section — every line between the heading and the
/// next ATX heading (or end of file). `None` if the heading is absent.
fn extract_section(content: &str, heading: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.iter().position(|l| l.trim_end() == heading)?;
    let end = lines[start + 1..]
        .iter()
        .position(|l| is_heading(l))
        .map(|rel| start + 1 + rel)
        .unwrap_or(lines.len());
    Some(lines[start + 1..end].join("\n"))
}

/// A Markdown ATX heading line — the same definition `daily_note::is_heading`
/// uses, kept local so the modules stay independent.
fn is_heading(line: &str) -> bool {
    let t = line.trim_start();
    let hashes = t.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hashes) && t[hashes..].starts_with(' ')
}

/// Truncate to at most `max` characters on a char boundary, with a trailing
/// ellipsis when clipped.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir()
            .join("sediment-test-self-model")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&p).expect("tempdir");
        p
    }

    /// The happy path: a `Self.md` with a `## Summary` yields a top-priority
    /// `## About you` grounding block carrying the summary verbatim.
    #[test]
    fn summary_is_extracted_and_framed() {
        let root = tempdir();
        std::fs::write(
            root.join(SELF_NOTE_PATH),
            "# Self\n\n\
             ## Summary\n\
             - Prefers async over meetings\n\
             - Shipping Sediment V1 by August\n\n\
             ## Work\n\
             - Leads [[Sediment]]\n",
        )
        .unwrap();

        let block = summary_for_grounding(&root).expect("a summary block");
        assert!(block.starts_with("## About you"), "framed as the Self slot");
        assert!(block.contains("Prefers async over meetings"));
        assert!(block.contains("Shipping Sediment V1 by August"));
        // The section is scoped: later sections of Self.md are not pulled in.
        assert!(
            !block.contains("Leads [[Sediment]]"),
            "stops at the next heading"
        );

        std::fs::remove_dir_all(root).ok();
    }

    /// No `Self.md` yet (the agent has learned nothing durable) → no section.
    #[test]
    fn missing_self_note_yields_none() {
        let root = tempdir();
        assert!(summary_for_grounding(&root).is_none());
        std::fs::remove_dir_all(root).ok();
    }

    /// A `Self.md` without a `## Summary`, or with an empty one, yields no section.
    #[test]
    fn missing_or_empty_summary_yields_none() {
        let root = tempdir();
        std::fs::write(
            root.join(SELF_NOTE_PATH),
            "# Self\n\n## Work\n- Leads things\n",
        )
        .unwrap();
        assert!(
            summary_for_grounding(&root).is_none(),
            "no ## Summary section"
        );

        std::fs::write(root.join(SELF_NOTE_PATH), "## Summary\n\n\n## Work\n- x\n").unwrap();
        assert!(summary_for_grounding(&root).is_none(), "empty ## Summary");

        std::fs::remove_dir_all(root).ok();
    }

    /// An over-long summary is clipped so the first-priority slot stays bounded.
    #[test]
    fn long_summary_is_truncated() {
        let root = tempdir();
        let big = "x".repeat(SELF_SUMMARY_BUDGET + 500);
        std::fs::write(root.join(SELF_NOTE_PATH), format!("## Summary\n{big}\n")).unwrap();
        let block = summary_for_grounding(&root).expect("block");
        assert!(block.ends_with('…'), "clipped with an ellipsis");
        // The summary body stays within budget (block also has the fixed header).
        assert!(block.chars().count() <= SELF_SUMMARY_BUDGET + 120);
        std::fs::remove_dir_all(root).ok();
    }
}
