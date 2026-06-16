//! Daily notes — `Daily Notes/<YYYY-MM-DD>.md` (ADR-0010, plan M3).
//!
//! A **Daily note** is a Markdown file capturing what the user did that day:
//! recurring `## Checklist` items (seeded from `Templates/Daily.md`), `## Did`
//! event bullets the agent and the indexer append, and `## Notes` reflections.
//! The file convention is plural-folder + ISO-8601 filename so the formation
//! stays a first-class Obsidian vault (ADR-0010 decision 1).
//!
//! This module is the **pure helper layer** the indexer and (later) the agent
//! share when they need to materialise today's note or splice a bullet into
//! `## Did`. It owns:
//!
//! - resolving "today" against strict-local-calendar (ADR-0010 decision 9),
//! - seeding a missing note from `Templates/Daily.md` (and seeding *that*
//!   from [`DEFAULT_DAILY_TEMPLATE`] if absent — the agent's prompt promises
//!   that creating a daily note never fails for lack of a template),
//! - an idempotent `## Did` append that survives the section being missing
//!   from the file,
//! - a refuse-on-edit `## Did` remove for undo (ADR-0010 decision 8): if the
//!   user has edited the appended bullet since it was logged, the remove
//!   reports `EditedSinceAppended` rather than destroying the edits.
//!
//! Every write goes through `atomic_write`, so a crash mid-write cannot
//! truncate a daily note. Section splitting follows the same `is_heading`
//! idiom as `task_note.rs` so the parsing is uniform across the codebase.

use crate::core::formation_state::atomic_write;
use crate::error::{AppError, AppResult};
use chrono::NaiveDate;
use std::path::{Path, PathBuf};

/// Folder under the formation root that holds `Daily Notes/<YYYY-MM-DD>.md`
/// files. Plural matches `People/`, `Organizations/`, … (ADR-0010 decision 1).
pub const DAILY_NOTES_DIR: &str = "Daily Notes";

/// Folder under the formation root that holds user-editable templates. The
/// agent and the indexer both read `Templates/Daily.md` from here when
/// seeding a fresh daily note (ADR-0010 decision 3).
pub const TEMPLATES_DIR: &str = "Templates";

/// Filename of the daily-note template inside [`TEMPLATES_DIR`].
pub const DAILY_TEMPLATE_FILENAME: &str = "Daily.md";

/// The `## Did` section heading — events the user reports in conversation,
/// and the bullets the indexer appends on a `Tasks.md` open→done transition.
pub const DID_HEADING: &str = "## Did";

/// The `## Checklist` section heading — recurring items seeded from the
/// template. The indexer never touches this section; only the agent flips
/// boxes here.
pub const CHECKLIST_HEADING: &str = "## Checklist";

/// The `## Notes` section heading — reflections / observations / sub-bullets.
pub const NOTES_HEADING: &str = "## Notes";

/// Fallback `Templates/Daily.md` written when the user has not created one.
/// Per ADR-0010 decision 3, the template is **checklist-only** content — the
/// `## Did` and `## Notes` headings grow on the daily note itself, not from
/// the template. The single example bullet tells the user where to edit.
pub const DEFAULT_DAILY_TEMPLATE: &str = "\
- [ ] Take vitamins
- [ ] 30 min reading
";

// ──────────────────────────────────────────────────────────────────────────
// Paths and "today"
// ──────────────────────────────────────────────────────────────────────────

/// The user's current local date. Strict calendar — flips at midnight in the
/// user's local zone, no felt-day cutoff (ADR-0010 decision 9).
pub fn today_local() -> NaiveDate {
    chrono::Local::now().date_naive()
}

/// `Daily Notes/<YYYY-MM-DD>.md` for `date`, as a formation-relative POSIX
/// path. The string lives in the audit-log entry, so it must round-trip
/// across platforms — forward slash, always.
pub fn daily_note_relative_path(date: NaiveDate) -> String {
    format!("{DAILY_NOTES_DIR}/{}.md", date.format("%Y-%m-%d"))
}

/// `Templates/Daily.md` as a formation-relative POSIX path.
pub fn daily_template_relative_path() -> String {
    format!("{TEMPLATES_DIR}/{DAILY_TEMPLATE_FILENAME}")
}

// ──────────────────────────────────────────────────────────────────────────
// Ensure today's daily note exists
// ──────────────────────────────────────────────────────────────────────────

/// Materialise `Daily Notes/<date>.md` if missing, returning its absolute
/// path either way.
///
/// On miss: read `Templates/Daily.md` (writing [`DEFAULT_DAILY_TEMPLATE`]
/// there first if *that* is also missing) and seed a fresh daily note with
/// `## Checklist` populated from the template plus empty `## Did` and
/// `## Notes` headings so later edits land in the right place.
///
/// Idempotent — calling twice for the same date is a no-op on the second
/// call (the file exists, so the template is not re-read).
pub fn ensure_daily_note(formation_root: &Path, date: NaiveDate) -> AppResult<PathBuf> {
    let rel = daily_note_relative_path(date);
    let abs = formation_root.join(&rel);
    if abs.is_file() {
        return Ok(abs);
    }

    // Seed the daily-note body from the template's checklist content. The
    // template owns ONLY `## Checklist` lines (ADR-0010 decision 3); we wrap
    // those with the section frame the daily note needs.
    let template_body = ensure_daily_template(formation_root)?;
    let body = render_initial_daily_note(&template_body);

    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent)?;
    }
    atomic_write(&abs, body.as_bytes())?;
    Ok(abs)
}

/// Materialise `Templates/Daily.md` if missing — returns its contents either
/// way. The template is checklist-only; the caller wraps it into the daily
/// note's section frame.
fn ensure_daily_template(formation_root: &Path) -> AppResult<String> {
    let abs = formation_root.join(daily_template_relative_path());
    if let Ok(existing) = std::fs::read_to_string(&abs) {
        return Ok(existing);
    }
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent)?;
    }
    atomic_write(&abs, DEFAULT_DAILY_TEMPLATE.as_bytes())?;
    Ok(DEFAULT_DAILY_TEMPLATE.to_string())
}

/// Frame a freshly-created daily note: `## Checklist` (template body) +
/// empty `## Did` + empty `## Notes`. The empty headings are deliberate —
/// the agent's later edits and the indexer's appends find their section
/// without having to insert a heading.
fn render_initial_daily_note(template_body: &str) -> String {
    let checklist = template_body.trim_end_matches('\n');
    format!("{CHECKLIST_HEADING}\n\n{checklist}\n\n{DID_HEADING}\n\n{NOTES_HEADING}\n")
}

// ──────────────────────────────────────────────────────────────────────────
// Append / remove `## Did` bullets
// ──────────────────────────────────────────────────────────────────────────

/// Append `bullet` (a `- ...` line; caller owns the leading dash) to the
/// `## Did` section of `daily_note_abs`. If the section is missing, the
/// heading is added at the end of the file and then the bullet.
///
/// **Idempotent on bullet text:** if a line identical to `bullet` already
/// exists anywhere inside the `## Did` section, the append is skipped. This
/// is a safety net — the real idempotence guarantee for the
/// `Tasks.md` open→done flow is the transition-detection upstream
/// (`core::tasks::reconcile_tasks_md` returns events once per transition).
///
/// Writes the file atomically. Returns `Ok(())` whether or not a write
/// actually happened.
pub fn append_did_bullet(daily_note_abs: &Path, bullet: &str) -> AppResult<()> {
    let bullet_line = bullet.trim_end_matches('\n').to_string();
    let content = std::fs::read_to_string(daily_note_abs).map_err(|e| {
        AppError::other(format!("read daily note {}: {e}", daily_note_abs.display()))
    })?;

    let updated = match splice_did_append(&content, &bullet_line) {
        SpliceOutcome::AlreadyPresent => return Ok(()),
        SpliceOutcome::Written(s) => s,
    };
    atomic_write(daily_note_abs, updated.as_bytes())?;
    Ok(())
}

/// Outcome of [`remove_did_bullet`]. The variants are how the undo layer
/// distinguishes "removed cleanly", "user has edited it — leave alone", and
/// "the file is gone, nothing to undo."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveResult {
    /// The exact bullet line was found in `## Did` and removed.
    Removed,
    /// The daily note exists but does not contain `bullet` verbatim — the
    /// user has edited the appended text. ADR-0010 decision 8: undo refuses
    /// to destroy the user's edit.
    EditedSinceAppended,
    /// The daily note does not exist — nothing to revert.
    FileMissing,
}

/// Remove a single occurrence of `bullet` from the `## Did` section of
/// `daily_note_abs`. Never deletes a line that is not an exact match for
/// `bullet` — the refuse-on-edit guarantee (ADR-0010 decision 8).
///
/// Looks at the `## Did` region only; an identical bullet that happens to
/// live elsewhere (the user pasted it into `## Notes`, say) is not removed.
pub fn remove_did_bullet(daily_note_abs: &Path, bullet: &str) -> AppResult<RemoveResult> {
    let bullet_line = bullet.trim_end_matches('\n');
    let content = match std::fs::read_to_string(daily_note_abs) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RemoveResult::FileMissing);
        }
        Err(e) => {
            return Err(AppError::other(format!(
                "read daily note {}: {e}",
                daily_note_abs.display()
            )));
        }
    };

    match splice_did_remove(&content, bullet_line) {
        RemoveOutcome::NotFound => Ok(RemoveResult::EditedSinceAppended),
        RemoveOutcome::Removed(updated) => {
            atomic_write(daily_note_abs, updated.as_bytes())?;
            Ok(RemoveResult::Removed)
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Section splice helpers (pure, testable)
// ──────────────────────────────────────────────────────────────────────────

enum SpliceOutcome {
    /// The bullet already lives in `## Did` — nothing to write.
    AlreadyPresent,
    /// New file contents — caller writes them.
    Written(String),
}

enum RemoveOutcome {
    /// The bullet line was not found verbatim inside `## Did`.
    NotFound,
    /// The bullet line was removed; here is the new file content.
    Removed(String),
}

/// Splice `bullet_line` into the `## Did` section of `content`. If the
/// section is missing, the heading is appended to the file and the bullet
/// goes under it.
fn splice_did_append(content: &str, bullet_line: &str) -> SpliceOutcome {
    let lines: Vec<&str> = content.lines().collect();
    match find_section(&lines, DID_HEADING) {
        Some((heading_idx, section_end)) => {
            // Idempotence: bail if the line already exists inside the section.
            let in_section = &lines[heading_idx + 1..section_end];
            if in_section.iter().any(|l| l.trim_end() == bullet_line) {
                return SpliceOutcome::AlreadyPresent;
            }
            // Drop trailing blank lines in the section so the bullet hugs the
            // last existing bullet; the section's blank-line tail is restored
            // when joining the next section back on.
            let mut new_section: Vec<&str> = in_section.to_vec();
            while matches!(new_section.last(), Some(s) if s.trim().is_empty()) {
                new_section.pop();
            }
            let mut out: Vec<String> = Vec::with_capacity(lines.len() + 3);
            out.extend(lines[..=heading_idx].iter().map(|s| (*s).to_string()));
            // A blank line after the heading, then the existing bullets, then
            // the new bullet. If the section was empty, this still produces a
            // clean `## Did\n\n- new bullet\n`.
            out.push(String::new());
            for l in &new_section {
                out.push((*l).to_string());
            }
            out.push(bullet_line.to_string());
            // Blank separator before the next section (if any).
            if section_end < lines.len() {
                out.push(String::new());
                for l in &lines[section_end..] {
                    out.push((*l).to_string());
                }
            }
            SpliceOutcome::Written(finalize(&out, content))
        }
        None => {
            // `## Did` heading is absent — add it to the end of the file.
            let mut out: Vec<String> = lines.iter().map(|s| (*s).to_string()).collect();
            // Ensure a blank line before the new section.
            if !out.last().map(|l| l.trim().is_empty()).unwrap_or(true) {
                out.push(String::new());
            }
            out.push(DID_HEADING.to_string());
            out.push(String::new());
            out.push(bullet_line.to_string());
            SpliceOutcome::Written(finalize(&out, content))
        }
    }
}

/// Remove a verbatim `bullet_line` from inside `## Did`. The line must match
/// (after `trim_end`) exactly — the safety guarantee for refuse-on-edit.
fn splice_did_remove(content: &str, bullet_line: &str) -> RemoveOutcome {
    let lines: Vec<&str> = content.lines().collect();
    let Some((heading_idx, section_end)) = find_section(&lines, DID_HEADING) else {
        return RemoveOutcome::NotFound;
    };
    let hit = lines
        .iter()
        .enumerate()
        .skip(heading_idx + 1)
        .take(section_end - (heading_idx + 1))
        .find(|(_, l)| l.trim_end() == bullet_line)
        .map(|(i, _)| i);
    let Some(remove_idx) = hit else {
        return RemoveOutcome::NotFound;
    };
    let mut out: Vec<String> = Vec::with_capacity(lines.len() - 1);
    for (i, l) in lines.iter().enumerate() {
        if i == remove_idx {
            continue;
        }
        out.push((*l).to_string());
    }
    RemoveOutcome::Removed(finalize(&out, content))
}

/// Join `lines` with `\n` and restore the trailing newline iff `original`
/// had one. Keeps the file's tail consistent across writes.
fn finalize(lines: &[String], original: &str) -> String {
    let joined = lines.join("\n");
    if original.ends_with('\n') && !joined.ends_with('\n') {
        format!("{joined}\n")
    } else {
        joined
    }
}

/// Locate the `## <heading>` section of `lines`: returns
/// `(heading_index, next_section_or_eof_index)`. The slice
/// `lines[heading_index+1 .. next_section]` is the section body.
fn find_section(lines: &[&str], heading: &str) -> Option<(usize, usize)> {
    let h = lines.iter().position(|l| l.trim_end() == heading)?;
    let next = lines[h + 1..]
        .iter()
        .position(|l| is_heading(l))
        .map(|rel| h + 1 + rel)
        .unwrap_or(lines.len());
    Some((h, next))
}

/// A markdown ATX heading line — same definition `task_note::is_heading`
/// uses, kept local so the two modules stay independent.
fn is_heading(line: &str) -> bool {
    let t = line.trim_start();
    let hashes = t.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hashes) && t[hashes..].starts_with(' ')
}

// ──────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir_for_test() -> PathBuf {
        let p = std::env::temp_dir()
            .join("sediment-test-daily-note")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&p).expect("tempdir");
        p
    }

    /// `today_local` and `daily_note_relative_path` produce a stable POSIX
    /// path with the date formatted as ISO 8601.
    #[test]
    fn relative_path_is_iso_and_posix() {
        let d = NaiveDate::from_ymd_opt(2026, 5, 22).unwrap();
        assert_eq!(daily_note_relative_path(d), "Daily Notes/2026-05-22.md");
        // today_local is just sanity — it must produce a parseable date.
        let today = today_local();
        let path = daily_note_relative_path(today);
        assert!(path.starts_with("Daily Notes/"));
        assert!(path.ends_with(".md"));
    }

    /// `ensure_daily_note` creates `Templates/Daily.md` from the default if
    /// it does not exist, then seeds today's daily note from it. A second
    /// call is a no-op.
    #[test]
    fn ensure_creates_template_and_daily_note_when_missing() {
        let root = tempdir_for_test();
        let date = NaiveDate::from_ymd_opt(2026, 5, 22).unwrap();
        let abs = ensure_daily_note(&root, date).expect("ensure");
        assert!(
            abs.is_file(),
            "today's daily note materialised at {}",
            abs.display()
        );
        assert!(
            root.join(daily_template_relative_path()).is_file(),
            "the template is also created from the default"
        );

        let body = std::fs::read_to_string(&abs).unwrap();
        assert!(
            body.starts_with("## Checklist"),
            "starts with the checklist"
        );
        assert!(body.contains("- [ ] Take vitamins"), "default seeded");
        assert!(body.contains("## Did"), "## Did heading exists");
        assert!(body.contains("## Notes"), "## Notes heading exists");

        // Second call is a no-op: same path, the file is not rewritten with
        // a new template even if we mutate the template body.
        std::fs::write(
            root.join(daily_template_relative_path()),
            "- [ ] Different\n",
        )
        .unwrap();
        let abs_again = ensure_daily_note(&root, date).expect("ensure again");
        assert_eq!(abs_again, abs);
        let body_again = std::fs::read_to_string(&abs_again).unwrap();
        assert!(
            body_again.contains("Take vitamins"),
            "the existing daily note is not regenerated"
        );

        std::fs::remove_dir_all(root).ok();
    }

    /// `ensure_daily_note` uses an existing `Templates/Daily.md` rather than
    /// the default when one is present.
    #[test]
    fn ensure_seeds_from_existing_template() {
        let root = tempdir_for_test();
        // Pre-create a custom template.
        let tmpl_abs = root.join(daily_template_relative_path());
        std::fs::create_dir_all(tmpl_abs.parent().unwrap()).unwrap();
        std::fs::write(&tmpl_abs, "- [ ] Meditate\n- [ ] Stretch\n").unwrap();

        let date = NaiveDate::from_ymd_opt(2026, 5, 22).unwrap();
        let abs = ensure_daily_note(&root, date).expect("ensure");
        let body = std::fs::read_to_string(&abs).unwrap();
        assert!(body.contains("- [ ] Meditate"));
        assert!(body.contains("- [ ] Stretch"));
        assert!(!body.contains("Take vitamins"));

        std::fs::remove_dir_all(root).ok();
    }

    /// `append_did_bullet` adds the line under `## Did`, leaving the
    /// surrounding sections intact.
    #[test]
    fn append_inserts_bullet_under_did_and_preserves_neighbours() {
        let root = tempdir_for_test();
        let date = NaiveDate::from_ymd_opt(2026, 5, 22).unwrap();
        let abs = ensure_daily_note(&root, date).expect("ensure");
        append_did_bullet(&abs, "- Called the dentist").expect("append");

        let body = std::fs::read_to_string(&abs).unwrap();
        let did = body.find("## Did").unwrap();
        let notes = body.find("## Notes").unwrap();
        assert!(did < notes, "section order preserved");

        let did_section = &body[did..notes];
        assert!(
            did_section.contains("- Called the dentist"),
            "bullet lands under ## Did"
        );

        // ## Checklist is untouched.
        assert!(body.contains("- [ ] Take vitamins"));

        std::fs::remove_dir_all(root).ok();
    }

    /// `append_did_bullet` is idempotent on identical bullet text.
    #[test]
    fn append_is_idempotent_on_exact_match() {
        let root = tempdir_for_test();
        let date = NaiveDate::from_ymd_opt(2026, 5, 22).unwrap();
        let abs = ensure_daily_note(&root, date).expect("ensure");
        append_did_bullet(&abs, "- Called the dentist").unwrap();
        append_did_bullet(&abs, "- Called the dentist").unwrap();
        let body = std::fs::read_to_string(&abs).unwrap();
        assert_eq!(
            body.matches("- Called the dentist").count(),
            1,
            "second append for an identical bullet is a no-op"
        );

        std::fs::remove_dir_all(root).ok();
    }

    /// `append_did_bullet` creates the `## Did` heading if the daily note
    /// doesn't have it yet.
    #[test]
    fn append_creates_heading_when_missing() {
        let root = tempdir_for_test();
        let date = NaiveDate::from_ymd_opt(2026, 5, 22).unwrap();
        let abs_rel = daily_note_relative_path(date);
        let abs = root.join(&abs_rel);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        // A daily note that only has a checklist — no `## Did` yet.
        std::fs::write(&abs, "## Checklist\n\n- [ ] Vitamins\n").unwrap();

        append_did_bullet(&abs, "- Called the dentist").expect("append");

        let body = std::fs::read_to_string(&abs).unwrap();
        assert!(body.contains("## Did"), "heading inserted");
        assert!(body.contains("- Called the dentist"));
        // The trailing newline survives the append.
        assert!(body.ends_with('\n'));

        std::fs::remove_dir_all(root).ok();
    }

    /// `append_did_bullet` appends at the END of an existing `## Did`
    /// section (after any earlier bullets), not the end of the file.
    #[test]
    fn append_lands_at_end_of_did_not_file() {
        let root = tempdir_for_test();
        let date = NaiveDate::from_ymd_opt(2026, 5, 22).unwrap();
        let abs = root.join(daily_note_relative_path(date));
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(
            &abs,
            "## Checklist\n\n- [ ] x\n\n## Did\n\n- earlier event\n\n## Notes\n\n- reflection\n",
        )
        .unwrap();

        append_did_bullet(&abs, "- new event").expect("append");

        let body = std::fs::read_to_string(&abs).unwrap();
        let earlier = body.find("- earlier event").unwrap();
        let newer = body.find("- new event").unwrap();
        let notes = body.find("## Notes").unwrap();
        let reflection = body.find("- reflection").unwrap();
        assert!(
            earlier < newer && newer < notes && notes < reflection,
            "new event lands under ## Did, before ## Notes"
        );

        std::fs::remove_dir_all(root).ok();
    }

    /// `remove_did_bullet` deletes an exact match cleanly and is a no-op
    /// (returns `EditedSinceAppended`) when the user has changed the text.
    #[test]
    fn remove_bullet_is_exact_match_or_refuses() {
        let root = tempdir_for_test();
        let date = NaiveDate::from_ymd_opt(2026, 5, 22).unwrap();
        let abs = ensure_daily_note(&root, date).expect("ensure");
        append_did_bullet(&abs, "- Called the dentist").unwrap();

        // Exact match — removed.
        let outcome = remove_did_bullet(&abs, "- Called the dentist").expect("remove");
        assert_eq!(outcome, RemoveResult::Removed);
        let body = std::fs::read_to_string(&abs).unwrap();
        assert!(!body.contains("- Called the dentist"), "the bullet is gone");

        // Now log a bullet, mutate it, attempt to remove the original — refused.
        append_did_bullet(&abs, "- Watched a youtube video").unwrap();
        let body = std::fs::read_to_string(&abs).unwrap();
        let edited = body.replace(
            "- Watched a youtube video",
            "- Watched a youtube video about Rust",
        );
        std::fs::write(&abs, &edited).unwrap();

        let outcome = remove_did_bullet(&abs, "- Watched a youtube video").expect("remove");
        assert_eq!(
            outcome,
            RemoveResult::EditedSinceAppended,
            "the edited bullet is left alone"
        );
        let body = std::fs::read_to_string(&abs).unwrap();
        assert!(
            body.contains("- Watched a youtube video about Rust"),
            "the user's edit is preserved"
        );

        std::fs::remove_dir_all(root).ok();
    }

    /// `remove_did_bullet` reports `FileMissing` when the daily note has
    /// been deleted — an undo on a removed file is a no-op, not an error.
    #[test]
    fn remove_returns_file_missing_for_absent_note() {
        let root = tempdir_for_test();
        let date = NaiveDate::from_ymd_opt(2026, 5, 22).unwrap();
        let abs = root.join(daily_note_relative_path(date));
        let outcome = remove_did_bullet(&abs, "- whatever").expect("remove");
        assert_eq!(outcome, RemoveResult::FileMissing);

        std::fs::remove_dir_all(root).ok();
    }

    /// A bullet that lives outside `## Did` (e.g. in `## Notes`) is NOT
    /// removed — section scoping is part of the safety guarantee.
    #[test]
    fn remove_only_acts_inside_did_section() {
        let root = tempdir_for_test();
        let date = NaiveDate::from_ymd_opt(2026, 5, 22).unwrap();
        let abs = root.join(daily_note_relative_path(date));
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        // The same text exists in ## Notes only — not in ## Did.
        std::fs::write(
            &abs,
            "## Checklist\n\n- [ ] x\n\n## Did\n\n## Notes\n\n- Called the dentist\n",
        )
        .unwrap();

        let outcome = remove_did_bullet(&abs, "- Called the dentist").expect("remove");
        assert_eq!(
            outcome,
            RemoveResult::EditedSinceAppended,
            "a match outside ## Did is treated as not-found"
        );
        let body = std::fs::read_to_string(&abs).unwrap();
        assert!(
            body.contains("- Called the dentist"),
            "the bullet in ## Notes survives"
        );

        std::fs::remove_dir_all(root).ok();
    }
}
