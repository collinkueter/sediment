//! Memory store commands. M-Pivot scope: just a smoke test that proves
//! SurrealDB can store and query a bi-temporal fact round-trip. Real
//! pipeline commands (insert_entity, relate_fact, query_facts, etc.) land
//! when graphrag-rs gets wired in.

use crate::commands::formation::APP_DIR;
use crate::core::formation_state::FormationState;
use crate::core::memory::{ChunkHit, MemoryHandle, NoteChunkInput};
use crate::core::ollama_sidecar::{OllamaSidecar, DEFAULT_EMBED_MODEL};
use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use tauri::State;

/// Max chars per chunk before we split. nomic-embed-text caps near 8K but real
/// recall on smaller windows is empirically better.
const CHUNK_MAX_CHARS: usize = 1500;

/// Smoke-test the embedded SurrealDB: write two entities, RELATE a fact
/// edge with a closed validity window, RELATE a second fact starting where
/// the first ended, then run both a "current" query and a point-in-time
/// query. Returns the resolved employer in each case so the round-trip is
/// observable from the UI.
#[tauri::command]
pub async fn memory_smoke_test(
    state: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
) -> AppResult<SmokeTestResult> {
    let formation = state.require()?;
    let memory_dir = formation.join(".chat-notes").join("memory");
    let store = memory.get_or_init(&memory_dir).await?;
    let db = store.handle();

    // Reset the smoke-test rows so this command is idempotent across runs.
    db.query(
        "DELETE fact WHERE source_chat_id IN ['smoke:msg_001','smoke:msg_017'];
         DELETE entity:smoke_john;
         DELETE entity:smoke_acme;
         DELETE entity:smoke_beta;",
    )
    .await
    .map_err(|e| AppError::other(format!("smoke reset: {e}")))?
    .check()
    .map_err(|e| AppError::other(format!("smoke reset check: {e}")))?;

    // Seed three entities.
    db.query(
        r#"
        CREATE entity:smoke_john   SET entity_type='person',       canonical_name='John Smith';
        CREATE entity:smoke_acme   SET entity_type='organization', canonical_name='Acme Corp';
        CREATE entity:smoke_beta   SET entity_type='organization', canonical_name='Beta Corp';
        "#,
    )
    .await
    .map_err(|e| AppError::other(format!("smoke seed entities: {e}")))?
    .check()
    .map_err(|e| AppError::other(format!("smoke seed entities check: {e}")))?;

    // RELATE the two `works_at` edges with overlapping-but-disjoint validity windows.
    db.query(
        r#"
        RELATE entity:smoke_john->fact->entity:smoke_acme
            SET predicate='works_at',
                valid_from=d'2024-01-01T00:00:00Z',
                valid_to=d'2026-03-15T00:00:00Z',
                source_chat_id='smoke:msg_001';

        RELATE entity:smoke_john->fact->entity:smoke_beta
            SET predicate='works_at',
                valid_from=d'2026-03-15T00:00:00Z',
                valid_to=NONE,
                source_chat_id='smoke:msg_017';
        "#,
    )
    .await
    .map_err(|e| AppError::other(format!("smoke RELATE: {e}")))?
    .check()
    .map_err(|e| AppError::other(format!("smoke RELATE check: {e}")))?;

    // Current employer (valid_to IS NONE) → expect Beta Corp.
    let current_name: Option<String> = {
        let mut res = db
            .query(
                r#"
                SELECT canonical_name FROM (
                    SELECT out FROM fact
                    WHERE in = entity:smoke_john
                      AND predicate = 'works_at'
                      AND valid_to IS NONE
                ).out;
                "#,
            )
            .await
            .map_err(|e| AppError::other(format!("smoke query current: {e}")))?;
        res.take::<Vec<String>>((0, "canonical_name"))
            .map_err(|e| AppError::other(format!("smoke take current: {e}")))?
            .into_iter()
            .next()
    };

    // Point-in-time: 2024-06-01 → expect Acme Corp.
    let historical_name: Option<String> = {
        let mut res = db
            .query(
                r#"
                LET $ts = d'2024-06-01T00:00:00Z';
                SELECT canonical_name FROM (
                    SELECT out FROM fact
                    WHERE in = entity:smoke_john
                      AND predicate = 'works_at'
                      AND valid_from <= $ts
                      AND (valid_to IS NONE OR valid_to > $ts)
                ).out;
                "#,
            )
            .await
            .map_err(|e| AppError::other(format!("smoke query historical: {e}")))?;
        res.take::<Vec<String>>((1, "canonical_name"))
            .map_err(|e| AppError::other(format!("smoke take historical: {e}")))?
            .into_iter()
            .next()
    };

    // Total fact-edge count for the round-trip integrity check.
    let fact_count: Option<i64> = {
        let mut res = db
            .query("SELECT count() AS c FROM fact WHERE in = entity:smoke_john GROUP ALL;")
            .await
            .map_err(|e| AppError::other(format!("smoke count: {e}")))?;
        res.take::<Vec<i64>>((0, "c"))
            .map_err(|e| AppError::other(format!("smoke take count: {e}")))?
            .into_iter()
            .next()
    };

    let ok = current_name.as_deref() == Some("Beta Corp")
        && historical_name.as_deref() == Some("Acme Corp")
        && fact_count == Some(2);
    Ok(SmokeTestResult {
        ok,
        current_employer: current_name.unwrap_or_default(),
        historical_employer: historical_name.unwrap_or_default(),
        fact_count: fact_count.unwrap_or(0),
    })
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SmokeTestResult {
    pub ok: bool,
    pub current_employer: String,
    pub historical_employer: String,
    pub fact_count: i64,
}

/// Read a note from disk, chunk it, embed each chunk via Ollama, and replace
/// the note's rows in SurrealDB. Idempotent — calling twice produces the same
/// result.
#[tauri::command]
pub async fn index_note(
    relative_path: String,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
    sidecar: State<'_, OllamaSidecar>,
) -> AppResult<IndexNoteResult> {
    let formation_root = formation.require()?;
    let memory_dir = formation_root.join(APP_DIR).join("memory");
    let store = memory.get_or_init(&memory_dir).await?;

    let abs = formation_root.join(&relative_path);
    let content = std::fs::read_to_string(&abs)
        .map_err(|e| AppError::other(format!("read {}: {e}", abs.display())))?;

    let chunks = chunk_markdown(&content);
    let mut inputs = Vec::with_capacity(chunks.len());
    for (idx, text) in chunks.iter().enumerate() {
        let embedding = sidecar.embed(DEFAULT_EMBED_MODEL, text).await?;
        inputs.push(NoteChunkInput {
            note_path: relative_path.clone(),
            chunk_idx: idx as i64,
            text: text.clone(),
            embedding,
        });
    }
    let count = inputs.len();
    store.replace_note_chunks(&relative_path, inputs).await?;
    Ok(IndexNoteResult {
        note_path: relative_path,
        chunk_count: count,
    })
}

/// Embed `query` and return the top-K most similar note chunks.
#[tauri::command]
pub async fn search_notes(
    query: String,
    k: Option<usize>,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
    sidecar: State<'_, OllamaSidecar>,
) -> AppResult<Vec<ChunkHit>> {
    let formation_root = formation.require()?;
    let memory_dir = formation_root.join(APP_DIR).join("memory");
    let store = memory.get_or_init(&memory_dir).await?;
    let embedding = sidecar.embed(DEFAULT_EMBED_MODEL, &query).await?;
    store.search_chunks(embedding, k.unwrap_or(5)).await
}

#[derive(Debug, Serialize)]
pub struct IndexNoteResult {
    pub note_path: String,
    pub chunk_count: usize,
}

/// Split markdown into chunks of `CHUNK_MAX_CHARS` or less, preferring
/// paragraph breaks. Conservative for Phase 1 — Phase 2+ can swap in a
/// markdown-aware splitter that respects headings + code fences.
fn chunk_markdown(content: &str) -> Vec<String> {
    let paragraphs: Vec<&str> = content
        .split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for para in paragraphs {
        if !current.is_empty() && current.len() + para.len() + 2 > CHUNK_MAX_CHARS {
            out.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        if para.len() > CHUNK_MAX_CHARS {
            // Hard-split very long paragraphs at char boundaries.
            for slice in para.as_bytes().chunks(CHUNK_MAX_CHARS) {
                let s = std::str::from_utf8(slice).unwrap_or("");
                if !s.is_empty() {
                    out.push(s.to_string());
                }
            }
        } else {
            current.push_str(para);
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}
