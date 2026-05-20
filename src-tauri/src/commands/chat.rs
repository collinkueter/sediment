//! Chat-driven commands.
//!
//! - `chat_write`: a Write-mode message is persisted, run through the
//!   extraction pipeline, and its facts land in SurrealDB with provenance.
//! - `chat_ask`: an Ask-mode question runs hybrid retrieval (vector search
//!   over note chunks + best-effort graph facts) and streams a cited answer.
//!
//! Intent classification (which of the two to use) follows in P2-M7.

use crate::commands::extraction::{run_extract_facts, ExtractFactsResult};
use crate::commands::formation::APP_DIR;
use crate::core::extraction::{
    default_relation_schema, extract_entities_and_relations, GlinerExtractor, ModelPaths,
    ENTITY_LABELS,
};
use crate::core::formation_state::FormationState;
use crate::core::memory::{ChunkHit, MemoryHandle, MemoryStore};
use crate::core::ollama_sidecar::{OllamaSidecar, DEFAULT_EMBED_MODEL};
use crate::error::{AppError, AppResult};
use futures::StreamExt;
use ollama_rs::generation::completion::request::GenerationRequest;
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::State;

/// Chat model for Ask-mode answer generation.
const ANSWER_MODEL: &str = "llama3.2:3b";
/// How many note chunks to pull into the answer context.
const RETRIEVAL_K: usize = 5;

/// Classify a draft message as Write or Ask. Pure heuristic — no formation or
/// model needed, so it is safe to call on every keystroke.
#[tauri::command]
pub fn classify_intent(message: String) -> crate::core::intent::IntentResult {
    crate::core::intent::classify(&message)
}

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

#[derive(Debug, Clone, Serialize)]
pub struct RetrievedSource {
    pub note_path: String,
    pub chunk_idx: i64,
    pub text: String,
    pub distance: f32,
}

#[derive(Debug, Serialize)]
pub struct ChatAskResult {
    pub source_chat_id: String,
    /// Note chunks that informed the answer, for a "show sources" panel.
    pub sources: Vec<RetrievedSource>,
    /// Whether graph facts were folded into the context (false if no GLiNER model).
    pub used_graph: bool,
}

/// Ask-mode chat turn. Persists the question, retrieves relevant note chunks
/// (vector search) plus best-effort graph facts, then streams a cited answer
/// through `on_token`. The answer is also persisted as an assistant message.
#[tauri::command]
pub async fn chat_ask(
    query: String,
    session_id: String,
    on_token: Channel<String>,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
    sidecar: State<'_, OllamaSidecar>,
) -> AppResult<ChatAskResult> {
    let formation_root = formation.require()?;
    let memory_dir = formation_root.join(APP_DIR).join("memory");
    let store = memory.get_or_init(&memory_dir).await?;

    let source_chat_id = store
        .insert_chat_message("user", &query, &session_id)
        .await?;

    // --- Retrieval: vector search over note chunks ---
    let query_embedding = sidecar.embed(DEFAULT_EMBED_MODEL, &query).await?;
    let hits: Vec<ChunkHit> = store.search_chunks(query_embedding, RETRIEVAL_K).await?;
    let sources: Vec<RetrievedSource> = hits
        .iter()
        .map(|h| RetrievedSource {
            note_path: h.note_path.clone(),
            chunk_idx: h.chunk_idx,
            text: h.text.clone(),
            distance: h.distance,
        })
        .collect();

    // --- Retrieval: best-effort graph facts (skipped if no GLiNER model) ---
    let graph_facts = retrieve_graph_facts(&formation_root, store, &query).await;
    let used_graph = !graph_facts.is_empty();

    // --- Assemble the prompt ---
    let prompt = build_ask_prompt(&query, &sources, &graph_facts);

    // --- Stream the answer ---
    let client = sidecar.client();
    let mut stream = client
        .generate_stream(GenerationRequest::new(ANSWER_MODEL.to_string(), prompt))
        .await
        .map_err(|e| AppError::other(format!("start answer generation: {e}")))?;
    let mut answer = String::new();
    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.map_err(|e| AppError::other(format!("stream error: {e}")))?;
        for response in chunk {
            if !response.response.is_empty() {
                answer.push_str(&response.response);
                on_token
                    .send(response.response)
                    .map_err(|e| AppError::other(format!("channel send: {e}")))?;
            }
        }
    }

    // Persist the assistant answer too — keeps the chat_message log complete.
    store
        .insert_chat_message("assistant", &answer, &session_id)
        .await?;

    Ok(ChatAskResult {
        source_chat_id,
        sources,
        used_graph,
    })
}

/// Run NER on the query, then pull current facts for each detected entity.
/// Returns an empty vec (rather than erroring) when the GLiNER model is
/// absent — Ask mode degrades to vector-only retrieval.
async fn retrieve_graph_facts(
    formation_root: &std::path::Path,
    store: &MemoryStore,
    query: &str,
) -> Vec<String> {
    let paths = ModelPaths::under_app_dir(&formation_root.join(APP_DIR));
    if !paths.exist() {
        return Vec::new();
    }
    let extractor = GlinerExtractor::new(paths);
    let schema = default_relation_schema();
    let Ok((entities, _relations)) =
        extract_entities_and_relations(&extractor, query, ENTITY_LABELS, &schema)
    else {
        return Vec::new();
    };

    let mut facts = Vec::new();
    for ent in entities {
        // upsert_entity is idempotent — used here purely to resolve the id.
        let Ok(resolved) = store.upsert_entity(&ent.text, &ent.class, Vec::new()).await else {
            continue;
        };
        if let Ok(rows) = store.current_facts(&resolved.id).await {
            for f in rows {
                facts.push(format!(
                    "{} {} {}",
                    record_local(&f.subject),
                    f.predicate,
                    record_local(&f.object),
                ));
            }
        }
    }
    facts
}

/// Strip the `table:` prefix from a SurrealDB record id for display.
fn record_local(rid: &surrealdb::types::RecordId) -> String {
    match &rid.key {
        surrealdb::types::RecordIdKey::String(s) => s.clone(),
        other => format!("{other:?}"),
    }
}

/// Assemble the cite-or-refuse prompt from retrieved context.
fn build_ask_prompt(query: &str, sources: &[RetrievedSource], graph_facts: &[String]) -> String {
    let mut p = String::new();
    p.push_str(
        "You answer questions using ONLY the context from the user's notes below. \
         Cite the note each claim comes from inline using [[note path]] syntax. \
         If the context does not contain the answer, reply exactly: \
         \"I don't have that in your formation.\"\n\n",
    );
    p.push_str("=== NOTE EXCERPTS ===\n");
    if sources.is_empty() {
        p.push_str("(no relevant note excerpts found)\n");
    } else {
        for s in sources {
            p.push_str(&format!("[[{}]]\n{}\n\n", s.note_path, s.text));
        }
    }
    if !graph_facts.is_empty() {
        p.push_str("=== KNOWN FACTS ===\n");
        for f in graph_facts {
            p.push_str(&format!("- {f}\n"));
        }
        p.push('\n');
    }
    p.push_str("=== QUESTION ===\n");
    p.push_str(query);
    p.push_str("\n\n=== ANSWER ===\n");
    p
}
