//! The conversational-agent command (ADR-0009).
//!
//! `chat_turn` is the single streaming command that drives a turn: persist the
//! user message, snapshot the formation, run the selected `ConversationEngine`,
//! diff the snapshot for changed notes, write a per-turn audit entry, persist
//! the reply. Recording and answering happen in the same turn; review is the
//! audit log, not a blocking queue.

use crate::commands::formation::APP_DIR;
use crate::core::audit::{self, AuditEntry, ChangedNote, ChatTurnEntry};
use crate::core::claude_code::{self, ClaudeCodeEngine};
use crate::core::conversation::{
    ConversationEngine, TranscriptTurn, TurnEvent, TurnEventSink, TurnOutcome, TurnRequest,
};
use crate::core::copilot::{self, CopilotEngineHandle};
use crate::core::embedding::EmbeddingProvider;
use crate::core::formation_state::{AppConfig, FormationState};
use crate::core::memory::MemoryHandle;
use crate::core::ollama_sidecar::OllamaSidecar;
use crate::core::pre_pass;
use crate::core::working_set::{self, WorkingSet};
use crate::error::AppResult;
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::State;

/// How many prior turns of conversation the agent sees as its continuity
/// window (ADR-0009 §5: ≈ the last 10–20 turns; older context is the agent's
/// own job to pull with `search_notes` / `read_note`).
const TURN_HISTORY_LIMIT: usize = 20;

/// Max characters of pushed grounding (ADR-0011 §6) — a proxy for tokens so the
/// prompt, and the subscription quota it spends, stay bounded.
const INJECTED_CONTEXT_BUDGET: usize = 8000;

/// What one `chat_turn` produced, returned to the front end.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTurnResult {
    /// The audit-entry id for this turn — the handle the audit-log panel uses
    /// to revert it (`undo_turn`).
    pub turn_id: String,
    /// The full assistant reply (also streamed token-by-token over `on_event`).
    pub reply: String,
    /// Notes the turn changed, learned by diffing the pre-turn snapshot.
    pub changed_notes: Vec<ChangedNote>,
    /// How many graph Facts the turn recorded through the MCP server.
    pub recorded_fact_count: usize,
    /// The Working Set as of this turn — what's in play, for the UI panel
    /// (ADR-0011 §3). Also pushed into the agent's prompt.
    pub working_set: WorkingSet,
}

/// Build the cold-spawn `ConversationEngine` for this turn — the Claude Code
/// engine, run on `claude_code_model` or the default alias. The warm Copilot
/// engine is resident and routed separately in `chat_turn` (ADR-0012).
fn build_engine(cfg: &AppConfig) -> Box<dyn ConversationEngine> {
    // The warm Copilot engine is routed in `chat_turn` (it is resident, not
    // rebuilt per turn). Only the cold Claude Code engine is built here; ADR-0012
    // retired the Gemini CLI engine.
    let model = cfg
        .claude_code_model
        .clone()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| claude_code::DEFAULT_MODEL.to_string());
    Box::new(ClaudeCodeEngine::new(model))
}

/// One conversational turn (ADR-0009). Persists the user message, snapshots
/// the whole formation, runs the selected `ConversationEngine`, diffs the
/// snapshot for changed notes, writes a per-turn audit entry, and persists the
/// assistant reply.
///
/// `on_event` streams [`TurnEvent`]s — reply text deltas and tool-activity
/// lines — to the UI as the turn runs; the returned [`ChatTurnResult`] is the
/// authoritative outcome.
#[tauri::command]
pub async fn chat_turn(
    message: String,
    session_id: String,
    on_event: Channel<TurnEvent>,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
    copilot: State<'_, CopilotEngineHandle>,
    app: tauri::AppHandle,
) -> AppResult<ChatTurnResult> {
    let formation_root = formation.require()?;
    let memory_dir = formation_root.join(APP_DIR).join("memory");
    let store = memory.get_or_init(&memory_dir).await?;

    // 1. Persist the user message — its id is the provenance the MCP server
    //    stamps onto every Fact this turn records.
    let source_chat_id = store
        .insert_chat_message("user", &message, &session_id)
        .await?;

    // 2. The recent-window transcript: the session's last messages, oldest
    //    first, minus the user message just inserted (it is the new `message`,
    //    not history). `recent_messages` is newest-tail; drop the trailing
    //    user row that equals the one we just wrote.
    let mut recent = store
        .recent_messages(&session_id, TURN_HISTORY_LIMIT + 1)
        .await?;
    if matches!(recent.last(), Some((role, content)) if role == "user" && content == &message) {
        recent.pop();
    }
    let history: Vec<TranscriptTurn> = recent
        .into_iter()
        .map(|(role, content)| TranscriptTurn { role, content })
        .collect();

    // 3. Snapshot the whole formation BEFORE the turn. The agent edits notes
    //    with its own file tools, so the diff is how the audit log learns what
    //    changed (ADR-0009 §6).
    let turn_id = audit::new_turn_id();
    let snapshot_dir = audit::snapshot_formation(&formation_root, &turn_id)?;

    // 4. Run the engine. The Tauri `Channel` is wrapped in a `TurnEventSink`
    //    closure at this edge so the engine layer never depends on Tauri IPC.
    let cfg = AppConfig::load(&app);
    let embedding_provider = EmbeddingProvider::from_config(cfg.embedding_provider.as_deref());
    // ADR-0011 §2–§3, §6: deterministic pre-pass + Working Set, pushed into the
    // turn so grounding never depends on the agent choosing to search. Assembled
    // in priority order under a budget (§6): resolved identity + Facts first (the
    // reliability fix), then the Working Set, then related-note excerpts (the
    // first droppable thing). Best-effort — a failing signal degrades to less
    // grounding, never a failed turn.
    let pre = pre_pass::build_pre_pass(
        store,
        &OllamaSidecar::default(),
        embedding_provider,
        &message,
    )
    .await;
    let working_set = working_set::derive_working_set(store).await;
    let injected_context = assemble_grounding(
        &[
            pre.render_entities_markdown(),
            working_set.render_markdown(),
            pre.render_related_markdown(),
        ],
        INJECTED_CONTEXT_BUDGET,
    );

    let turn_request = TurnRequest {
        message: message.clone(),
        history,
        formation_root: formation_root.clone(),
        source_chat_id: source_chat_id.clone(),
        embedding_provider: embedding_provider.as_str().to_string(),
        injected_context,
    };
    let sink: TurnEventSink = {
        let channel = on_event.clone();
        Box::new(move |event: TurnEvent| {
            // A failed send means the UI dropped the channel — log and move
            // on; the turn's outcome is still captured in the audit entry.
            if let Err(e) = channel.send(event) {
                tracing::warn!("chat_turn: forward TurnEvent failed: {e}");
            }
        })
    };
    // On engine failure, the pre-turn snapshot would otherwise leak forever —
    // `prune_old` only sees turns with audit entries, which a failed turn never
    // writes. Clean it up before propagating the error.
    // ADR-0012: the warm Copilot engine is resident in Tauri state (one session
    // across turns), not rebuilt per turn like the cold engines — so it is
    // routed here rather than through `build_engine`.
    let turn_outcome = if cfg.conversation_engine.as_deref() == Some("copilot") {
        let model = cfg
            .copilot_model
            .clone()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| copilot::DEFAULT_MODEL.to_string());
        copilot.run_turn(&turn_request, &sink, &model).await
    } else {
        build_engine(&cfg).run_turn(&turn_request, &sink).await
    };
    let TurnOutcome { reply } = match turn_outcome {
        Ok(o) => o,
        Err(e) => {
            std::fs::remove_dir_all(&snapshot_dir).ok();
            return Err(e);
        }
    };

    // ADR-0011 §4: rotate the surfaced open loop so the rider doesn't repeat the
    // same one. The Working Set lists loops least-recently-surfaced first and the
    // prompt nudges the agent to prefer the first; mark that one surfaced so it
    // cycles to the back next turn. Best-effort.
    if let Some(first_loop) = working_set.open_loops.first() {
        if let Err(e) = store.mark_loop_surfaced(&first_loop.id).await {
            tracing::warn!("chat_turn: mark_loop_surfaced failed: {e}");
        }
    }

    // 5. After the turn: diff the snapshot for changed notes, and query the
    //    Facts stamped with this turn's provenance.
    let changed_notes = audit::diff_formation(&formation_root, &snapshot_dir)?;
    let recorded_fact_ids = store.facts_by_source(&source_chat_id).await?;
    let recorded_fact_count = recorded_fact_ids.len();

    // 6. Write the audit entry, then prune snapshots beyond the retention
    //    window (ADR-0009: the last 20 turns).
    let entry = AuditEntry::ChatTurn(ChatTurnEntry {
        turn_id: turn_id.clone(),
        created: chrono::Utc::now(),
        user_excerpt: audit::excerpt(&message),
        reply_excerpt: audit::excerpt(&reply),
        snapshot_dir: format!("{APP_DIR}/snapshots/{turn_id}"),
        changed_notes: changed_notes.clone(),
        recorded_fact_ids,
    });
    audit::write_audit(&formation_root, &entry)?;
    if let Err(e) = audit::prune_old(&formation_root, audit::AUDIT_RETENTION) {
        tracing::warn!("chat_turn: prune old snapshots failed: {e}");
    }

    // 7. Persist the assistant reply so it joins the transcript.
    store
        .insert_chat_message("assistant", &reply, &session_id)
        .await?;

    Ok(ChatTurnResult {
        turn_id,
        reply,
        changed_notes,
        recorded_fact_count,
        working_set,
    })
}

/// The current Working Set — for the "what's in play" panel on load and refresh
/// (ADR-0011 §3). Cheap; derived fresh from the store, no agent involved.
#[tauri::command]
pub async fn get_working_set(
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
) -> AppResult<WorkingSet> {
    let root = formation.require()?;
    let store = memory
        .get_or_init(&root.join(APP_DIR).join("memory"))
        .await?;
    Ok(working_set::derive_working_set(store).await)
}

/// Dismiss an Open Loop from the UI — archives it so it stops surfacing
/// (ADR-0011 §5). The one-tap companion to the agent's `close_open_loop`.
#[tauri::command]
pub async fn dismiss_open_loop(
    loop_id: String,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
) -> AppResult<()> {
    let root = formation.require()?;
    let store = memory
        .get_or_init(&root.join(APP_DIR).join("memory"))
        .await?;
    store.close_open_loop(&loop_id).await
}

/// Assemble grounding sections in priority order under a character budget
/// (ADR-0011 §6). Whole sections are included while they fit; the first section
/// that overflows is truncated and assembly stops, so lower-priority sections are
/// dropped rather than crowding out the reliability-critical ones. `None` when
/// there is nothing to inject.
fn assemble_grounding(sections: &[Option<String>], budget: usize) -> Option<String> {
    let mut out = String::new();
    for section in sections.iter().flatten() {
        let sep = if out.is_empty() { 0 } else { 2 };
        if out.len() + sep >= budget {
            break;
        }
        let room = budget - out.len() - sep;
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        if section.len() <= room {
            out.push_str(section);
        } else {
            out.push_str(&truncate_chars(section, room));
            break;
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Truncate to at most `max` bytes on a char boundary, with a trailing ellipsis.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    // Reserve room for the ellipsis so the result stays within `max` bytes.
    let mut end = max.saturating_sub('…'.len_utf8()).min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assemble_grounding_keeps_priority_and_respects_budget() {
        let a = "A".repeat(100);
        let b = "B".repeat(100);
        let c = "C".repeat(100);
        // Budget fits A + "\n\n" + B (202) but leaves no room for C.
        let out = assemble_grounding(&[Some(a.clone()), Some(b.clone()), Some(c)], 204).unwrap();
        assert!(out.contains(&a), "highest-priority section kept");
        assert!(out.contains(&b), "second section kept");
        assert!(
            !out.contains('C'),
            "overflowing lower-priority section dropped"
        );

        // Nothing to inject.
        assert!(assemble_grounding(&[None, None], 1000).is_none());

        // A single oversize section is truncated, not dropped.
        let out = assemble_grounding(&[Some("X".repeat(1000))], 100).unwrap();
        assert!(
            out.len() <= 100,
            "truncated within budget including the ellipsis"
        );
        assert!(out.ends_with('…'));
    }
}
