//! Memory store commands. M-Pivot scope: just a smoke test that proves
//! SurrealDB can store and query a bi-temporal fact round-trip. Real
//! pipeline commands (insert_entity, relate_fact, query_facts, etc.) land
//! when graphrag-rs gets wired in.

use crate::commands::formation::APP_DIR;
use crate::core::formation_state::FormationState;
use crate::core::indexer::index_note_path;
use crate::core::memory::{ChunkHit, FactRow, FactWriteInput, MemoryHandle};
use crate::core::ollama_sidecar::{OllamaSidecar, DEFAULT_EMBED_MODEL};
use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use tauri::State;

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
/// the note's rows in SurrealDB. Idempotent. Thin wrapper over the shared
/// `core::indexer::index_note_path` so the on-demand command and the
/// background auto-indexer share one code path.
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
    let chunk_count = index_note_path(&formation_root, store, &sidecar, &relative_path).await?;
    Ok(IndexNoteResult {
        note_path: relative_path,
        chunk_count,
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

#[derive(Debug, Clone, Serialize)]
pub struct IndexProgress {
    pub done: usize,
    pub total: usize,
    pub current_path: String,
}

#[derive(Debug, Serialize)]
pub struct IndexFormationResult {
    pub total: usize,
    pub indexed: usize,
    pub skipped: usize,
    pub failed: usize,
}

/// Walk the open formation and index every `.md` file. Files whose mtime has
/// not advanced past their recorded index-state are skipped unless `force`.
/// Emits `index-progress` events so the UI can show a bar.
#[tauri::command]
pub async fn index_formation(
    force: bool,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
    sidecar: State<'_, OllamaSidecar>,
    app: tauri::AppHandle,
) -> AppResult<IndexFormationResult> {
    use tauri::Emitter;

    let formation_root = formation.require()?;
    let memory_dir = formation_root.join(APP_DIR).join("memory");
    let store = memory.get_or_init(&memory_dir).await?;

    let notes = crate::commands::formation::walk_notes(&formation_root)?;
    let total = notes.len();
    let mut indexed = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;

    for (i, note) in notes.iter().enumerate() {
        let needs_index = force
            || match store.indexed_mtime(&note.relative_path).await? {
                Some(recorded) => note.modified_secs > recorded,
                None => true,
            };
        if !needs_index {
            skipped += 1;
            continue;
        }
        let _ = app.emit(
            "index-progress",
            IndexProgress {
                done: i,
                total,
                current_path: note.relative_path.clone(),
            },
        );
        match index_note_path(&formation_root, store, &sidecar, &note.relative_path).await {
            Ok(_) => indexed += 1,
            Err(e) => {
                tracing::warn!("index_formation: {} failed: {e}", note.relative_path);
                failed += 1;
            }
        }
    }

    // Final 100% tick so the UI can clear its progress affordance.
    let _ = app.emit(
        "index-progress",
        IndexProgress {
            done: total,
            total,
            current_path: String::new(),
        },
    );

    Ok(IndexFormationResult {
        total,
        indexed,
        skipped,
        failed,
    })
}

/// JS-facing payload for `relate_fact_command`. Mirrors `FactWriteInput` but
/// expresses time as ISO-8601 strings since serde_json round-trips DateTime
/// through RFC3339 anyway and this is friendlier to construct from TS.
#[derive(Debug, Deserialize)]
pub struct RelateFactPayload {
    pub subject_id: String,
    pub predicate: String,
    pub object_id: String,
    /// ISO-8601 / RFC3339. If `None`, the server uses `now()`.
    #[serde(default)]
    pub valid_from: Option<String>,
    pub source_chat_id: String,
    #[serde(default = "default_confidence")]
    pub confidence: f64,
}

fn default_confidence() -> f64 {
    1.0
}

#[derive(Debug, Serialize)]
pub struct RelateFactResult {
    pub fact_id: String,
}

/// Write a bi-temporal fact edge with supersession. JS-callable wrapper over
/// `MemoryStore::relate_fact`. Returns the new fact's record id as a string.
#[tauri::command]
pub async fn relate_fact_command(
    payload: RelateFactPayload,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
) -> AppResult<RelateFactResult> {
    let formation_root = formation.require()?;
    let memory_dir = formation_root.join(APP_DIR).join("memory");
    let store = memory.get_or_init(&memory_dir).await?;

    let valid_from = match payload.valid_from {
        Some(s) => chrono::DateTime::parse_from_rfc3339(&s)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .map_err(|e| AppError::other(format!("parse valid_from: {e}")))?,
        None => chrono::Utc::now(),
    };

    let fact_id = store
        .relate_fact(FactWriteInput {
            subject_id: payload.subject_id,
            predicate: payload.predicate,
            object_id: payload.object_id,
            valid_from,
            source_chat_id: payload.source_chat_id,
            confidence: payload.confidence,
        })
        .await?;
    Ok(RelateFactResult { fact_id })
}

/// All currently-valid facts about a subject (valid_to IS NONE).
#[tauri::command]
pub async fn current_facts(
    subject_id: String,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
) -> AppResult<Vec<FactRow>> {
    let formation_root = formation.require()?;
    let memory_dir = formation_root.join(APP_DIR).join("memory");
    let store = memory.get_or_init(&memory_dir).await?;
    store.current_facts(&subject_id).await
}
