//! Template-based markdown diff generation (Phase 3 decision #1).
//!
//! Each staged fact renders to a deterministic bullet under a managed
//! `## Facts` section. Per-fact provenance (`fact-id → source-chat`) lives in
//! a `chat-notes` YAML frontmatter block. User prose outside the managed
//! section — and any user frontmatter keys outside the `chat-notes` block —
//! is never touched. The LLM-polished natural-prose merge is a post-V1
//! enhancement (see the Phase 3 plan).

use crate::core::memory::slugify;
use crate::core::staging::{ChangeKind, NoteChange, StagedFact};

/// The managed section heading. A note has at most one.
const FACTS_HEADING: &str = "## Facts";

/// Human-readable verb phrase for a predicate. Anything outside the table
/// falls back to a humanised form of the raw predicate.
pub fn predicate_phrasing(predicate: &str) -> String {
    let phrase = match predicate {
        "works_at" => "Works at",
        "joined" => "Joined",
        "left" => "Left",
        "founded" => "Founded",
        "invests_in" => "Invests in",
        "advises" => "Advises",
        "member_of" => "Member of",
        "former_member_of" => "Former member of",
        "volunteered_at" => "Volunteered at",
        "knows" => "Knows",
        "reports_to" => "Reports to",
        "manages" => "Manages",
        "parent_of" => "Parent of",
        "child_of" => "Child of",
        "sibling_of" => "Sibling of",
        "partner_of" => "Partner of",
        "mentored_by" => "Mentored by",
        "collaborates_with" => "Collaborates with",
        "lives_in" => "Lives in",
        "born_in" => "Born in",
        "visited" => "Visited",
        "expert_in" => "Expert in",
        "interested_in" => "Interested in",
        "advocates_for" => "Advocates for",
        "attended" => "Attended",
        "organized" => "Organized",
        "presented_at" => "Presented at",
        "created" => "Created",
        "contributes_to" => "Contributes to",
        "leads" => "Leads",
        "owns_task" => "Owns task",
        "completed" => "Completed",
        "delegated_to" => "Delegated to",
        "subsidiary_of" => "Subsidiary of",
        "competitor_of" => "Competitor of",
        "acquired_by" => "Acquired by",
        "partner_with" => "Partner with",
        "located_in" => "Located in",
        "headquartered_in" => "Headquartered in",
        "scheduled_for" => "Scheduled for",
        "about" => "About",
        "due_on" => "Due on",
        "blocks" => "Blocks",
        "depends_on" => "Depends on",
        other => return humanize(other),
    };
    phrase.to_string()
}

/// Past-tense verb phrase for a predicate, used when a fact has a closed
/// validity interval (a `valid_to`). Predicates that already read as past
/// tense — or have no distinct past form worth special-casing — fall back to
/// the present-tense phrasing.
pub fn predicate_phrasing_past(predicate: &str) -> String {
    let phrase = match predicate {
        "works_at" => "Worked at",
        "member_of" => "Was a member of",
        "leads" => "Led",
        "manages" => "Managed",
        "advises" => "Advised",
        "reports_to" => "Reported to",
        "owns_task" => "Owned task",
        "advocates_for" => "Advocated for",
        "lives_in" => "Lived in",
        "contributes_to" => "Contributed to",
        "invests_in" => "Invested in",
        "expert_in" => "Was an expert in",
        "interested_in" => "Was interested in",
        _ => return predicate_phrasing(predicate),
    };
    phrase.to_string()
}

/// `snake_case` → `Sentence case` fallback for unknown predicates.
fn humanize(predicate: &str) -> String {
    let spaced = predicate.replace('_', " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Stable id for a fact within a note: `<predicate>:<object-slug>`. Two facts
/// with the same predicate + object collapse to one bullet (idempotence).
pub fn fact_id(fact: &StagedFact) -> String {
    format!("{}:{}", fact.predicate, slugify(&fact.object_name))
}

/// Render one fact as a markdown bullet, e.g. `- Founded Microsoft`. A fact
/// with a closed validity interval (`valid_to` set) renders past-tense; one
/// with an explicitly-extracted `valid_from` has its year appended.
pub fn render_fact_bullet(fact: &StagedFact) -> String {
    let phrasing = if fact.valid_to.is_some() {
        predicate_phrasing_past(&fact.predicate)
    } else {
        predicate_phrasing(&fact.predicate)
    };
    let base = format!("- {} {}", phrasing, fact.object_name.trim());
    if fact.valid_from_explicit {
        format!("{base} ({})", fact.valid_from.format("%Y"))
    } else {
        base
    }
}

/// Apply `facts` to a note, producing the `NoteChange` the staging tray shows
/// and a Keep commits. `existing_content` is the on-disk note (`None` for a
/// brand-new note). Facts whose id already appears in the note's `chat-notes`
/// block are skipped — re-applying the same fact is idempotent.
pub fn apply_facts_to_note(
    note_path: &str,
    existing_content: Option<&str>,
    facts: &[StagedFact],
    source_chat_id: &str,
) -> NoteChange {
    let original = existing_content.unwrap_or("");
    let kind = if existing_content.is_some() {
        ChangeKind::Update
    } else {
        ChangeKind::Create
    };
    let parsed = parse_note(original);

    let mut provenance = parsed.provenance.clone();
    let mut added: Vec<StagedFact> = Vec::new();
    let mut new_bullets: Vec<String> = Vec::new();
    for fact in facts {
        let id = fact_id(fact);
        if provenance.iter().any(|(existing, _)| existing == &id) {
            continue; // already filed (or a duplicate within this batch)
        }
        new_bullets.push(render_fact_bullet(fact));
        provenance.push((id, source_chat_id.to_string()));
        added.push(fact.clone());
    }

    if added.is_empty() {
        // Every fact was already present — a no-op the caller can drop.
        return NoteChange {
            kind,
            note_path: note_path.to_string(),
            diff: String::new(),
            new_content: original.to_string(),
            facts: Vec::new(),
            confidence: 1.0,
            conflicts: Vec::new(),
        };
    }

    let new_content = render_note(&parsed, &provenance, &new_bullets);
    let diff = additive_diff(original, &new_content);
    let confidence = added
        .iter()
        .map(|f| f.confidence)
        .fold(f64::INFINITY, f64::min);

    NoteChange {
        kind,
        note_path: note_path.to_string(),
        diff,
        new_content,
        facts: added,
        confidence,
        conflicts: Vec::new(),
    }
}

/// A note decomposed into its managed and unmanaged regions.
struct ParsedNote {
    /// User frontmatter keys, `---` fences and the `chat-notes` block removed.
    /// Trailing newline normalised off; empty when the note had none.
    user_frontmatter: String,
    /// `(fact-id, source-chat)` pairs from the `chat-notes` block, in order.
    provenance: Vec<(String, String)>,
    /// Body text before the `## Facts` heading (or the whole body if absent),
    /// trailing newlines trimmed.
    prose_before: String,
    /// Existing content of the `## Facts` section (heading excluded), trimmed.
    facts_body: String,
    /// Body from the heading after the `## Facts` section onward; empty if none.
    prose_after: String,
}

fn parse_note(content: &str) -> ParsedNote {
    let (frontmatter, body) = split_frontmatter(content);
    let (user_frontmatter, provenance) = match frontmatter {
        Some(fm) => split_chat_notes_block(fm),
        None => (String::new(), Vec::new()),
    };
    let (prose_before, facts_body, prose_after) = split_facts_section(body);
    ParsedNote {
        user_frontmatter,
        provenance,
        prose_before,
        facts_body,
        prose_after,
    }
}

/// Split a leading `---` ... `---` frontmatter block off the content. Returns
/// `(frontmatter_body, rest)`. A note without a well-formed block yields
/// `(None, whole_content)`.
fn split_frontmatter(content: &str) -> (Option<&str>, &str) {
    let Some(rest) = content.strip_prefix("---\n") else {
        return (None, content);
    };
    if let Some(end) = rest.find("\n---\n") {
        (Some(&rest[..end]), &rest[end + "\n---\n".len()..])
    } else if let Some(fm) = rest.strip_suffix("\n---") {
        (Some(fm), "")
    } else if let Some(fm) = rest.strip_suffix("\n---\n") {
        (Some(fm), "")
    } else {
        (None, content)
    }
}

/// Separate user frontmatter keys from the managed `chat-notes` block. The
/// block, when present, is always the trailing region (we always write it
/// last); everything before a column-0 `chat-notes:` line is the user's.
fn split_chat_notes_block(frontmatter: &str) -> (String, Vec<(String, String)>) {
    let lines: Vec<&str> = frontmatter.lines().collect();
    let block_start = lines.iter().position(|l| l.trim_end() == "chat-notes:");
    let Some(start) = block_start else {
        return (normalise_block(&lines), Vec::new());
    };
    let user = normalise_block(&lines[..start]);
    let provenance = parse_provenance(&lines[start + 1..]);
    (user, provenance)
}

/// Join frontmatter lines back into a string with a single trailing newline
/// (or empty when there are none).
fn normalise_block(lines: &[&str]) -> String {
    let joined = lines.join("\n");
    if joined.trim().is_empty() {
        String::new()
    } else {
        format!("{joined}\n")
    }
}

/// Parse `    "fact-id": "source-chat"` lines under the `chat-notes` block's
/// `facts:` key. Unparseable lines are skipped.
fn parse_provenance(block_lines: &[&str]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut in_facts = false;
    for line in block_lines {
        let trimmed = line.trim();
        if trimmed == "facts:" {
            in_facts = true;
            continue;
        }
        if !in_facts {
            continue;
        }
        if let Some(pair) = parse_quoted_pair(line) {
            out.push(pair);
        }
    }
    out
}

/// Parse a `"key": "value"` line (leading indentation ignored).
fn parse_quoted_pair(line: &str) -> Option<(String, String)> {
    let rest = line.trim_start().strip_prefix('"')?;
    let (key, rest) = take_quoted(rest)?;
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let (val, _) = take_quoted(rest)?;
    Some((key, val))
}

/// Read a string up to the next unescaped `"`. Returns the content (with `\"`
/// and `\\` unescaped) and the remainder after the closing quote.
fn take_quoted(s: &str) -> Option<(String, &str)> {
    let mut content = String::new();
    let mut chars = s.char_indices();
    while let Some((idx, c)) = chars.next() {
        match c {
            '\\' => {
                if let Some((_, next)) = chars.next() {
                    content.push(next);
                }
            }
            '"' => return Some((content, &s[idx + 1..])),
            other => content.push(other),
        }
    }
    None
}

/// Split a body into `(before, facts-section-content, after)` around the
/// single managed `## Facts` section.
fn split_facts_section(body: &str) -> (String, String, String) {
    let lines: Vec<&str> = body.lines().collect();
    let heading = lines.iter().position(|l| l.trim_end() == FACTS_HEADING);
    let Some(h) = heading else {
        return (
            body.trim_end_matches('\n').to_string(),
            String::new(),
            String::new(),
        );
    };
    // The section runs to the next markdown heading, or end of body.
    let next = lines[h + 1..]
        .iter()
        .position(|l| is_heading(l))
        .map(|rel| h + 1 + rel);
    let before = lines[..h].join("\n");
    let (section_lines, after_lines): (&[&str], &[&str]) = match next {
        Some(n) => (&lines[h + 1..n], &lines[n..]),
        None => (&lines[h + 1..], &[]),
    };
    (
        before.trim_end_matches('\n').to_string(),
        section_lines.join("\n").trim().to_string(),
        after_lines.join("\n"),
    )
}

/// A markdown ATX heading line (`#` ... `######` followed by a space).
fn is_heading(line: &str) -> bool {
    let t = line.trim_start();
    let hashes = t.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hashes) && t[hashes..].starts_with(' ')
}

/// Reassemble a note from its parsed regions, the merged provenance map, and
/// the bullets to append to the `## Facts` section.
fn render_note(
    parsed: &ParsedNote,
    provenance: &[(String, String)],
    new_bullets: &[String],
) -> String {
    let section = render_facts_section(&parsed.facts_body, new_bullets);

    let mut body = String::new();
    if !parsed.prose_before.is_empty() {
        body.push_str(&parsed.prose_before);
        body.push_str("\n\n");
    }
    body.push_str(&section);
    if !parsed.prose_after.trim().is_empty() {
        body.push('\n');
        body.push_str(parsed.prose_after.trim_start_matches('\n'));
    }

    let mut content = String::new();
    if !parsed.user_frontmatter.trim().is_empty() || !provenance.is_empty() {
        content.push_str("---\n");
        if !parsed.user_frontmatter.trim().is_empty() {
            content.push_str(parsed.user_frontmatter.trim_end_matches('\n'));
            content.push('\n');
        }
        content.push_str(&render_chat_notes_block(provenance));
        content.push_str("---\n\n");
    }
    content.push_str(&body);

    format!("{}\n", content.trim_end_matches('\n'))
}

/// Render the `## Facts` section: existing content followed by the new bullets.
fn render_facts_section(facts_body: &str, new_bullets: &[String]) -> String {
    let mut s = String::from(FACTS_HEADING);
    s.push_str("\n\n");
    let existing = facts_body.trim();
    if !existing.is_empty() {
        s.push_str(existing);
        if !new_bullets.is_empty() {
            s.push('\n');
        }
    }
    s.push_str(&new_bullets.join("\n"));
    s.push('\n');
    s
}

/// Render the managed `chat-notes` frontmatter block.
fn render_chat_notes_block(provenance: &[(String, String)]) -> String {
    let mut s = String::from("chat-notes:\n  facts:\n");
    for (id, chat) in provenance {
        s.push_str(&format!("    \"{}\": \"{}\"\n", escape(id), escape(chat)));
    }
    s
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// A line-level additive diff: every line present in `new` but not matched in
/// `old` (a subsequence walk) is emitted with a `+` prefix. Our note edits are
/// purely additive, so this is exact and cheap — used for the tray summary.
fn additive_diff(old: &str, new: &str) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let mut i = 0;
    let mut out: Vec<String> = Vec::new();
    for line in new.lines() {
        if i < old_lines.len() && old_lines[i] == line {
            i += 1;
        } else {
            out.push(format!("+{line}"));
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(predicate: &str, object: &str) -> StagedFact {
        StagedFact {
            subject_id: "entity:bill_gates".into(),
            subject_name: "Bill Gates".into(),
            subject_type: "person".into(),
            predicate: predicate.into(),
            object_id: format!("entity:{}", slugify(object)),
            object_name: object.into(),
            object_type: "organization".into(),
            valid_from: chrono::Utc::now(),
            valid_from_explicit: false,
            valid_to: None,
            confidence: 0.9,
            explicit_coexist: false,
        }
    }

    #[test]
    fn predicate_phrasing_table_and_fallback() {
        assert_eq!(predicate_phrasing("works_at"), "Works at");
        assert_eq!(predicate_phrasing("founded"), "Founded");
        assert_eq!(predicate_phrasing("headquartered_in"), "Headquartered in");
        // Unknown predicate is humanised, not dropped.
        assert_eq!(predicate_phrasing("co_authored"), "Co authored");
    }

    #[test]
    fn render_fact_bullet_plain_and_dated() {
        let f = fact("founded", "Microsoft");
        assert_eq!(render_fact_bullet(&f), "- Founded Microsoft");

        let mut dated = fact("joined", "Acme");
        dated.valid_from_explicit = true;
        dated.valid_from = chrono::DateTime::parse_from_rfc3339("2021-06-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(render_fact_bullet(&dated), "- Joined Acme (2021)");
    }

    #[test]
    fn new_note_gets_frontmatter_and_facts_section() {
        let change = apply_facts_to_note(
            "People/Bill Gates.md",
            None,
            &[fact("founded", "Microsoft")],
            "chat_message:abc",
        );
        assert_eq!(change.kind, ChangeKind::Create);
        assert_eq!(change.facts.len(), 1);
        let c = &change.new_content;
        assert!(c.starts_with("---\nchat-notes:\n  facts:\n"));
        assert!(c.contains("\"founded:microsoft\": \"chat_message:abc\""));
        assert!(c.contains("## Facts\n\n- Founded Microsoft"));
        assert!(c.ends_with('\n'));
    }

    #[test]
    fn update_appends_under_facts_without_disturbing_prose() {
        let existing = "---\ntags: [people]\n---\n\nBill is a person I know.\n\n## Facts\n\n- Founded Microsoft\n\n## Notes\n\nMet him in 1998.\n";
        let change = apply_facts_to_note(
            "People/Bill Gates.md",
            Some(existing),
            &[fact("invests_in", "Cascade")],
            "chat_message:def",
        );
        assert_eq!(change.kind, ChangeKind::Update);
        let c = &change.new_content;
        // User frontmatter key preserved, chat-notes block added after it.
        assert!(c.contains("tags: [people]"));
        assert!(c.contains("\"invests_in:cascade\": \"chat_message:def\""));
        // Both old and new bullets present, prose untouched.
        assert!(c.contains("- Founded Microsoft"));
        assert!(c.contains("- Invests in Cascade"));
        assert!(c.contains("Bill is a person I know."));
        assert!(c.contains("## Notes\n\nMet him in 1998."));
        // The new bullet sits inside the Facts section, before ## Notes.
        let facts_at = c.find("- Invests in Cascade").unwrap();
        let notes_at = c.find("## Notes").unwrap();
        assert!(facts_at < notes_at);
    }

    #[test]
    fn re_applying_the_same_fact_is_idempotent() {
        let first = apply_facts_to_note(
            "People/Bill Gates.md",
            None,
            &[fact("founded", "Microsoft")],
            "chat_message:abc",
        );
        // Feed the produced note back in with the same fact.
        let second = apply_facts_to_note(
            "People/Bill Gates.md",
            Some(&first.new_content),
            &[fact("founded", "Microsoft")],
            "chat_message:xyz",
        );
        assert!(second.facts.is_empty(), "duplicate fact must not re-stage");
        assert_eq!(
            second.new_content, first.new_content,
            "idempotent re-apply leaves the note byte-identical"
        );
        // The bullet appears exactly once.
        assert_eq!(first.new_content.matches("- Founded Microsoft").count(), 1);
    }

    #[test]
    fn duplicate_facts_within_one_batch_collapse() {
        let change = apply_facts_to_note(
            "People/Bill Gates.md",
            None,
            &[fact("founded", "Microsoft"), fact("founded", "Microsoft")],
            "chat_message:abc",
        );
        assert_eq!(change.facts.len(), 1);
        assert_eq!(change.new_content.matches("- Founded Microsoft").count(), 1);
    }

    #[test]
    fn note_without_facts_section_appends_one() {
        let existing = "Just some freeform notes about Bill.\n";
        let change = apply_facts_to_note(
            "People/Bill Gates.md",
            Some(existing),
            &[fact("works_at", "Acme")],
            "chat_message:q",
        );
        let c = &change.new_content;
        assert!(c.contains("Just some freeform notes about Bill."));
        assert!(c.contains("## Facts\n\n- Works at Acme"));
        // Diff reports the additions.
        assert!(change.diff.contains("+- Works at Acme"));
    }
}
