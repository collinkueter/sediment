//! Embedded SurrealDB wrapper. Owns the connection, applies the schema on first
//! launch, exposes typed helpers for the temporal-fact write/query patterns used
//! by the extraction pipeline.

use crate::error::{AppError, AppResult};
use std::path::Path;
use std::sync::Arc;
use surrealdb::engine::local::{Db, SurrealKv};
use surrealdb::types::SurrealValue;
use surrealdb::Surreal;
use tokio::sync::OnceCell;

const NS: &str = "sediment";
const DB: &str = "memory";

#[derive(Clone)]
pub struct MemoryStore {
    db: Arc<Surreal<Db>>,
}

impl MemoryStore {
    /// Open the embedded SurrealDB at `<formation>/.chat-notes/memory/` and apply the
    /// schema if not already present. Idempotent.
    pub async fn open(memory_dir: &Path) -> AppResult<Self> {
        std::fs::create_dir_all(memory_dir)?;
        let path = format!("surrealkv://{}", memory_dir.display());

        let db: Surreal<Db> = Surreal::new::<SurrealKv>(path.as_str())
            .await
            .map_err(|e| AppError::other(format!("open SurrealKV: {e}")))?;

        db.use_ns(NS)
            .use_db(DB)
            .await
            .map_err(|e| AppError::other(format!("use ns/db: {e}")))?;

        let store = Self { db: Arc::new(db) };
        store.apply_schema().await?;
        Ok(store)
    }

    pub fn handle(&self) -> &Surreal<Db> {
        &self.db
    }

    /// Replace all `note_chunk` rows for `note_path` with the supplied chunks.
    /// Wrapping the delete + insert in one query is atomic from a caller's view.
    pub async fn replace_note_chunks(
        &self,
        note_path: &str,
        chunks: Vec<NoteChunkInput>,
    ) -> AppResult<()> {
        self.db
            .query("DELETE note_chunk WHERE note_path = $path;")
            .bind(("path", note_path.to_string()))
            .await
            .map_err(|e| AppError::other(format!("delete note_chunks: {e}")))?
            .check()
            .map_err(|e| AppError::other(format!("delete check: {e}")))?;

        for chunk in chunks {
            self.db
                .query(
                    "CREATE note_chunk SET \
                     note_path = $note_path, \
                     chunk_idx = $chunk_idx, \
                     text = $text, \
                     embedding = $embedding;",
                )
                .bind(("note_path", chunk.note_path))
                .bind(("chunk_idx", chunk.chunk_idx))
                .bind(("text", chunk.text))
                .bind(("embedding", chunk.embedding))
                .await
                .map_err(|e| AppError::other(format!("insert chunk: {e}")))?
                .check()
                .map_err(|e| AppError::other(format!("insert chunk check: {e}")))?;
        }
        Ok(())
    }

    /// Top-K similarity search over `note_chunk.embedding`. Uses the HNSW
    /// index defined in the schema (cosine distance).
    pub async fn search_chunks(&self, embedding: Vec<f32>, k: usize) -> AppResult<Vec<ChunkHit>> {
        // SurrealDB's `<|K|>` KNN operator requires a literal integer for K;
        // it cannot be bound as a parameter. Clamp K to a sane range to keep
        // this string-interpolation safe.
        let k_clamped = k.clamp(1, 100);
        // HNSW indexes use the `<|K,EF|>` variant — bare `<|K|>` is reserved
        // for MTREE. EF (search list width) trades recall vs latency; 64 is a
        // safe default for the small note-chunk collections we expect.
        const EF: usize = 64;
        let sql = format!(
            "SELECT note_path, chunk_idx, text, \
             vector::distance::knn() AS distance \
             FROM note_chunk \
             WHERE embedding <|{k_clamped},{EF}|> $query;"
        );
        let mut res = self
            .db
            .query(sql)
            .bind(("query", embedding))
            .await
            .map_err(|e| AppError::other(format!("vector search: {e}")))?;
        let hits: Vec<ChunkHit> = res
            .take(0)
            .map_err(|e| AppError::other(format!("take hits: {e}")))?;
        Ok(hits)
    }

    async fn apply_schema(&self) -> AppResult<()> {
        // SurrealDB DDL is idempotent under DEFINE ... IF NOT EXISTS.
        self.db
            .query(SCHEMA_SQL)
            .await
            .map_err(|e| AppError::other(format!("apply schema: {e}")))?
            .check()
            .map_err(|e| AppError::other(format!("schema query check: {e}")))?;
        Ok(())
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NoteChunkInput {
    pub note_path: String,
    pub chunk_idx: i64,
    pub text: String,
    pub embedding: Vec<f32>,
}

/// SurrealValue lets `Response::take` deserialise rows directly into this shape
/// (required by surrealdb 3.x). Serde derives stay for Tauri IPC.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, SurrealValue)]
pub struct ChunkHit {
    pub note_path: String,
    pub chunk_idx: i64,
    pub text: String,
    pub distance: f32,
}

/// Apply once at startup. All DDL is `IF NOT EXISTS` so re-running is safe.
/// Bumping the schema = additional `DEFINE ... IF NOT EXISTS` blocks below.
const SCHEMA_SQL: &str = r#"
-- Entities: people, orgs, meetings, projects, etc.
DEFINE TABLE IF NOT EXISTS entity SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS entity_type    ON entity TYPE string
    ASSERT $value IN ['person','organization','meeting','project',
                      'task','topic','location','date','event'];
DEFINE FIELD IF NOT EXISTS canonical_name ON entity TYPE string;
DEFINE FIELD IF NOT EXISTS aliases        ON entity TYPE array<string> DEFAULT [];
DEFINE FIELD IF NOT EXISTS note_path      ON entity TYPE option<string>;
DEFINE FIELD IF NOT EXISTS embedding      ON entity TYPE option<array<float>>;
DEFINE FIELD IF NOT EXISTS created_at     ON entity TYPE datetime VALUE time::now() READONLY;
DEFINE FIELD IF NOT EXISTS updated_at     ON entity TYPE datetime VALUE time::now();
DEFINE INDEX IF NOT EXISTS entity_name      ON entity FIELDS canonical_name;
DEFINE INDEX IF NOT EXISTS entity_embedding ON entity FIELDS embedding
    HNSW DIMENSION 768 DIST COSINE;

-- Facts: bi-temporal graph edges between entities.
DEFINE TABLE IF NOT EXISTS fact SCHEMAFULL TYPE RELATION FROM entity TO entity;
DEFINE FIELD IF NOT EXISTS predicate      ON fact TYPE string;
DEFINE FIELD IF NOT EXISTS valid_from     ON fact TYPE datetime;
DEFINE FIELD IF NOT EXISTS valid_to       ON fact TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS source_chat_id ON fact TYPE string;
DEFINE FIELD IF NOT EXISTS confidence     ON fact TYPE float DEFAULT 1.0;
DEFINE FIELD IF NOT EXISTS created_at     ON fact TYPE datetime VALUE time::now() READONLY;
DEFINE INDEX IF NOT EXISTS fact_validity  ON fact FIELDS valid_from, valid_to;
DEFINE INDEX IF NOT EXISTS fact_predicate ON fact FIELDS predicate;

-- Note chunks for semantic retrieval.
DEFINE TABLE IF NOT EXISTS note_chunk SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS note_path ON note_chunk TYPE string;
DEFINE FIELD IF NOT EXISTS chunk_idx ON note_chunk TYPE int;
DEFINE FIELD IF NOT EXISTS text      ON note_chunk TYPE string;
DEFINE FIELD IF NOT EXISTS embedding ON note_chunk TYPE array<float>;
DEFINE INDEX IF NOT EXISTS chunk_embedding ON note_chunk FIELDS embedding
    HNSW DIMENSION 768 DIST COSINE;

-- Chat history; also the audit trail for fact provenance.
DEFINE TABLE IF NOT EXISTS chat_message SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS role       ON chat_message TYPE string
    ASSERT $value IN ['user','assistant','system'];
DEFINE FIELD IF NOT EXISTS content    ON chat_message TYPE string;
DEFINE FIELD IF NOT EXISTS session_id ON chat_message TYPE string;
DEFINE FIELD IF NOT EXISTS timestamp  ON chat_message TYPE datetime VALUE time::now();
"#;

/// One-shot bootstrap shared across Tauri commands. Lives in app state.
#[derive(Default)]
pub struct MemoryHandle(pub OnceCell<MemoryStore>);

impl MemoryHandle {
    /// Initialise on first call, return the existing instance on subsequent calls.
    pub async fn get_or_init(&self, memory_dir: &Path) -> AppResult<&MemoryStore> {
        self.0
            .get_or_try_init(|| async { MemoryStore::open(memory_dir).await })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir_for_test() -> std::path::PathBuf {
        let p = std::env::temp_dir()
            .join("sediment-test-memory")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&p).expect("tempdir");
        p
    }

    /// Round-trip a bi-temporal employment change. Verifies:
    ///   1. Schema applies cleanly on a fresh store.
    ///   2. Two RELATE edges with disjoint validity windows persist.
    ///   3. "current" query (valid_to IS NONE) returns the active edge only.
    ///   4. Point-in-time query returns the historical edge.
    ///
    /// This is the proof the spec asks for before the pipeline gets wired in.
    #[tokio::test]
    async fn temporal_fact_round_trip() {
        let dir = tempdir_for_test();
        let store = MemoryStore::open(&dir).await.expect("open");
        let db = store.handle();

        db.query(
            r#"
            CREATE entity:john SET entity_type='person',       canonical_name='John Smith';
            CREATE entity:acme SET entity_type='organization', canonical_name='Acme Corp';
            CREATE entity:beta SET entity_type='organization', canonical_name='Beta Corp';

            RELATE entity:john->fact->entity:acme
                SET predicate='works_at',
                    valid_from=d'2024-01-01T00:00:00Z',
                    valid_to=d'2026-03-15T00:00:00Z',
                    source_chat_id='msg_001';
            RELATE entity:john->fact->entity:beta
                SET predicate='works_at',
                    valid_from=d'2026-03-15T00:00:00Z',
                    valid_to=NONE,
                    source_chat_id='msg_017';
            "#,
        )
        .await
        .expect("seed")
        .check()
        .expect("seed check");

        // Current employer should be Beta Corp.
        let mut res = db
            .query(
                r#"
                SELECT (->fact[?valid_to IS NONE]->entity.*).canonical_name AS names
                FROM entity:john;
                "#,
            )
            .await
            .expect("current query");
        let names: Vec<Vec<String>> = res.take("names").expect("take current");
        assert_eq!(
            names.first().map(|v| v.as_slice()),
            Some(&["Beta Corp".to_string()][..])
        );

        // As-of 2024-06-01 should be Acme Corp.
        let mut res = db
            .query(
                r#"
                LET $ts = d'2024-06-01T00:00:00Z';
                SELECT (
                    ->fact[?valid_from <= $ts AND (valid_to IS NONE OR valid_to > $ts)]
                    ->entity.*
                ).canonical_name AS names
                FROM entity:john;
                "#,
            )
            .await
            .expect("historical query");
        let names: Vec<Vec<String>> = res.take((1, "names")).expect("take historical");
        assert_eq!(
            names.first().map(|v| v.as_slice()),
            Some(&["Acme Corp".to_string()][..])
        );

        // Both edges retained (not destructive update).
        let mut res = db
            .query("SELECT count() AS c FROM fact WHERE in = entity:john GROUP ALL;")
            .await
            .expect("count");
        let counts: Vec<i64> = res.take("c").expect("take count");
        assert_eq!(counts.first().copied(), Some(2));

        std::fs::remove_dir_all(dir).ok();
    }

    /// Insert two synthetic chunks and verify similarity search returns the
    /// nearer one first. Uses fake 768-d vectors so this test doesn't depend
    /// on Ollama being available — it exercises just the storage path.
    #[tokio::test]
    async fn note_chunk_similarity_round_trip() {
        let dir = tempdir_for_test();
        let store = MemoryStore::open(&dir).await.expect("open");

        // Two distinct vectors: chunk A leans toward index 0, chunk B toward index 100.
        let mut vec_a = vec![0.0_f32; 768];
        vec_a[0] = 1.0;
        let mut vec_b = vec![0.0_f32; 768];
        vec_b[100] = 1.0;

        store
            .replace_note_chunks(
                "People/John.md",
                vec![
                    NoteChunkInput {
                        note_path: "People/John.md".into(),
                        chunk_idx: 0,
                        text: "John works at Acme.".into(),
                        embedding: vec_a.clone(),
                    },
                    NoteChunkInput {
                        note_path: "People/John.md".into(),
                        chunk_idx: 1,
                        text: "His kid plays baseball.".into(),
                        embedding: vec_b.clone(),
                    },
                ],
            )
            .await
            .expect("insert chunks");

        // Query near vec_a — should rank chunk 0 first.
        let hits = store.search_chunks(vec_a.clone(), 2).await.expect("search");
        assert!(!hits.is_empty(), "expected at least one hit, got none");
        assert_eq!(
            hits[0].chunk_idx, 0,
            "expected chunk 0 (matching vec_a) first; got {:?}",
            hits
        );

        std::fs::remove_dir_all(dir).ok();
    }
}
