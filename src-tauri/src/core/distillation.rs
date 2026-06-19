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

/// The receipt a finished distillation hands back: a one-line summary and the audit
/// `turn_id` the UI exposes for undo.
pub struct DistillResult {
    pub summary: String,
    pub turn_id: String,
}

/// Character cap on the transcript grounding pushed into the distillation turn —
/// sized like `chat_turn`'s `INJECTED_CONTEXT_BUDGET`. Beyond this the agent reads
/// the rest of the note with `read_note`.
const DISTILL_GROUNDING_BUDGET: usize = 8000;

/// Run the distillation turn for a just-ended Session. Returns `Ok(None)` when the
/// Meeting note has no transcript to distil (nothing was said / captured). On
/// success the turn is recorded as an undoable audit entry, exactly like a
/// `chat_turn`, and the assistant reply joins the meeting's conversation.
pub async fn distill_meeting(
    formation_root: &Path,
    note_rel: &str,
    title: &str,
    attendees: &[String],
    conversation_id: &str,
    store: &MemoryStore,
    cfg: &AppConfig,
) -> AppResult<Option<DistillResult>> {
    let note_abs = formation_root.join(note_rel);
    let windows = meeting_note::transcript_windows(&note_abs, meeting_note::DISTILL_WINDOW_BUDGET)?;
    if windows.is_empty() {
        return Ok(None);
    }

    let message = distillation_message(title, note_rel, attendees);
    // Provenance: a message in the meeting's own conversation, so every Fact the
    // turn records is stamped as coming from this meeting (ADR-0017 §7).
    let source_chat_id = store
        .insert_chat_message("user", &message, conversation_id)
        .await?;

    // Snapshot the formation before the turn so the diff/undo machinery learns what
    // the distillation changed — the same path `chat_turn` uses.
    let turn_id = audit::new_turn_id();
    let snapshot_dir = audit::snapshot_formation(formation_root, &turn_id)?;

    let engine = ClaudeCodeEngine::new(
        cfg.claude_code_model
            .clone()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| claude_code::DEFAULT_MODEL.to_string()),
    );
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

    let outcome = match engine.run_turn(&turn, &sink).await {
        Ok(o) => o,
        Err(e) => {
            // A failed turn writes no audit entry, so its snapshot would leak —
            // clean it up before propagating (mirrors `chat_turn`).
            std::fs::remove_dir_all(&snapshot_dir).ok();
            return Err(e);
        }
    };
    let reply = outcome.reply;

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

    Ok(Some(DistillResult {
        summary: summarize(&reply),
        turn_id,
    }))
}

/// The distillation instruction. A turn-scoped rider rather than a change to the
/// shared behaviour prompt, so normal chat turns are unaffected.
fn distillation_message(title: &str, note_rel: &str, attendees: &[String]) -> String {
    let who = if attendees.is_empty() {
        "the attendees".to_string()
    } else {
        attendees.join(", ")
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
person's note). Finish with a single short sentence summarising what you recorded \
— that line is the receipt the user sees."
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

/// One-line receipt from the reply: the last non-empty line (the agent is asked to
/// end with a summary sentence), capped. Falls back to a fixed line.
fn summarize(reply: &str) -> String {
    let line = reply
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or("");
    let line = line.trim_start_matches(['-', '*', '#', ' ']).trim();
    let line = if line.is_empty() {
        "Meeting distilled."
    } else {
        line
    };
    if line.chars().count() > 240 {
        let truncated: String = line.chars().take(239).collect();
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
    fn summarize_takes_last_line_and_caps() {
        let reply = "Recorded 3 facts.\nUpdated Sarah's note.\nSummary: filed 2 tasks and noted the Q3 decision.";
        assert_eq!(
            summarize(reply),
            "Summary: filed 2 tasks and noted the Q3 decision."
        );
        assert_eq!(summarize("   \n  "), "Meeting distilled.");
        assert!(summarize(&"x".repeat(500)).chars().count() <= 240);
    }
}
