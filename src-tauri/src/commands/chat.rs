//! Chat-driven commands. Phase 2 ships `chat_write`: a Write-mode message is
//! persisted, run through the extraction pipeline, and its facts land in
//! SurrealDB with provenance back to the stored chat message.
//!
//! Ask-mode (`chat_ask`) and intent classification follow in P2-M6 / P2-M7.

use crate::commands::extraction::{run_extract_facts, ExtractFactsResult};
use crate::commands::formation::APP_DIR;
use crate::core::formation_state::FormationState;
use crate::core::memory::MemoryHandle;
use crate::error::AppResult;
use serde::Serialize;
use tauri::State;

#[derive(Debug, Serialize)]
pub struct ChatWriteResult {
    /// Record id of the persisted user message (provenance for the facts).
    pub source_chat_id: String,
    /// Extraction outcome — entities upserted, facts written, skip counts.
    pub extraction: ExtractFactsResult,
}

/// Write-mode chat turn: persist the user's message, extract entities and
/// relations from it, and write the facts with provenance pointing back at
/// the stored message. Returns a structured summary for the chat pane.
#[tauri::command]
pub async fn chat_write(
    message: String,
    session_id: String,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
) -> AppResult<ChatWriteResult> {
    let formation_root = formation.require()?;
    let memory_dir = formation_root.join(APP_DIR).join("memory");
    let store = memory.get_or_init(&memory_dir).await?;

    // Persist the user message first so extracted facts can cite it.
    let source_chat_id = store
        .insert_chat_message("user", &message, &session_id)
        .await?;

    let extraction = run_extract_facts(&message, &source_chat_id, &formation_root, store).await?;

    Ok(ChatWriteResult {
        source_chat_id,
        extraction,
    })
}
