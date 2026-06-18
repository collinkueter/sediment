//! Meeting notes — `Meetings/<YYYY-MM-DD HHmm> — <title>.md` (ADR-0016 §5,
//! plan M1).
//!
//! A **Meeting note** is the Note of a `meeting` Entity: an event-shaped Note
//! (like a Daily note — the transcript is not Facts; Facts are what the Agent
//! distils from it afterward) capturing one **Session**. It has five Sections:
//! `## Attendees`, `## Notes` (the user's typed notes + live chat, time-anchored),
//! `## Transcript` (speaker-labelled **Transcript segments**), `## Action items`,
//! `## Decisions`.
//!
//! This is the **pure helper layer** the Session commands share: materialise the
//! note, splice a transcript segment or a note line into the right Section, and
//! keep `## Attendees` deduplicated. It mirrors `daily_note.rs` deliberately —
//! same `atomic_write`, same `find_section` / `is_heading` / `finalize` idiom —
//! so parsing stays uniform across the codebase. The section helpers are kept
//! local (not shared with `daily_note`) so the two modules stay independent, the
//! same choice ADR-0010's module made.
//!
//! M1 scope: the writer and its commands feed *fake* segments to validate the
//! spine without audio. M2+ replaces the fake source with real capture; nothing
//! in this module changes.

use crate::core::formation_state::atomic_write;
use crate::error::{AppError, AppResult};
use chrono::{DateTime, Local};
use std::path::{Path, PathBuf};

/// Folder under the formation root holding `Meetings/*.md`. Plural matches
/// `People/`, `Daily Notes/`, … (ADR-0016 §5; ADR-0010 decision 1).
pub const MEETINGS_DIR: &str = "Meetings";

pub const ATTENDEES_HEADING: &str = "## Attendees";
pub const NOTES_HEADING: &str = "## Notes";
pub const TRANSCRIPT_HEADING: &str = "## Transcript";
pub const ACTION_ITEMS_HEADING: &str = "## Action items";
pub const DECISIONS_HEADING: &str = "## Decisions";

// ──────────────────────────────────────────────────────────────────────────
// Paths
// ──────────────────────────────────────────────────────────────────────────

/// `Meetings/<YYYY-MM-DD HHmm> — <title>.md` for a Session started at `started`,
/// as a formation-relative POSIX path. The string lands in the audit log and the
/// session registry, so it must round-trip across platforms — forward slash.
pub fn meeting_note_relative_path(started: DateTime<Local>, title: &str) -> String {
    let stamp = started.format("%Y-%m-%d %H%M");
    format!("{MEETINGS_DIR}/{stamp} — {}.md", sanitize_title(title))
}

/// Make a title safe to use as a filename component: drop path separators and
/// control characters, collapse whitespace, and fall back to "Meeting" when the
/// result is empty. Keeps spaces and case — these are real Obsidian filenames.
pub fn sanitize_title(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        "Meeting".to_string()
    } else {
        collapsed
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Create
// ──────────────────────────────────────────────────────────────────────────

/// Create the Meeting note for a Session if it does not exist, returning its
/// absolute path either way. Idempotent — a second call for the same path is a
/// no-op (the file exists, so it is not rewritten).
pub fn ensure_meeting_note(
    formation_root: &Path,
    relative_path: &str,
    title: &str,
    started: DateTime<Local>,
) -> AppResult<PathBuf> {
    let abs = formation_root.join(relative_path);
    if abs.is_file() {
        return Ok(abs);
    }
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent)?;
    }
    atomic_write(&abs, render_initial_meeting_note(title, started).as_bytes())?;
    Ok(abs)
}

/// The empty section frame for a fresh Meeting note. Empty headings are
/// deliberate — later appends (segments, notes, attendees) find their Section
/// without having to insert a heading first.
fn render_initial_meeting_note(title: &str, started: DateTime<Local>) -> String {
    let title = sanitize_title(title);
    let when = started.format("%Y-%m-%d %H:%M");
    format!(
        "# {title}\n\n\
         > Recorded {when} · Sediment Session\n\n\
         {ATTENDEES_HEADING}\n\n\
         {NOTES_HEADING}\n\n\
         {TRANSCRIPT_HEADING}\n\n\
         {ACTION_ITEMS_HEADING}\n\n\
         {DECISIONS_HEADING}\n"
    )
}

// ──────────────────────────────────────────────────────────────────────────
// Appends
// ──────────────────────────────────────────────────────────────────────────

/// Format an audio offset (ms from Session start) as `mm:ss` (or `h:mm:ss` past
/// an hour) for the timestamp prefix on segments and note lines — the §8 spine
/// that time-aligns notes to what was said.
pub fn format_offset(offset_ms: i64) -> String {
    let total = (offset_ms.max(0) / 1000) as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// Append one **Transcript segment** to `## Transcript`:
/// `` - `[mm:ss]` **Speaker:** text ``. Transcript lines are never deduplicated —
/// a speaker really can repeat themselves.
pub fn append_transcript_segment(
    note_abs: &Path,
    offset_ms: i64,
    speaker: &str,
    text: &str,
) -> AppResult<()> {
    let bullet = format!(
        "- `[{}]` **{}:** {}",
        format_offset(offset_ms),
        speaker.trim(),
        text.trim()
    );
    append_bullet(note_abs, TRANSCRIPT_HEADING, &bullet, false)
}

/// Append a time-anchored line to `## Notes` (the user's typed note or live
/// chat): `` - `[mm:ss]` text ``.
pub fn append_note_line(note_abs: &Path, offset_ms: i64, text: &str) -> AppResult<()> {
    let bullet = format!("- `[{}]` {}", format_offset(offset_ms), text.trim());
    append_bullet(note_abs, NOTES_HEADING, &bullet, false)
}

/// Ensure `name` appears in `## Attendees` as a wiki-link bullet `- [[Name]]`.
/// Idempotent: a no-op if the attendee is already listed. Use [`attendee_present`]
/// first when you need to know whether this was a genuinely new attendee.
pub fn ensure_attendee(note_abs: &Path, name: &str) -> AppResult<()> {
    let bullet = format!("- [[{}]]", name.trim());
    append_bullet(note_abs, ATTENDEES_HEADING, &bullet, true)
}

/// Whether `name` is already listed in `## Attendees`. Lets the Session command
/// decide novelty before calling [`ensure_attendee`], so an `attendeeChanged`
/// event fires only for genuinely new attendees.
pub fn attendee_present(note_abs: &Path, name: &str) -> AppResult<bool> {
    let content = read(note_abs)?;
    let bullet = format!("- [[{}]]", name.trim());
    let lines: Vec<&str> = content.lines().collect();
    Ok(match find_section(&lines, ATTENDEES_HEADING) {
        Some((h, end)) => lines[h + 1..end].iter().any(|l| l.trim_end() == bullet),
        None => false,
    })
}

// ──────────────────────────────────────────────────────────────────────────
// Section splice (pure, testable) — parameterised by heading
// ──────────────────────────────────────────────────────────────────────────

fn read(note_abs: &Path) -> AppResult<String> {
    std::fs::read_to_string(note_abs)
        .map_err(|e| AppError::other(format!("read meeting note {}: {e}", note_abs.display())))
}

/// Append `bullet` to the `heading` section of `note_abs`, creating the heading
/// at end-of-file if it is missing. When `dedupe`, an identical line already in
/// the section makes the append a no-op. Writes atomically.
fn append_bullet(note_abs: &Path, heading: &str, bullet: &str, dedupe: bool) -> AppResult<()> {
    let content = read(note_abs)?;
    match splice_append(&content, heading, bullet, dedupe) {
        None => Ok(()), // dedupe hit — nothing to write
        Some(updated) => atomic_write(note_abs, updated.as_bytes()),
    }
}

/// Returns `None` when `dedupe` and the line is already present; otherwise the
/// new file contents.
fn splice_append(content: &str, heading: &str, bullet: &str, dedupe: bool) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    match find_section(&lines, heading) {
        Some((heading_idx, section_end)) => {
            let in_section = &lines[heading_idx + 1..section_end];
            if dedupe && in_section.iter().any(|l| l.trim_end() == bullet) {
                return None;
            }
            // Trim trailing blank lines in the section so the new bullet hugs the
            // last existing one; the blank separator before the next section is
            // restored when we join it back on.
            let mut body: Vec<&str> = in_section.to_vec();
            while matches!(body.last(), Some(s) if s.trim().is_empty()) {
                body.pop();
            }
            let mut out: Vec<String> = Vec::with_capacity(lines.len() + 3);
            out.extend(lines[..=heading_idx].iter().map(|s| s.to_string()));
            out.push(String::new());
            out.extend(body.iter().map(|s| s.to_string()));
            out.push(bullet.to_string());
            if section_end < lines.len() {
                out.push(String::new());
                out.extend(lines[section_end..].iter().map(|s| s.to_string()));
            }
            Some(finalize(&out, content))
        }
        None => {
            let mut out: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
            if !out.last().map(|l| l.trim().is_empty()).unwrap_or(true) {
                out.push(String::new());
            }
            out.push(heading.to_string());
            out.push(String::new());
            out.push(bullet.to_string());
            Some(finalize(&out, content))
        }
    }
}

fn finalize(lines: &[String], original: &str) -> String {
    let joined = lines.join("\n");
    if original.ends_with('\n') && !joined.ends_with('\n') {
        format!("{joined}\n")
    } else {
        joined
    }
}

/// `(heading_index, next_section_or_eof_index)`; body is `lines[h+1 .. next]`.
fn find_section(lines: &[&str], heading: &str) -> Option<(usize, usize)> {
    let h = lines.iter().position(|l| l.trim_end() == heading)?;
    let next = lines[h + 1..]
        .iter()
        .position(|l| is_heading(l))
        .map(|rel| h + 1 + rel)
        .unwrap_or(lines.len());
    Some((h, next))
}

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
    use chrono::TimeZone;

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir()
            .join("sediment-test-meeting-note")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn started() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 6, 18, 15, 30, 0).unwrap()
    }

    #[test]
    fn relative_path_is_posix_and_readable() {
        let p = meeting_note_relative_path(started(), "Q3 Planning");
        assert_eq!(p, "Meetings/2026-06-18 1530 — Q3 Planning.md");
    }

    #[test]
    fn title_is_sanitized_and_falls_back() {
        assert_eq!(sanitize_title("a/b:c"), "a b c");
        assert_eq!(sanitize_title("   "), "Meeting");
        assert_eq!(sanitize_title("Weekly  sync\n"), "Weekly sync");
    }

    #[test]
    fn format_offset_minutes_and_hours() {
        assert_eq!(format_offset(0), "00:00");
        assert_eq!(format_offset(65_000), "01:05");
        assert_eq!(format_offset(3_725_000), "1:02:05");
        assert_eq!(format_offset(-5), "00:00");
    }

    #[test]
    fn ensure_creates_frame_and_is_idempotent() {
        let root = tempdir();
        let rel = meeting_note_relative_path(started(), "Q3 Planning");
        let abs = ensure_meeting_note(&root, &rel, "Q3 Planning", started()).unwrap();
        let body = std::fs::read_to_string(&abs).unwrap();
        assert!(body.starts_with("# Q3 Planning"));
        for h in [
            ATTENDEES_HEADING,
            NOTES_HEADING,
            TRANSCRIPT_HEADING,
            ACTION_ITEMS_HEADING,
            DECISIONS_HEADING,
        ] {
            assert!(body.contains(h), "missing {h}");
        }
        // Second call does not regenerate.
        std::fs::write(&abs, "# edited\n").unwrap();
        let abs2 = ensure_meeting_note(&root, &rel, "Q3 Planning", started()).unwrap();
        assert_eq!(abs, abs2);
        assert_eq!(std::fs::read_to_string(&abs2).unwrap(), "# edited\n");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn segments_and_notes_land_in_their_sections_in_order() {
        let root = tempdir();
        let rel = meeting_note_relative_path(started(), "Sync");
        let abs = ensure_meeting_note(&root, &rel, "Sync", started()).unwrap();

        append_transcript_segment(&abs, 5_000, "Sarah", "Let's start with Q3.").unwrap();
        append_note_line(&abs, 7_000, "follow up on the vendor").unwrap();
        append_transcript_segment(&abs, 12_000, "Self", "Sounds good.").unwrap();

        let body = std::fs::read_to_string(&abs).unwrap();
        let notes = body.find(NOTES_HEADING).unwrap();
        let transcript = body.find(TRANSCRIPT_HEADING).unwrap();
        let actions = body.find(ACTION_ITEMS_HEADING).unwrap();

        // Note line sits in ## Notes (before ## Transcript).
        let note_pos = body.find("- `[00:07]` follow up on the vendor").unwrap();
        assert!(notes < note_pos && note_pos < transcript);

        // Both segments sit in ## Transcript (after the heading, before ## Action items).
        let seg1 = body.find("**Sarah:** Let's start with Q3.").unwrap();
        let seg2 = body.find("**Self:** Sounds good.").unwrap();
        assert!(transcript < seg1 && seg1 < seg2 && seg2 < actions);
        assert!(body.contains("- `[00:05]` **Sarah:** Let's start with Q3."));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn attendees_dedupe_and_presence() {
        let root = tempdir();
        let rel = meeting_note_relative_path(started(), "Sync");
        let abs = ensure_meeting_note(&root, &rel, "Sync", started()).unwrap();

        assert!(!attendee_present(&abs, "Sarah Chen").unwrap());
        ensure_attendee(&abs, "Sarah Chen").unwrap();
        ensure_attendee(&abs, "Sarah Chen").unwrap(); // idempotent
        assert!(attendee_present(&abs, "Sarah Chen").unwrap());

        let body = std::fs::read_to_string(&abs).unwrap();
        assert_eq!(body.matches("- [[Sarah Chen]]").count(), 1);
        std::fs::remove_dir_all(root).ok();
    }
}
