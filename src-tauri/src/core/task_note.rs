//! The `## Tasks` managed region of `Tasks.md` (ADR-0007).
//!
//! Tasks live canonically as an Obsidian-Tasks-compatible markdown checklist.
//! Each line carries its identity inline, so — unlike the `## Facts` section —
//! no `chat-notes` frontmatter provenance block is needed:
//!
//! ```text
//! - [ ] Renew passport 📅 2026-06-01 🆔 renew_passport_a1b2c3
//! - [x] Call the dentist 📅 2026-05-21 ✅ 2026-05-20 🆔 call_dentist_d4e5f6
//! ```
//!
//! This module is pure markdown ↔ `ChecklistLine`. Everything outside the
//! `## Tasks` section — user prose, frontmatter — is preserved verbatim.

/// The managed section heading. A `Tasks.md` has at most one.
pub const TASKS_HEADING: &str = "## Tasks";

/// One parsed checklist line. Dates are date-granularity (`📅`/`✅` are
/// date-only in markdown); the `task` table widens them to datetimes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChecklistLine {
    pub done: bool,
    pub title: String,
    pub due: Option<chrono::NaiveDate>,
    pub completed: Option<chrono::NaiveDate>,
    /// The `🆔` token — a `task` record-id key (no `task:` prefix).
    pub id: Option<String>,
}

/// Render one checklist line in the Obsidian-Tasks field order:
/// `- [ ] title 📅 due ✅ completed 🆔 id`.
pub fn render_checklist_line(line: &ChecklistLine) -> String {
    let mark = if line.done { "x" } else { " " };
    let mut s = format!("- [{mark}] {}", line.title.trim());
    if let Some(due) = line.due {
        s.push_str(&format!(" 📅 {}", due.format("%Y-%m-%d")));
    }
    if let Some(completed) = line.completed {
        s.push_str(&format!(" ✅ {}", completed.format("%Y-%m-%d")));
    }
    if let Some(id) = &line.id {
        s.push_str(&format!(" 🆔 {id}"));
    }
    s
}

/// Parse one line into a `ChecklistLine`, or `None` if it is not a task line.
/// `📅`/`✅`/`🆔` are recognised anywhere after the checkbox; the remaining
/// words are the title.
pub fn parse_checklist_line(line: &str) -> Option<ChecklistLine> {
    let rest = line.trim_start().strip_prefix("- [")?;
    let mark = rest.chars().next()?;
    let done = match mark {
        ' ' => false,
        'x' | 'X' => true,
        _ => return None,
    };
    let body = rest
        .get(mark.len_utf8()..)
        .and_then(|r| r.strip_prefix("] "))?;

    let mut title_parts: Vec<&str> = Vec::new();
    let mut due = None;
    let mut completed = None;
    let mut id = None;
    let tokens: Vec<&str> = body.split_whitespace().collect();
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i] {
            "📅" => {
                due = tokens.get(i + 1).and_then(|v| parse_date(v));
                i += 2;
            }
            "✅" => {
                completed = tokens.get(i + 1).and_then(|v| parse_date(v));
                i += 2;
            }
            "🆔" => {
                id = tokens.get(i + 1).map(|v| (*v).to_string());
                i += 2;
            }
            word => {
                title_parts.push(word);
                i += 1;
            }
        }
    }
    Some(ChecklistLine {
        done,
        title: title_parts.join(" "),
        due,
        completed,
        id,
    })
}

fn parse_date(raw: &str) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok()
}

/// Every checklist line in the note's `## Tasks` section, in document order.
/// A note without the section yields an empty list.
pub fn parse_tasks_section(content: &str) -> Vec<ChecklistLine> {
    let (_, section, _) = split_tasks_section(content);
    section.lines().filter_map(parse_checklist_line).collect()
}

/// Reassemble `Tasks.md` with the `## Tasks` section replaced by exactly
/// `lines`, preserving any prose before and after it. `existing_content` is
/// the on-disk note (`None` for a brand-new `Tasks.md`).
pub fn render_tasks_note(existing_content: Option<&str>, lines: &[ChecklistLine]) -> String {
    let (before, _, after) = split_tasks_section(existing_content.unwrap_or(""));

    let mut section = String::from(TASKS_HEADING);
    section.push_str("\n\n");
    for line in lines {
        section.push_str(&render_checklist_line(line));
        section.push('\n');
    }

    let mut body = String::new();
    if !before.is_empty() {
        body.push_str(&before);
        body.push_str("\n\n");
    }
    body.push_str(&section);
    if !after.trim().is_empty() {
        body.push('\n');
        body.push_str(after.trim_start_matches('\n'));
    }
    format!("{}\n", body.trim_end_matches('\n'))
}

/// Split a note into `(prose_before, tasks_section_body, prose_after)` around
/// the single managed `## Tasks` section. Without the heading the whole note
/// is `prose_before`.
fn split_tasks_section(content: &str) -> (String, String, String) {
    let lines: Vec<&str> = content.lines().collect();
    let Some(h) = lines.iter().position(|l| l.trim_end() == TASKS_HEADING) else {
        return (
            content.trim_end_matches('\n').to_string(),
            String::new(),
            String::new(),
        );
    };
    let next = lines[h + 1..]
        .iter()
        .position(|l| is_heading(l))
        .map(|rel| h + 1 + rel);
    let before = lines[..h].join("\n");
    let (section, after): (&[&str], &[&str]) = match next {
        Some(n) => (&lines[h + 1..n], &lines[n..]),
        None => (&lines[h + 1..], &[]),
    };
    (
        before.trim_end_matches('\n').to_string(),
        section.join("\n").trim().to_string(),
        after.join("\n"),
    )
}

/// A markdown ATX heading line (`#`..`######` then a space).
fn is_heading(line: &str) -> bool {
    let t = line.trim_start();
    let hashes = t.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hashes) && t[hashes..].starts_with(' ')
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn renders_open_and_done_lines_in_field_order() {
        let open = ChecklistLine {
            done: false,
            title: "Renew passport".into(),
            due: Some(date(2026, 6, 1)),
            completed: None,
            id: Some("renew_passport_a1b2c3".into()),
        };
        assert_eq!(
            render_checklist_line(&open),
            "- [ ] Renew passport 📅 2026-06-01 🆔 renew_passport_a1b2c3"
        );

        let done = ChecklistLine {
            done: true,
            title: "Call the dentist".into(),
            due: Some(date(2026, 5, 21)),
            completed: Some(date(2026, 5, 20)),
            id: Some("call_dentist_d4e5f6".into()),
        };
        assert_eq!(
            render_checklist_line(&done),
            "- [x] Call the dentist 📅 2026-05-21 ✅ 2026-05-20 🆔 call_dentist_d4e5f6"
        );

        // A bare task with no fields still renders.
        let bare = ChecklistLine {
            done: false,
            title: "Think about it".into(),
            due: None,
            completed: None,
            id: None,
        };
        assert_eq!(render_checklist_line(&bare), "- [ ] Think about it");
    }

    #[test]
    fn parses_lines_and_round_trips() {
        for line in [
            ChecklistLine {
                done: false,
                title: "Renew passport".into(),
                due: Some(date(2026, 6, 1)),
                completed: None,
                id: Some("renew_passport_a1b2c3".into()),
            },
            ChecklistLine {
                done: true,
                title: "Call the dentist".into(),
                due: Some(date(2026, 5, 21)),
                completed: Some(date(2026, 5, 20)),
                id: Some("call_dentist_d4e5f6".into()),
            },
        ] {
            let rendered = render_checklist_line(&line);
            assert_eq!(parse_checklist_line(&rendered).as_ref(), Some(&line));
        }

        // A non-checklist line is not a task.
        assert!(parse_checklist_line("Just some prose.").is_none());
        assert!(parse_checklist_line("## Tasks").is_none());
        // A capital-X checkbox is accepted.
        assert!(parse_checklist_line("- [X] Done it").unwrap().done);
    }

    #[test]
    fn parse_tasks_section_extracts_only_section_lines() {
        let note = "Intro prose.\n\n## Tasks\n\n- [ ] First 🆔 a\n- [x] Second 🆔 b\n\n## Notes\n\n- [ ] Not a managed task\n";
        let lines = parse_tasks_section(note);
        assert_eq!(lines.len(), 2, "the line under ## Notes is out of section");
        assert_eq!(lines[0].title, "First");
        assert!(lines[1].done);
    }

    #[test]
    fn render_tasks_note_creates_section_and_preserves_prose() {
        let line = ChecklistLine {
            done: false,
            title: "Renew passport".into(),
            due: Some(date(2026, 6, 1)),
            completed: None,
            id: Some("renew_passport_a1b2c3".into()),
        };

        // A brand-new Tasks.md.
        let fresh = render_tasks_note(None, std::slice::from_ref(&line));
        assert!(fresh.starts_with("## Tasks\n\n- [ ] Renew passport"));
        assert!(fresh.ends_with('\n'));

        // An update preserves prose before and after the section, and replaces
        // the section body with exactly the supplied lines.
        let existing = "My to-dos.\n\n## Tasks\n\n- [ ] Stale 🆔 old\n\n## Notes\n\nkeep me\n";
        let updated = render_tasks_note(Some(existing), std::slice::from_ref(&line));
        assert!(updated.contains("My to-dos."));
        assert!(updated.contains("## Notes\n\nkeep me"));
        assert!(updated.contains("- [ ] Renew passport"));
        assert!(!updated.contains("Stale"), "the section body is replaced");
        let before = updated.find("My to-dos.").unwrap();
        let tasks = updated.find("## Tasks").unwrap();
        let notes = updated.find("## Notes").unwrap();
        assert!(before < tasks && tasks < notes, "regions stay in order");
    }
}
