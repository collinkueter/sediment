//! Chat-driven commands.
//!
//! - `chat_write`: a Write-mode message is persisted and run through the
//!   extraction pipeline; its facts are routed, diffed, and written to a
//!   staging entry for review — nothing touches SurrealDB until a Keep
//!   (spec principle #3, "AI proposes, human disposes").
//! - `chat_ask`: an Ask-mode question runs hybrid retrieval (vector search
//!   over note chunks + best-effort graph facts) and streams a cited answer.

use crate::commands::extraction::{
    extract_facts_only, MIN_ENTITY_CONFIDENCE, MIN_RELATION_CONFIDENCE,
};
use crate::commands::formation::APP_DIR;
use crate::core::diff_gen::apply_facts_to_note;
use crate::core::extraction::{
    default_relation_schema, extract_entities_and_relations, EntitySpan, GlinerExtractor,
    ModelPaths, ENTITY_LABELS,
};
use crate::core::formation_state::FormationState;
use crate::core::memory::{slugify, ChunkHit, MemoryHandle, MemoryStore};
use crate::core::ollama_sidecar::{OllamaSidecar, DEFAULT_EMBED_MODEL};
use crate::core::router::route_fact;
use crate::core::staging::{self, StagedFact, StagingEntry};
use crate::error::{AppError, AppResult};
use futures::StreamExt;
use ollama_rs::generation::completion::request::GenerationRequest;
use serde::Serialize;
use std::collections::HashMap;
use tauri::ipc::Channel;
use tauri::{Emitter, State};

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
    /// The staging entry created for review, or `None` when the message
    /// yielded no new facts.
    pub staged: Option<StagingEntry>,
}

/// Write-mode chat turn. Persists the user's message, extracts entities and
/// relations, routes each fact to its subject entity's note, renders the
/// resulting markdown diffs, and writes them to a staging entry. **Nothing is
/// committed to SurrealDB or to note files** — the user reviews and Keeps the
/// entry from the staging tray (see `commands::staging`).
#[tauri::command]
pub async fn chat_write(
    message: String,
    session_id: String,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
    app: tauri::AppHandle,
) -> AppResult<ChatWriteResult> {
    let formation_root = formation.require()?;
    let memory_dir = formation_root.join(APP_DIR).join("memory");
    let store = memory.get_or_init(&memory_dir).await?;

    // Persist the user message first so staged facts can cite it.
    let source_chat_id = store
        .insert_chat_message("user", &message, &session_id)
        .await?;

    // Extract entities + relations. This makes no SurrealDB writes.
    let (entities, relations) = extract_facts_only(&message, &formation_root)?;

    // Entities above the confidence floor, keyed by surface text.
    let mut entity_by_name: HashMap<String, EntitySpan> = HashMap::new();
    for span in entities {
        if span.probability < MIN_ENTITY_CONFIDENCE {
            continue;
        }
        entity_by_name.entry(span.text.clone()).or_insert(span);
    }

    // Resolve each relation into a StagedFact, grouped by its target note.
    let valid_from = chrono::Utc::now();
    let mut by_note: Vec<(String, Vec<StagedFact>)> = Vec::new();
    for rel in relations {
        if rel.probability < MIN_RELATION_CONFIDENCE {
            continue;
        }
        let (Some(subj), Some(obj)) = (
            entity_by_name.get(&rel.subject),
            entity_by_name.get(&rel.object),
        ) else {
            continue; // RE proposed an entity NER did not surface
        };
        let subject = resolve_entity(store, &subj.text, &subj.class).await?;
        let object = resolve_entity(store, &obj.text, &obj.class).await?;
        let (note_path, _) = route_fact(
            &subject.entity_type,
            &subject.name,
            subject.note_path.as_deref(),
        );
        let fact = StagedFact {
            subject_id: subject.id,
            subject_name: subject.name,
            subject_type: subject.entity_type,
            predicate: rel.predicate,
            object_id: object.id,
            object_name: object.name,
            object_type: object.entity_type,
            valid_from,
            valid_from_explicit: false,
            confidence: rel.probability as f64,
            explicit_coexist: false,
        };
        match by_note.iter_mut().find(|(p, _)| p == &note_path) {
            Some((_, facts)) => facts.push(fact),
            None => by_note.push((note_path, vec![fact])),
        }
    }

    // Render the markdown diff for each affected note. `apply_facts_to_note`
    // drops facts already filed, so a fully-idempotent message stages nothing.
    let mut changes = Vec::new();
    for (note_path, facts) in by_note {
        let existing = std::fs::read_to_string(formation_root.join(&note_path)).ok();
        let change = apply_facts_to_note(&note_path, existing.as_deref(), &facts, &source_chat_id);
        if !change.facts.is_empty() {
            changes.push(change);
        }
    }

    if changes.is_empty() {
        return Ok(ChatWriteResult {
            source_chat_id,
            staged: None,
        });
    }

    let entry = StagingEntry {
        id: StagingEntry::new_id(),
        created: chrono::Utc::now(),
        chat_message_id: source_chat_id.clone(),
        chat_excerpt: excerpt(&message),
        status: "pending".to_string(),
        changes,
    };
    staging::write(&formation_root.join(APP_DIR).join("staging"), &entry)?;
    if let Err(e) = app.emit("staging-created", &entry) {
        tracing::warn!("emit staging-created failed: {e}");
    }

    Ok(ChatWriteResult {
        source_chat_id,
        staged: Some(entry),
    })
}

/// A subject/object entity resolved against the graph. Entities not yet stored
/// get a locally-derived slug id and no note path — the commit step upserts
/// them for real and resolves the authoritative id.
struct ResolvedEntity {
    id: String,
    name: String,
    entity_type: String,
    note_path: Option<String>,
}

/// Resolve an extracted entity span against the graph without writing. Falls
/// back to the NER surface text + class for entities not yet stored.
async fn resolve_entity(store: &MemoryStore, text: &str, class: &str) -> AppResult<ResolvedEntity> {
    match store.lookup_entity(text).await? {
        Some(found) => Ok(ResolvedEntity {
            id: found.id,
            name: found.canonical_name,
            entity_type: found.entity_type,
            note_path: found.note_path,
        }),
        None => Ok(ResolvedEntity {
            id: format!("entity:{}", slugify(text)),
            name: text.to_string(),
            entity_type: class.to_string(),
            note_path: None,
        }),
    }
}

/// First ~140 characters of the message, for the staging tray header.
fn excerpt(message: &str) -> String {
    const MAX: usize = 140;
    let trimmed = message.trim();
    if trimmed.chars().count() <= MAX {
        trimmed.to_string()
    } else {
        let head: String = trimmed.chars().take(MAX).collect();
        format!("{head}…")
    }
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
