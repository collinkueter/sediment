//! End-of-Session distillation turn (ADR-0017 §7, plan M6).
//!
//! When a meeting Session stops, the Agent does to the Meeting note what it already
//! does for a typed turn (ADR-0009): record Facts about attendees, update their
//! People notes, open Tasks for action items, capture Decisions — through the
//! **existing conversation engine**, not a parallel pipeline. It runs *quietly and
//! automatically* (ADR-0017 Q2): no gating prompt, a one-line summary as the
//! receipt, and the whole thing audited + undoable per Fact like any turn.
//!
//! Two things make it a distillation rather than a chat turn: the instruction tells
//! the agent to distil (not dump) and to gate named attribution on a clear speaker
//! (ADR-0017 Gap B), and the transcript is pushed **segment-windowed** so a long
//! meeting never blows the context budget (ADR-0011), with the full note one
//! `read_note` away. It runs on the cold Claude Code engine — the ~6 s spawn is
//! irrelevant after the meeting (ADR-0017 Gap A reserves the warm engine for live
//! chat).

use crate::commands::formation::APP_DIR;
use crate::core::agent_tone::AgentTone;
use crate::core::audit::{self, AuditEntry, ChatTurnEntry};
use crate::core::claude_code::{self, ClaudeCodeEngine};
use crate::core::conversation::{ConversationEngine, TurnEventSink, TurnRequest};
use crate::core::embedding::EmbeddingProvider;
use crate::core::formation_state::AppConfig;
use crate::core::meeting_note;
use crate::core::memory::MemoryStore;
use crate::core::ollama_sidecar;
use crate::error::AppResult;
use std::path::Path;
use tokio_util::sync::CancellationToken;

/// The receipt a finished distillation hands back: a one-line summary, the audit
/// `turn_id` the UI exposes for undo, and an optional content-derived title the UI
/// can offer as a rename when it improves on the one typed at Start.
pub struct DistillResult {
    pub summary: String,
    pub turn_id: String,
    pub suggested_title: Option<String>,
}

/// Character cap on the transcript grounding pushed into the distillation turn —
/// sized like `chat_turn`'s `INJECTED_CONTEXT_BUDGET`. Beyond this the agent reads
/// the rest of the note with `read_note`.
const DISTILL_GROUNDING_BUDGET: usize = 8000;

/// Run the distillation turn for a just-ended Session. Returns `Ok(None)` when the
/// Meeting note has no transcript to distil (nothing was said / captured). On
/// success the turn is recorded as an undoable audit entry, exactly like a
/// `chat_turn`, and the assistant reply joins the meeting's conversation.
#[allow(clippy::too_many_arguments)]
pub async fn distill_meeting(
    formation_root: &Path,
    note_rel: &str,
    title: &str,
    attendees: &[String],
    conversation_id: &str,
    store: &MemoryStore,
    cfg: &AppConfig,
    copilot: &crate::core::copilot::CopilotEngineHandle,
) -> AppResult<Option<DistillResult>> {
    let note_abs = formation_root.join(note_rel);
    let windows = meeting_note::transcript_windows(&note_abs, meeting_note::DISTILL_WINDOW_BUDGET)?;
    if windows.is_empty() {
        return Ok(None);
    }

    // Unidentified voices (with any borderline guess the second pass recorded) so the
    // agent can ask who they were in its reply — the Agent-led debrief (ADR-0017 §6).
    let suggestions = meeting_note::read_speaker_suggestions(&note_abs).unwrap_or_default();
    let unresolved: Vec<(String, Option<String>)> = attendees
        .iter()
        .filter(|a| crate::core::session::is_unknown_speaker(a))
        .map(|a| {
            let guess = suggestions
                .iter()
                .find(|(label, _)| label == a)
                .map(|(_, name)| name.clone());
            (a.clone(), guess)
        })
        .collect();
    let message = distillation_message(title, note_rel, attendees, &unresolved);
    // Provenance: a message in the meeting's own conversation, so every Fact the
    // turn records is stamped as coming from this meeting (ADR-0017 §7).
    let source_chat_id = store
        .insert_chat_message("user", &message, conversation_id)
        .await?;

    // Snapshot the formation before the turn so the diff/undo machinery learns what
    // the distillation changed — the same path `chat_turn` uses.
    let turn_id = audit::new_turn_id();
    let snapshot_dir = audit::snapshot_formation(formation_root, &turn_id)?;

    let turn = TurnRequest {
        message: message.clone(),
        history: Vec::new(),
        formation_root: formation_root.to_path_buf(),
        source_chat_id: source_chat_id.clone(),
        embedding_provider: EmbeddingProvider::from_config(cfg.embedding_provider.as_deref())
            .as_str()
            .to_string(),
        ollama_url: ollama_sidecar::resolved_endpoint(cfg.ollama_url.clone()),
        injected_context: grounding_from_windows(&windows, DISTILL_GROUNDING_BUDGET),
        tone: AgentTone::from_config(cfg.agent_tone.as_deref())
            .as_str()
            .to_string(),
        // Distillation is not user-interruptible — it runs in the background after
        // the capture surface has already collapsed.
        cancel: CancellationToken::new(),
        conversation_id: conversation_id.to_string(),
    };
    // Quiet: the agent's tokens are not streamed to any live surface.
    let sink: TurnEventSink = Box::new(|_event| {});

    // Run on the user's configured engine — the warm Copilot engine when selected
    // (so Copilot-only users, who may not have Claude Code installed, still get a
    // distillation), else cold Claude Code (the ~6 s spawn is irrelevant here).
    let (engine_label, model_label) = if cfg.conversation_engine.as_deref() == Some("copilot") {
        (
            "copilot",
            cfg.copilot_model
                .clone()
                .filter(|m| !m.trim().is_empty())
                .unwrap_or_else(|| crate::core::copilot::DEFAULT_MODEL.to_string()),
        )
    } else {
        (
            "claude-code",
            cfg.claude_code_model
                .clone()
                .filter(|m| !m.trim().is_empty())
                .unwrap_or_else(|| claude_code::DEFAULT_MODEL.to_string()),
        )
    };
    tracing::info!(
        engine = engine_label,
        model = %model_label,
        windows = windows.len(),
        attendees = attendees.len(),
        "distillation: running turn"
    );
    let run = if engine_label == "copilot" {
        copilot.run_turn(&turn, &sink, &model_label).await
    } else {
        ClaudeCodeEngine::new(model_label.clone())
            .run_turn(&turn, &sink)
            .await
    };
    let outcome = match run {
        Ok(o) => o,
        Err(e) => {
            // A failed turn writes no audit entry, so its snapshot would leak —
            // clean it up before propagating (mirrors `chat_turn`).
            tracing::warn!(engine = engine_label, error = %e, "distillation: engine turn failed");
            std::fs::remove_dir_all(&snapshot_dir).ok();
            return Err(e);
        }
    };
    let reply = outcome.reply;
    // The full reply is the single most useful thing to see when a distillation goes
    // wrong (a refusal, a truncation, the model ignoring the receipt format). Log its
    // shape at info and the whole text at debug so it's always recoverable from
    // `sediment.log` (raise with `SEDIMENT_LOG=debug`).
    tracing::info!(
        engine = engine_label,
        reply_chars = reply.len(),
        has_summary_marker = marker_value(&reply, "SUMMARY:").is_some(),
        "distillation: engine returned a reply"
    );
    tracing::debug!(reply = %reply, "distillation: full agent reply");

    // Record the turn as an undoable audit entry (ADR-0009 §6): diff the snapshot
    // for changed notes and collect the Facts stamped with this turn's provenance.
    let changed_notes = audit::diff_formation(formation_root, &snapshot_dir)?;
    let recorded_fact_ids = store.facts_by_source(&source_chat_id).await?;
    let entry = AuditEntry::ChatTurn(ChatTurnEntry {
        turn_id: turn_id.clone(),
        created: chrono::Utc::now(),
        user_excerpt: audit::excerpt(&format!("Meeting distillation — {title}")),
        reply_excerpt: audit::excerpt(&reply),
        snapshot_dir: format!("{APP_DIR}/snapshots/{turn_id}"),
        changed_notes,
        recorded_fact_ids,
    });
    audit::write_audit(formation_root, &entry)?;
    if let Err(e) = audit::prune_old(formation_root, audit::AUDIT_RETENTION) {
        tracing::warn!("distillation: prune old snapshots failed: {e}");
    }

    // The reply joins the meeting's conversation transcript.
    store
        .insert_chat_message("assistant", &reply, conversation_id)
        .await?;

    // Never let a refusal or empty reply become the receipt the user sees. When the
    // agent ignored the format AND declined/said nothing, show a neutral line and log
    // the raw reply (so the *why* is in the log, not lost behind a polite refusal).
    let had_marker = marker_value(&reply, "SUMMARY:").is_some();
    let (summary, suggested_title) = if !had_marker
        && (reply.trim().is_empty() || looks_like_refusal(&reply))
    {
        tracing::warn!(
            engine = engine_label,
            reply_excerpt = %audit::excerpt(&reply),
            "distillation: no usable summary (refusal or empty reply) — showing a neutral receipt; \
             see the full reply at debug level"
        );
        (
            "Meeting saved — couldn't auto-summarize this one.".to_string(),
            None,
        )
    } else {
        parse_receipt(&reply, title)
    };
    tracing::info!(
        turn_id = %turn_id,
        summary = %summary,
        suggested_title = ?suggested_title,
        "distillation: done"
    );
    Ok(Some(DistillResult {
        summary,
        turn_id,
        suggested_title,
    }))
}

/// The distillation instruction. A turn-scoped rider rather than a change to the
/// shared behaviour prompt, so normal chat turns are unaffected.
/// Build the line the distillation turn asks the agent to act on. `unresolved` lists
/// the meeting's unidentified voices as `(label, maybe_suggested_name)` so the agent
/// can raise them by name in its reply (ADR-0017 §6 Part B — the Agent-led debrief).
fn distillation_message(
    title: &str,
    note_rel: &str,
    attendees: &[String],
    unresolved: &[(String, Option<String>)],
) -> String {
    let who = if attendees.is_empty() {
        "the attendees".to_string()
    } else {
        attendees.join(", ")
    };
    // When voices went unidentified, ask the user who they were — name a best guess
    // where the second pass left one, but ask, never assert (suggest-not-assert §6).
    let reconcile = if unresolved.is_empty() {
        String::new()
    } else {
        let list = unresolved
            .iter()
            .map(|(label, guess)| match guess {
                Some(name) => format!("\"{label}\" (possibly {name})"),
                None => format!("\"{label}\""),
            })
            .collect::<Vec<_>>()
            .join("; ");
        format!(
            "\n\nSome voices were not identified: {list}. Before the SUMMARY line, add one \
short, friendly sentence asking who they were so they can be named — offer your best \
guess where there is one, but ask, never assert. Do not record Facts attributed to an \
unidentified voice.\n"
        )
    };
    format!(
        "This is an automatic end-of-meeting distillation — no one is waiting on a \
reply. The meeting \"{title}\" just ended; its Meeting note is at `{note_rel}` and \
its attendees are {who}. Read the note ('read_note') and distil it the way you \
handle any turn:\n\
\n\
- Record durable Facts about the attendees with 'record_fact', superseding any \
that are now stale.\n\
- Update each attendee's People note with what you learned about them.\n\
- Open a Task ('record_task') or open loop for each action item or commitment.\n\
- Capture Decisions made.\n\
\n\
Distil, don't dump: record what matters, not every utterance. Attribute a Fact to \
a *named* person only when the transcript makes the speaker clear; otherwise record \
it unattributed rather than guess (a wrong attribution silently pollutes a real \
person's note).\n\
{reconcile}\
\n\
Finish your reply with exactly these two lines, each on its own line and nothing \
after them:\n\
SUMMARY: <one short sentence summarising what you recorded — the receipt the user sees>\n\
TITLE: <a concise 3-to-6 word title naming what this meeting was actually about>\n\
\n\
The meeting was opened as \"{title}\". If that name already captures the topic well, \
repeat it on the TITLE line; only suggest a different title when the content clearly \
warrants a better one."
    )
}

/// Render the segment-windowed transcript as one grounding block under `budget`,
/// oldest window first. Whole windows are kept while they fit; the rest is left for
/// the agent to pull with `read_note`.
fn grounding_from_windows(windows: &[String], budget: usize) -> Option<String> {
    let header = "## Meeting transcript (for distillation)\n";
    let mut out = String::from(header);
    let mut wrote_any = false;
    for window in windows {
        if out.len() + window.len() + 1 > budget {
            break;
        }
        out.push_str(window);
        out.push('\n');
        wrote_any = true;
    }
    wrote_any.then_some(out)
}

/// Pull the receipt out of the reply: the one-line `summary` and an optional
/// content-derived `suggested_title`, both emitted as trailing `SUMMARY:` / `TITLE:`
/// marker lines (see `distillation_message`). The summary falls back to the last
/// non-empty line when the marker is missing, so an older-style reply still yields
/// a receipt. The title is dropped when it is empty, the bare "Meeting" fallback,
/// or equal to `current_title` — i.e. only a genuinely *different* name is offered.
fn parse_receipt(reply: &str, current_title: &str) -> (String, Option<String>) {
    let summary = marker_value(reply, "SUMMARY:")
        .or_else(|| last_receipt_line(reply))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Meeting distilled.".to_string());
    let summary = cap(&summary, 240);

    let suggested_title = marker_value(reply, "TITLE:").and_then(|raw| {
        let clean = meeting_note::sanitize_title(&raw);
        let differs = !clean.eq_ignore_ascii_case(&meeting_note::sanitize_title(current_title));
        (clean != "Meeting" && differs).then(|| cap(&clean, 120))
    });
    (summary, suggested_title)
}

/// The text after the first `MARKER:` line (case-insensitive, list-bullet/`#`
/// prefixes stripped), trimmed; `None` when absent or empty.
fn marker_value(reply: &str, marker: &str) -> Option<String> {
    reply.lines().find_map(|l| {
        let l = l.trim_start_matches(['-', '*', '#', ' ']).trim();
        let rest = l
            .get(..marker.len())?
            .eq_ignore_ascii_case(marker)
            .then(|| {
                l[marker.len()..]
                    .trim()
                    .trim_matches(['"', '\'', '“', '”'])
                    .trim()
            })?;
        (!rest.is_empty()).then(|| rest.to_string())
    })
}

/// The last non-empty line as a fallback summary, skipping a trailing `TITLE:`
/// marker so it is never mistaken for the receipt.
fn last_receipt_line(reply: &str) -> Option<String> {
    reply
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .rfind(|l| {
            !l.trim_start_matches(['-', '*', '#', ' '])
                .to_ascii_uppercase()
                .starts_with("TITLE:")
        })
        .map(|l| {
            l.trim_start_matches(['-', '*', '#', ' '])
                .trim()
                .to_string()
        })
}

/// Whether `reply` reads like a model **refusal** or canned non-answer rather than a
/// distillation. Used so a refusal ("I'm sorry, but I cannot assist with that
/// request.") never becomes the user-facing receipt — instead we show a neutral line
/// and log the raw reply for debugging. Matched case-insensitively against the
/// openings models use to decline; only the first ~200 chars are inspected so a long
/// genuine summary that happens to contain such a phrase later isn't misflagged.
fn looks_like_refusal(reply: &str) -> bool {
    let head: String = reply
        .trim()
        .chars()
        .take(200)
        .collect::<String>()
        .to_lowercase();
    const SIGNS: &[&str] = &[
        "i'm sorry",
        "i am sorry",
        "i apologize",
        "i apologise",
        "i cannot assist",
        "i can't assist",
        "i cannot help",
        "i can't help",
        "i cannot fulfill",
        "i can't fulfill",
        "i cannot comply",
        "i can't comply",
        "unable to assist",
        "unable to help",
        "i won't be able to",
        "i will not be able to",
        "as an ai",
    ];
    SIGNS.iter().any(|sign| head.contains(sign))
}

/// Cap a line at `max` chars, appending an ellipsis when it had to be cut.
fn cap(line: &str, max: usize) -> String {
    if line.chars().count() > max {
        let truncated: String = line.chars().take(max - 1).collect();
        format!("{truncated}…")
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grounding_packs_windows_within_budget() {
        let windows = vec!["A".repeat(100), "B".repeat(100), "C".repeat(100)];
        // Header (~40) + two 100-char windows + newlines fits in 260; the third doesn't.
        let g = grounding_from_windows(&windows, 260).unwrap();
        assert!(g.contains(&"A".repeat(100)) && g.contains(&"B".repeat(100)));
        assert!(
            !g.contains(&"C".repeat(100)),
            "third window dropped over budget"
        );
        // Empty transcript → no grounding.
        assert!(grounding_from_windows(&[], 1000).is_none());
    }

    #[test]
    fn distillation_message_raises_unresolved_voices_with_guesses() {
        // No unknowns → no reconciliation ask.
        let plain = distillation_message("Q3", "Meetings/q3.md", &["Self".into()], &[]);
        assert!(!plain.contains("not identified"));

        // Unknowns → the agent is asked to raise them, naming a guess where present.
        let unresolved = vec![
            ("Unknown speaker 2".to_string(), Some("Dana Kim".to_string())),
            ("Unknown speaker 3".to_string(), None),
        ];
        let m = distillation_message("Q3", "Meetings/q3.md", &["Self".into()], &unresolved);
        assert!(m.contains("Some voices were not identified"));
        assert!(m.contains("\"Unknown speaker 2\" (possibly Dana Kim)"));
        assert!(m.contains("\"Unknown speaker 3\""));
        assert!(m.contains("ask, never assert"));
        // The SUMMARY/TITLE contract still terminates the prompt.
        assert!(m.contains("SUMMARY:") && m.contains("TITLE:"));
    }

    #[test]
    fn parse_receipt_pulls_summary_and_title_markers() {
        let reply = "Recorded 3 facts.\nUpdated Sarah's note.\n\
                     SUMMARY: Filed 2 tasks and noted the Q3 budget decision.\n\
                     TITLE: Q3 Budget Review";
        let (summary, title) = parse_receipt(reply, "Untitled");
        assert_eq!(summary, "Filed 2 tasks and noted the Q3 budget decision.");
        assert_eq!(title.as_deref(), Some("Q3 Budget Review"));
    }

    #[test]
    fn parse_receipt_drops_title_equal_to_current_or_fallback() {
        // Same title (case/space-insensitive via sanitize) → no rename offered.
        let (_, same) = parse_receipt("SUMMARY: ok\nTITLE: Weekly  Sync", "weekly sync");
        assert_eq!(same, None);
        // The bare fallback is never offered as a rename.
        let (_, fb) = parse_receipt("SUMMARY: ok\nTITLE:    ", "Whatever");
        assert_eq!(fb, None);
    }

    #[test]
    fn parse_receipt_falls_back_to_last_line_without_markers() {
        // No SUMMARY marker → last non-TITLE line is the receipt; no title offered.
        let (summary, title) = parse_receipt("Recorded the decision.\nTITLE: A Title", "x");
        assert_eq!(summary, "Recorded the decision.");
        assert_eq!(title.as_deref(), Some("A Title"));
        // Empty reply → the fixed fallback line.
        assert_eq!(parse_receipt("   \n  ", "x").0, "Meeting distilled.");
        assert!(cap(&"x".repeat(500), 240).chars().count() <= 240);
    }

    #[test]
    fn looks_like_refusal_catches_declines_not_real_summaries() {
        assert!(looks_like_refusal(
            "I'm sorry, but I cannot assist with that request."
        ));
        assert!(looks_like_refusal(
            "I apologize, but I'm unable to help with this."
        ));
        assert!(looks_like_refusal("As an AI, I can't do that."));
        // A real summary that merely mentions an apology later is not a refusal.
        assert!(!looks_like_refusal(
            "Recorded that Sarah apologized for the delay and will resend the deck."
        ));
        assert!(!looks_like_refusal(
            "Filed 2 tasks and noted the Q3 budget decision."
        ));
    }
}
