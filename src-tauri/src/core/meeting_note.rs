//! Meeting notes — `Meetings/<YYYY-MM-DD HHmm> — <title>.md` (ADR-0017 §5,
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
/// `People/`, `Daily Notes/`, … (ADR-0017 §5; ADR-0010 decision 1).
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

/// The title component of a `Meetings/<stamp> — <title>.md` path: the text after
/// the ` — ` separator, sans extension and directory. Falls back to the whole
/// stem when the separator is absent. The inverse of [`meeting_note_relative_path`]
/// for the title half — used by the rename flow to learn the current title from
/// the filename without threading it through the frontend.
pub fn title_from_path(relative_path: &str) -> String {
    let file = relative_path
        .rsplit_once('/')
        .map(|(_, f)| f)
        .unwrap_or(relative_path);
    let stem = file.strip_suffix(".md").unwrap_or(file);
    stem.split_once(" — ")
        .map(|(_, t)| t)
        .unwrap_or(stem)
        .to_string()
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

/// The attendees currently listed in `## Attendees`, as plain names (the `[[…]]`
/// unwrapped). Derived from the note so it is the single source of truth — the
/// `attendeeChanged` event and the stop summary both read it.
pub fn list_attendees(note_abs: &Path) -> AppResult<Vec<String>> {
    let content = read(note_abs)?;
    let lines: Vec<&str> = content.lines().collect();
    let Some((h, end)) = find_section(&lines, ATTENDEES_HEADING) else {
        return Ok(Vec::new());
    };
    Ok(lines[h + 1..end]
        .iter()
        .filter_map(|l| {
            let t = l.trim();
            let inner = t.strip_prefix("- [[")?.strip_suffix("]]")?;
            Some(inner.to_string())
        })
        .collect())
}

/// How many transcript segments the note holds — bullets in `## Transcript`.
/// The Session stop summary reports this (derived, not tracked in memory).
pub fn count_transcript_segments(note_abs: &Path) -> AppResult<usize> {
    let content = read(note_abs)?;
    let lines: Vec<&str> = content.lines().collect();
    let Some((h, end)) = find_section(&lines, TRANSCRIPT_HEADING) else {
        return Ok(0);
    };
    Ok(lines[h + 1..end]
        .iter()
        .filter(|l| l.trim_start().starts_with("- `["))
        .count())
}

/// The most recent `## Transcript` bullets, oldest-first within `budget` bytes,
/// rendered as a grounding block for a live in-meeting chat turn (ADR-0017 §7).
/// Takes from the end (newest) so a long meeting still grounds the chat on *what
/// was just said*. `None` when there is no transcript yet.
pub fn recent_transcript_grounding(note_abs: &Path, budget: usize) -> AppResult<Option<String>> {
    let content = read(note_abs)?;
    let lines: Vec<&str> = content.lines().collect();
    let Some((h, end)) = find_section(&lines, TRANSCRIPT_HEADING) else {
        return Ok(None);
    };
    let bullets: Vec<&str> = lines[h + 1..end]
        .iter()
        .copied()
        .filter(|l| l.trim_start().starts_with("- `["))
        .collect();
    if bullets.is_empty() {
        return Ok(None);
    }
    // Walk from the newest bullet back until the budget is spent; keep at least one.
    let mut chosen: Vec<&str> = Vec::new();
    let mut total = 0usize;
    for b in bullets.iter().rev() {
        let add = b.len() + 1; // + newline
        if total + add > budget && !chosen.is_empty() {
            break;
        }
        chosen.push(b);
        total += add;
    }
    chosen.reverse(); // back to oldest-first reading order
    Ok(Some(format!(
        "## Live meeting transcript (most recent)\n{}",
        chosen.join("\n")
    )))
}

/// Default per-window character budget for distillation (ADR-0017 §7). Sized to
/// stay well inside the agent's `INJECTED_CONTEXT_BUDGET` once the meeting note,
/// prompt, and per-window grounding are added.
// M6 distillation scaffolding (ADR-0017 §7): tested; used when the end-of-session
// distillation turn is wired. Unused in the default lib build.
#[allow(dead_code)]
pub const DISTILL_WINDOW_BUDGET: usize = 4000;

/// Group the `## Transcript` bullets into windows each at most `budget` chars,
/// **never splitting a segment line**. Feeds the segment-windowed distillation
/// turn (ADR-0017 §7): the Agent processes one window at a time so a long meeting
/// never blows its context budget. Oldest-first; a single oversized segment
/// becomes its own window. Empty transcript → no windows.
#[allow(dead_code)]
pub fn transcript_windows(note_abs: &Path, budget: usize) -> AppResult<Vec<String>> {
    let content = read(note_abs)?;
    let lines: Vec<&str> = content.lines().collect();
    let Some((h, end)) = find_section(&lines, TRANSCRIPT_HEADING) else {
        return Ok(Vec::new());
    };
    let budget = budget.max(1);
    let mut windows = Vec::new();
    let mut cur = String::new();
    for bullet in lines[h + 1..end]
        .iter()
        .filter(|l| l.trim_start().starts_with("- `["))
    {
        // Flush before adding when the current window can't take this bullet.
        if !cur.is_empty() && cur.len() + bullet.len() + 1 > budget {
            windows.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push('\n');
        }
        cur.push_str(bullet);
        // A lone segment larger than the budget stands as its own window.
        if cur.len() >= budget {
            windows.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        windows.push(cur);
    }
    Ok(windows)
}

/// Rename a speaker throughout the Meeting note — the "that was Sarah" move
/// (ADR-0017 §6): rewrite `**<from>:**` labels in `## Transcript` and the
/// `[[<from>]]` bullet in `## Attendees` to `<to>`, deduping if `<to>` is already
/// an attendee. Returns the number of transcript segments relabelled. A no-op
/// (count 0, no write) when `<from>` does not appear. Identity correction is
/// suggest-not-assert: this is how a wrong/unknown label gets fixed by hand.
pub fn rename_speaker(note_abs: &Path, from: &str, to: &str) -> AppResult<usize> {
    let from = from.trim();
    let to = to.trim();
    if from.is_empty() || to.is_empty() || from == to {
        return Ok(0);
    }
    let content = read(note_abs)?;
    let lines: Vec<&str> = content.lines().collect();
    let transcript = find_section(&lines, TRANSCRIPT_HEADING);
    let attendees = find_section(&lines, ATTENDEES_HEADING);

    let from_tok = format!("**{from}:**");
    let to_tok = format!("**{to}:**");
    let from_link = format!("- [[{from}]]");
    let to_link = format!("- [[{to}]]");
    let to_already_attendee = attendees
        .map(|(h, e)| lines[h + 1..e].iter().any(|l| l.trim_end() == to_link))
        .unwrap_or(false);

    let in_range = |range: Option<(usize, usize)>, i: usize| {
        range.map(|(h, e)| i > h && i < e).unwrap_or(false)
    };

    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut renamed = 0usize;
    let mut changed = false;
    for (i, l) in lines.iter().enumerate() {
        if in_range(transcript, i) && l.contains(&from_tok) {
            out.push(l.replace(&from_tok, &to_tok));
            renamed += 1;
            changed = true;
        } else if in_range(attendees, i) && l.trim_end() == from_link {
            changed = true;
            if to_already_attendee {
                // Drop the duplicate attendee bullet.
            } else {
                out.push(to_link.clone());
            }
        } else {
            out.push((*l).to_string());
        }
    }

    if changed {
        atomic_write(note_abs, finalize(&out, &content).as_bytes())?;
    }
    Ok(renamed)
}

// ──────────────────────────────────────────────────────────────────────────
// Rename
// ──────────────────────────────────────────────────────────────────────────

/// Rename a finished Meeting note to `new_title` (ADR-0017 §7): rewrite its `# `
/// H1 and move the file to `Meetings/<same stamp> — <new title>.md`, keeping the
/// `<YYYY-MM-DD HHmm>` prefix so the chronological filename ordering survives the
/// rename. Returns the new formation-relative POSIX path. A no-op move when the
/// sanitized title resolves to the existing filename (only the H1 is touched).
/// Errors if the note is missing or a different note already occupies the target.
pub fn rename_meeting_note(
    formation_root: &Path,
    old_relative_path: &str,
    new_title: &str,
) -> AppResult<String> {
    let new_title = sanitize_title(new_title);
    let old_abs = formation_root.join(old_relative_path);
    if !old_abs.is_file() {
        return Err(AppError::other(format!(
            "meeting note not found: {old_relative_path}"
        )));
    }
    let new_relative_path = swap_title_in_path(old_relative_path, &new_title);
    let new_abs = formation_root.join(&new_relative_path);

    // Check for a collision BEFORE mutating anything, so a clash fails cleanly
    // rather than leaving the H1 rewritten but the file unmoved (a split-brain where
    // the in-file title no longer matches the filename).
    if new_abs != old_abs && new_abs.exists() {
        return Err(AppError::other(format!(
            "a meeting note already exists at {new_relative_path}"
        )));
    }

    // Rewrite the note's H1 to the new title (the in-file half of the rename).
    let content = read(&old_abs)?;
    atomic_write(&old_abs, rewrite_h1(&content, &new_title).as_bytes())?;

    // Move the file unless the path is unchanged (title sanitised to the same).
    if new_abs != old_abs {
        std::fs::rename(&old_abs, &new_abs)
            .map_err(|e| AppError::other(format!("rename meeting note: {e}")))?;
    }
    Ok(new_relative_path)
}

/// Replace the title component of a `Meetings/<stamp> — <title>.md` path, keeping
/// the directory and stamp prefix. Falls back to appending the title next to the
/// original stem when the expected ` — ` separator is absent.
fn swap_title_in_path(old_relative_path: &str, new_title: &str) -> String {
    let (dir, file) = match old_relative_path.rsplit_once('/') {
        Some((d, f)) => (Some(d), f),
        None => (None, old_relative_path),
    };
    let stem = file.strip_suffix(".md").unwrap_or(file);
    let stamp = stem.split_once(" — ").map(|(s, _)| s).unwrap_or(stem);
    let new_file = format!("{stamp} — {new_title}.md");
    match dir {
        Some(d) => format!("{d}/{new_file}"),
        None => new_file,
    }
}

/// Replace the first ATX `# ` heading with `# {title}`, leaving the rest of the
/// note untouched. Prepends a heading when the note has none.
fn rewrite_h1(content: &str, title: &str) -> String {
    let mut replaced = false;
    let out: Vec<String> = content
        .lines()
        .map(|line| {
            if !replaced && line.trim_start().starts_with("# ") {
                replaced = true;
                format!("# {title}")
            } else {
                line.to_string()
            }
        })
        .collect();
    if !replaced {
        return format!("# {title}\n\n{content}");
    }
    finalize(&out, content)
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
    fn recent_transcript_grounding_takes_newest_within_budget() {
        let root = tempdir();
        let rel = meeting_note_relative_path(started(), "Sync");
        let abs = ensure_meeting_note(&root, &rel, "Sync", started()).unwrap();

        // No transcript yet → None.
        assert!(recent_transcript_grounding(&abs, 2000).unwrap().is_none());

        for i in 0..5 {
            append_transcript_segment(&abs, i * 1000, "Sarah", &format!("line number {i}"))
                .unwrap();
        }
        let all = recent_transcript_grounding(&abs, 2000).unwrap().unwrap();
        assert!(all.starts_with("## Live meeting transcript"));
        assert!(all.contains("line number 0") && all.contains("line number 4"));

        // A tight budget keeps only the newest bullet(s), and always at least one.
        let tight = recent_transcript_grounding(&abs, 1).unwrap().unwrap();
        assert!(tight.contains("line number 4"), "newest kept: {tight}");
        assert!(!tight.contains("line number 0"), "oldest dropped: {tight}");
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

    #[test]
    fn transcript_windows_pack_by_budget_without_splitting_segments() {
        let root = tempdir();
        let rel = meeting_note_relative_path(started(), "Sync");
        let abs = ensure_meeting_note(&root, &rel, "Sync", started()).unwrap();
        for i in 0..6 {
            append_transcript_segment(&abs, i * 1000, "Sarah", &format!("line number {i}"))
                .unwrap();
        }

        // Huge budget → a single window holding every segment.
        let one = transcript_windows(&abs, 100_000).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].matches("- `[").count(), 6);

        // Small budget → multiple windows, each within budget, no segment split.
        let many = transcript_windows(&abs, 60).unwrap();
        assert!(
            many.len() > 1,
            "expected several windows, got {}",
            many.len()
        );
        let total: usize = many.iter().map(|w| w.matches("- `[").count()).sum();
        assert_eq!(total, 6, "every segment lands in exactly one window");
        assert!(
            many.iter()
                .all(|w| w.lines().all(|l| l.starts_with("- `["))),
            "windows contain only whole segment lines"
        );
    }

    #[test]
    fn transcript_windows_empty_when_no_transcript() {
        let root = tempdir();
        let rel = meeting_note_relative_path(started(), "Sync");
        let abs = ensure_meeting_note(&root, &rel, "Sync", started()).unwrap();
        assert!(transcript_windows(&abs, DISTILL_WINDOW_BUDGET)
            .unwrap()
            .is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rename_speaker_rewrites_transcript_and_dedupes_attendees() {
        let root = tempdir();
        let rel = meeting_note_relative_path(started(), "Sync");
        let abs = ensure_meeting_note(&root, &rel, "Sync", started()).unwrap();
        // Both already listed as attendees (Sarah will absorb the unknown).
        ensure_attendee(&abs, "Unknown speaker 2").unwrap();
        ensure_attendee(&abs, "Sarah Chen").unwrap();
        // Two segments from an unknown speaker, one from Sarah.
        append_transcript_segment(&abs, 1000, "Unknown speaker 2", "first").unwrap();
        append_transcript_segment(&abs, 2000, "Sarah Chen", "hi").unwrap();
        append_transcript_segment(&abs, 3000, "Unknown speaker 2", "second").unwrap();

        let n = rename_speaker(&abs, "Unknown speaker 2", "Sarah Chen").unwrap();
        assert_eq!(n, 2, "both unknown segments relabelled");

        let body = std::fs::read_to_string(&abs).unwrap();
        assert!(!body.contains("Unknown speaker 2"), "old label gone");
        assert_eq!(
            body.matches("**Sarah Chen:**").count(),
            3,
            "all attributed to Sarah"
        );
        // Attendees deduped to a single Sarah bullet (she was already listed).
        assert_eq!(body.matches("- [[Sarah Chen]]").count(), 1);
        assert!(!body.contains("[[Unknown speaker 2]]"));

        // Renaming a speaker that isn't present is a no-op.
        assert_eq!(rename_speaker(&abs, "Nobody", "X").unwrap(), 0);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn title_from_path_unwraps_the_stamp_separator() {
        assert_eq!(
            title_from_path("Meetings/2026-06-18 1530 — Q3 Planning.md"),
            "Q3 Planning"
        );
        // No separator → the whole stem.
        assert_eq!(title_from_path("Meetings/loose.md"), "loose");
    }

    #[test]
    fn rename_meeting_note_moves_file_keeps_stamp_and_rewrites_h1() {
        let root = tempdir();
        let rel = meeting_note_relative_path(started(), "Untitled");
        let abs = ensure_meeting_note(&root, &rel, "Untitled", started()).unwrap();
        append_transcript_segment(&abs, 1000, "Sarah", "Let's plan Q3.").unwrap();

        let new_rel = rename_meeting_note(&root, &rel, "Q3 Planning").unwrap();
        assert_eq!(new_rel, "Meetings/2026-06-18 1530 — Q3 Planning.md");
        assert!(!root.join(&rel).exists(), "old file moved");

        let body = std::fs::read_to_string(root.join(&new_rel)).unwrap();
        assert!(body.starts_with("# Q3 Planning"), "H1 rewritten: {body}");
        // The transcript (and everything below the H1) is carried over intact.
        assert!(body.contains("**Sarah:** Let's plan Q3."));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rename_meeting_note_errors_on_collision_and_missing() {
        let root = tempdir();
        let rel = meeting_note_relative_path(started(), "Sync");
        ensure_meeting_note(&root, &rel, "Sync", started()).unwrap();
        // A second note the rename would collide with.
        let taken = meeting_note_relative_path(started(), "Q3 Planning");
        ensure_meeting_note(&root, &taken, "Q3 Planning", started()).unwrap();

        assert!(rename_meeting_note(&root, &rel, "Q3 Planning").is_err());
        // Original is untouched after the failed rename.
        assert!(root.join(&rel).exists());
        // Renaming a note that does not exist errors too.
        assert!(rename_meeting_note(&root, "Meetings/ghost.md", "X").is_err());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rewrite_h1_replaces_only_the_first_heading() {
        let content = "# Old\n\n## Notes\n\n# not a real h1 inside\n";
        let out = rewrite_h1(content, "New");
        assert!(out.starts_with("# New\n"));
        assert_eq!(out.matches("# New").count(), 1);
        assert!(out.contains("# not a real h1 inside"), "later lines kept");
        // No H1 at all → one is prepended.
        assert!(rewrite_h1("## Notes\n", "New").starts_with("# New\n\n## Notes"));
    }
}
