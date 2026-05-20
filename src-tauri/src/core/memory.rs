//! Embedded SurrealDB wrapper. Owns the connection, applies the schema on first
//! launch, exposes typed helpers for the temporal-fact write/query patterns used
//! by the extraction pipeline.

use crate::error::{AppError, AppResult};
use std::path::Path;
use std::sync::Arc;
use surrealdb::engine::local::{Db, SurrealKv};
use surrealdb::types::{RecordId, RecordIdKey, SurrealValue};
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

    /// Upsert an entity by canonical_name + aliases.
    ///
    /// Resolution order:
    /// 1. Match on `canonical_name = $name`.
    /// 2. Match on `$name IN aliases`.
    ///
    /// On hit, merge any new aliases. On miss, CREATE with a slug-derived id
    /// (`entity:bill_gates`), suffixed `_2`/`_3`... if the slug collides with
    /// an unrelated entity. Idempotent for repeat calls with the same name.
    pub async fn upsert_entity(
        &self,
        canonical_name: &str,
        entity_type: &str,
        aliases: Vec<String>,
    ) -> AppResult<UpsertedEntity> {
        // 1. Lookup by canonical_name OR alias.
        let mut res = self
            .db
            .query(
                "SELECT id, canonical_name, aliases FROM entity \
                 WHERE canonical_name = $name OR $name IN aliases;",
            )
            .bind(("name", canonical_name.to_string()))
            .await
            .map_err(|e| AppError::other(format!("upsert lookup: {e}")))?;
        let found: Vec<ExistingEntity> = res
            .take(0)
            .map_err(|e| AppError::other(format!("upsert take: {e}")))?;

        if let Some(existing) = found.into_iter().next() {
            // Merge new aliases (skipping the canonical_name itself and ones already present).
            let new_aliases: Vec<String> = aliases
                .into_iter()
                .filter(|a| a != &existing.canonical_name && !existing.aliases.contains(a))
                .collect();
            if !new_aliases.is_empty() {
                self.db
                    .query("UPDATE $id SET aliases += $extra;")
                    .bind(("id", existing.id.clone()))
                    .bind(("extra", new_aliases))
                    .await
                    .map_err(|e| AppError::other(format!("upsert merge: {e}")))?
                    .check()
                    .map_err(|e| AppError::other(format!("upsert merge check: {e}")))?;
            }
            return Ok(UpsertedEntity {
                id: record_id_to_string(&existing.id),
                canonical_name: existing.canonical_name,
                was_new: false,
            });
        }

        // 2. No existing entity — pick a free slug.
        let base_slug = slugify(canonical_name);
        if base_slug.is_empty() {
            return Err(AppError::other(format!(
                "cannot slugify empty entity name: {canonical_name:?}"
            )));
        }
        let final_slug = self.pick_available_slug(&base_slug).await?;
        // CREATE with literal slug in the record id (binding doesn't work for ids in DDL position).
        let sql = format!(
            "CREATE entity:{final_slug} SET \
             entity_type = $entity_type, \
             canonical_name = $canonical_name, \
             aliases = $aliases, \
             canonical_name_history = [];"
        );
        self.db
            .query(sql)
            .bind(("entity_type", entity_type.to_string()))
            .bind(("canonical_name", canonical_name.to_string()))
            .bind(("aliases", aliases))
            .await
            .map_err(|e| AppError::other(format!("create entity: {e}")))?
            .check()
            .map_err(|e| AppError::other(format!("create entity check: {e}")))?;

        Ok(UpsertedEntity {
            id: format!("entity:{final_slug}"),
            canonical_name: canonical_name.to_string(),
            was_new: true,
        })
    }

    /// Returns `base` if no row with that id exists, otherwise tries
    /// `base_2`, `base_3`, ... up to 9 attempts before giving up.
    async fn pick_available_slug(&self, base: &str) -> AppResult<String> {
        for n in 1..10 {
            let candidate = if n == 1 {
                base.to_string()
            } else {
                format!("{base}_{n}")
            };
            let exists = self.entity_exists(&candidate).await?;
            if !exists {
                return Ok(candidate);
            }
        }
        Err(AppError::other(format!(
            "could not find free slug for base {base:?} after 9 attempts"
        )))
    }

    async fn entity_exists(&self, slug: &str) -> AppResult<bool> {
        let sql = format!("SELECT id FROM entity:{slug};");
        let mut res = self
            .db
            .query(sql)
            .await
            .map_err(|e| AppError::other(format!("entity_exists: {e}")))?;
        let rows: Vec<IdRow> = res
            .take(0)
            .map_err(|e| AppError::other(format!("entity_exists take: {e}")))?;
        Ok(!rows.is_empty())
    }

    /// Write a bi-temporal fact edge from `subject_id` to `object_id`,
    /// superseding any contradicting current fact. Thin wrapper over
    /// `relate_fact_with` with supersession on.
    pub async fn relate_fact(&self, fact: FactWriteInput) -> AppResult<String> {
        self.relate_fact_with(fact, true).await
    }

    /// Write a bi-temporal fact edge, controlling supersession.
    ///
    /// With `supersede = true` (atomic, in one SurrealDB query):
    /// 1. Any existing fact with the same `(subject, predicate)`,
    ///    `valid_to IS NONE`, AND a different `object` gets its
    ///    `valid_to = $valid_from`. History is preserved; the older edge
    ///    stays queryable for point-in-time reads.
    /// 2. The new edge is RELATEd with `valid_to = NONE`.
    ///
    /// With `supersede = false`, step 1 is skipped — the new fact coexists
    /// with the contradicting current fact. This backs the "Keep both"
    /// conflict resolution (concurrent employment; ADR-0004 refinement R3).
    ///
    /// Returns the new fact's record id as a `"fact:..."` string.
    pub async fn relate_fact_with(
        &self,
        fact: FactWriteInput,
        supersede: bool,
    ) -> AppResult<String> {
        // Strip "entity:" prefix if present — we splice the raw key into the
        // RELATE statement to avoid binding-position limits.
        let subject_key = fact
            .subject_id
            .strip_prefix("entity:")
            .unwrap_or(&fact.subject_id);
        let object_key = fact
            .object_id
            .strip_prefix("entity:")
            .unwrap_or(&fact.object_id);

        let relate = format!(
            "RELATE entity:{subject_key} -> fact -> entity:{object_key} \
               SET predicate = $predicate, \
                   valid_from = $valid_from, \
                   valid_to = NONE, \
                   source_chat_id = $source_chat_id, \
                   confidence = $confidence;"
        );
        // With supersession the RELATE is the second statement; without it,
        // the first — `take` must address whichever index the RELATE lands at.
        let (sql, relate_idx) = if supersede {
            (
                format!(
                    "UPDATE fact SET valid_to = $valid_from \
                       WHERE in = entity:{subject_key} \
                         AND predicate = $predicate \
                         AND out != entity:{object_key} \
                         AND valid_to IS NONE; \
                     {relate}"
                ),
                1,
            )
        } else {
            (relate, 0)
        };

        let mut res = self
            .db
            .query(sql)
            .bind(("predicate", fact.predicate))
            .bind(("valid_from", fact.valid_from))
            .bind(("source_chat_id", fact.source_chat_id))
            .bind(("confidence", fact.confidence))
            .await
            .map_err(|e| AppError::other(format!("relate_fact: {e}")))?
            .check()
            .map_err(|e| AppError::other(format!("relate_fact check: {e}")))?;

        let rows: Vec<IdRow> = res
            .take(relate_idx)
            .map_err(|e| AppError::other(format!("relate_fact take new: {e}")))?;
        let fact_row = rows
            .into_iter()
            .next()
            .ok_or_else(|| AppError::other("RELATE returned no rows"))?;
        Ok(record_id_to_string(&fact_row.id))
    }

    /// Persist a chat message and return its record id (e.g. `chat_message:abc`).
    /// The id becomes the `source_chat_id` provenance pointer on extracted facts.
    pub async fn insert_chat_message(
        &self,
        role: &str,
        content: &str,
        session_id: &str,
    ) -> AppResult<String> {
        let mut res = self
            .db
            .query(
                "CREATE chat_message SET \
                 role = $role, content = $content, session_id = $sid;",
            )
            .bind(("role", role.to_string()))
            .bind(("content", content.to_string()))
            .bind(("sid", session_id.to_string()))
            .await
            .map_err(|e| AppError::other(format!("insert chat_message: {e}")))?
            .check()
            .map_err(|e| AppError::other(format!("insert chat_message check: {e}")))?;
        let rows: Vec<IdRow> = res
            .take(0)
            .map_err(|e| AppError::other(format!("insert chat_message take: {e}")))?;
        rows.into_iter()
            .next()
            .map(|r| record_id_to_string(&r.id))
            .ok_or_else(|| AppError::other("CREATE chat_message returned no row"))
    }

    /// Current facts about an entity: edges where `valid_to IS NONE`.
    pub async fn current_facts(&self, subject_id: &str) -> AppResult<Vec<FactRow>> {
        let key = subject_id.strip_prefix("entity:").unwrap_or(subject_id);
        let sql = format!(
            "SELECT id, in AS subject, out AS object, predicate, valid_from, valid_to, \
             source_chat_id, confidence \
             FROM fact \
             WHERE in = entity:{key} AND valid_to IS NONE;"
        );
        let mut res = self
            .db
            .query(sql)
            .await
            .map_err(|e| AppError::other(format!("current_facts: {e}")))?;
        res.take(0)
            .map_err(|e| AppError::other(format!("current_facts take: {e}")))
    }

    /// Resolve an entity by canonical name or alias **without writing**.
    /// Returns the stored record's id, type, canonical name, and `note_path`
    /// (if linked to a note). Used by the staging pipeline to decide whether a
    /// fact updates an existing note or creates a new one.
    pub async fn lookup_entity(&self, name: &str) -> AppResult<Option<EntityLookup>> {
        let mut res = self
            .db
            .query(
                "SELECT id, entity_type, canonical_name, note_path FROM entity \
                 WHERE canonical_name = $name OR $name IN aliases;",
            )
            .bind(("name", name.to_string()))
            .await
            .map_err(|e| AppError::other(format!("lookup_entity: {e}")))?;
        let rows: Vec<EntityRow> = res
            .take(0)
            .map_err(|e| AppError::other(format!("lookup_entity take: {e}")))?;
        Ok(rows.into_iter().next().map(|r| EntityLookup {
            id: record_id_to_string(&r.id),
            entity_type: r.entity_type,
            canonical_name: r.canonical_name,
            note_path: r.note_path,
        }))
    }

    /// Link an entity to a note by setting its `note_path`. Called on commit so
    /// a later fact about the same subject routes as an update of that note
    /// rather than creating a duplicate (Phase 3 decision #2).
    pub async fn set_entity_note_path(&self, entity_id: &str, note_path: &str) -> AppResult<()> {
        let key = entity_id.strip_prefix("entity:").unwrap_or(entity_id);
        self.db
            .query("UPDATE type::record('entity', $key) SET note_path = $path;")
            .bind(("key", key.to_string()))
            .bind(("path", note_path.to_string()))
            .await
            .map_err(|e| AppError::other(format!("set_entity_note_path: {e}")))?
            .check()
            .map_err(|e| AppError::other(format!("set_entity_note_path check: {e}")))?;
        Ok(())
    }

    /// Delete a fact edge by record id. `undo_commit` uses this to remove
    /// exactly the facts a Keep wrote — the ids are tracked, never re-derived.
    pub async fn delete_fact(&self, fact_id: &str) -> AppResult<()> {
        let key = fact_id.strip_prefix("fact:").unwrap_or(fact_id);
        self.db
            .query("DELETE type::record('fact', $key);")
            .bind(("key", key.to_string()))
            .await
            .map_err(|e| AppError::other(format!("delete_fact: {e}")))?
            .check()
            .map_err(|e| AppError::other(format!("delete_fact check: {e}")))?;
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

    /// Record that `note_path` was indexed at file-mtime `mtime_secs`. Used to
    /// skip unchanged files on a formation-wide re-index. Replaces any prior row.
    pub async fn record_index_state(&self, note_path: &str, mtime_secs: i64) -> AppResult<()> {
        self.db
            .query(
                "DELETE note_index_state WHERE note_path = $p; \
                 CREATE note_index_state SET note_path = $p, indexed_mtime = $m;",
            )
            .bind(("p", note_path.to_string()))
            .bind(("m", mtime_secs))
            .await
            .map_err(|e| AppError::other(format!("record index state: {e}")))?
            .check()
            .map_err(|e| AppError::other(format!("record index state check: {e}")))?;
        Ok(())
    }

    /// The file mtime (unix seconds) at which `note_path` was last indexed,
    /// or `None` if it has never been indexed.
    pub async fn indexed_mtime(&self, note_path: &str) -> AppResult<Option<i64>> {
        let mut res = self
            .db
            .query("SELECT indexed_mtime FROM note_index_state WHERE note_path = $p;")
            .bind(("p", note_path.to_string()))
            .await
            .map_err(|e| AppError::other(format!("indexed_mtime: {e}")))?;
        let rows: Vec<i64> = res
            .take((0, "indexed_mtime"))
            .map_err(|e| AppError::other(format!("indexed_mtime take: {e}")))?;
        Ok(rows.into_iter().next())
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

/// Surfaced to JS as a flat string id. The id stays stable across renames —
/// the canonical_name field can change; the slug-derived record id doesn't.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UpsertedEntity {
    /// Full SurrealDB record id, e.g. `entity:bill_gates`.
    pub id: String,
    pub canonical_name: String,
    /// `true` if this call created the row; `false` if it matched an existing row.
    pub was_new: bool,
}

#[derive(Debug, Clone, serde::Deserialize, SurrealValue)]
struct ExistingEntity {
    pub id: RecordId,
    pub canonical_name: String,
    pub aliases: Vec<String>,
}

/// Row shape for `lookup_entity` — the fields the staging pipeline needs.
#[derive(Debug, Clone, serde::Deserialize, SurrealValue)]
struct EntityRow {
    pub id: RecordId,
    pub entity_type: String,
    pub canonical_name: String,
    pub note_path: Option<String>,
}

/// JS/staging-facing entity resolution result. The `id` is the flat
/// `entity:<slug>` string; `note_path` is `Some` once the entity has been
/// filed into a note.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EntityLookup {
    pub id: String,
    pub entity_type: String,
    pub canonical_name: String,
    pub note_path: Option<String>,
}

/// Caller-supplied data for a fact write. Strings instead of typed enums
/// because the predicate vocabulary lives in `core::extraction` and may
/// expand without recompiling the storage layer.
#[derive(Debug, Clone)]
pub struct FactWriteInput {
    pub subject_id: String,
    pub predicate: String,
    pub object_id: String,
    pub valid_from: chrono::DateTime<chrono::Utc>,
    pub source_chat_id: String,
    pub confidence: f64,
}

/// Minimal row shape for queries that only need the record id back
/// (CREATE/RELATE returns, existence checks).
#[derive(Debug, Clone, serde::Deserialize, SurrealValue)]
struct IdRow {
    pub id: RecordId,
}

/// Shape returned by `current_facts` and similar fact queries. The SQL aliases
/// the relation table's implicit `in`/`out` columns to `subject`/`object` so
/// we sidestep raw-identifier deserialisation in the SurrealValue derive.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, SurrealValue)]
pub struct FactRow {
    pub id: RecordId,
    pub subject: RecordId,
    pub object: RecordId,
    pub predicate: String,
    pub valid_from: chrono::DateTime<chrono::Utc>,
    pub valid_to: Option<chrono::DateTime<chrono::Utc>>,
    pub source_chat_id: String,
    pub confidence: f64,
}

/// Format a SurrealDB RecordId as the `table:key` string we hand back to JS.
/// We only ever produce string-keyed ids in this app, so the other RecordIdKey
/// variants are best-effort fallbacks.
fn record_id_to_string(rid: &RecordId) -> String {
    let key = match &rid.key {
        RecordIdKey::String(s) => s.clone(),
        RecordIdKey::Number(n) => n.to_string(),
        other => format!("{other:?}"),
    };
    format!("{}:{}", rid.table.as_str(), key)
}

/// Stable slug for use as the SurrealDB record id after `entity:`. Lowercases,
/// collapses runs of non-alphanumerics into a single underscore, and trims.
/// `"Bill Gates"` → `"bill_gates"`; `"J.P. Morgan & Co."` → `"j_p_morgan_co"`.
pub fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_was_sep = true; // suppress leading separator
    for c in input.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_was_sep = false;
        } else if !last_was_sep {
            out.push('_');
            last_was_sep = true;
        }
    }
    out.trim_end_matches('_').to_string()
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
DEFINE FIELD IF NOT EXISTS canonical_name_history ON entity TYPE array<string> DEFAULT [];
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

-- Per-note index state: lets a formation-wide re-index skip unchanged files.
DEFINE TABLE IF NOT EXISTS note_index_state SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS note_path     ON note_index_state TYPE string;
DEFINE FIELD IF NOT EXISTS indexed_mtime ON note_index_state TYPE int;
DEFINE FIELD IF NOT EXISTS indexed_at    ON note_index_state TYPE datetime VALUE time::now();
DEFINE INDEX IF NOT EXISTS note_index_state_path ON note_index_state FIELDS note_path;

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

    /// End-to-end storage pipeline (P2-M8 integration check): persist a chat
    /// message, upsert entities, relate a fact citing that message, then
    /// supersede it. Verifies provenance and supersession chain together
    /// without needing the GLiNER or embedding models.
    #[tokio::test]
    async fn full_storage_pipeline_chat_to_superseded_fact() {
        use chrono::TimeZone;
        let dir = tempdir_for_test();
        let store = MemoryStore::open(&dir).await.expect("open");

        // 1. Persist the originating chat message.
        let chat_id = store
            .insert_chat_message("user", "Sarah is the CTO at Acme.", "sess-1")
            .await
            .expect("insert chat message");
        assert!(chat_id.starts_with("chat_message:"));

        // 2. Upsert the entities the extractor would have found.
        let sarah = store
            .upsert_entity("Sarah", "person", vec![])
            .await
            .expect("sarah")
            .id;
        let acme = store
            .upsert_entity("Acme", "organization", vec![])
            .await
            .expect("acme")
            .id;

        // 3. Relate the fact, citing the chat message as provenance.
        let t0 = chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        store
            .relate_fact(FactWriteInput {
                subject_id: sarah.clone(),
                predicate: "works_at".into(),
                object_id: acme.clone(),
                valid_from: t0,
                source_chat_id: chat_id.clone(),
                confidence: 0.95,
            })
            .await
            .expect("relate acme fact");

        // The current fact must cite the stored chat message.
        let current = store.current_facts(&sarah).await.expect("current");
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].source_chat_id, chat_id);

        // 4. A later message supersedes it.
        let chat_id2 = store
            .insert_chat_message("user", "Sarah moved to Beta Corp.", "sess-1")
            .await
            .expect("insert chat message 2");
        let beta = store
            .upsert_entity("Beta Corp", "organization", vec![])
            .await
            .expect("beta")
            .id;
        let t1 = chrono::Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap();
        store
            .relate_fact(FactWriteInput {
                subject_id: sarah.clone(),
                predicate: "works_at".into(),
                object_id: beta.clone(),
                valid_from: t1,
                source_chat_id: chat_id2.clone(),
                confidence: 0.95,
            })
            .await
            .expect("relate beta fact");

        // Current fact is now Beta, citing the second message; history kept.
        let current = store.current_facts(&sarah).await.expect("current 2");
        assert_eq!(current.len(), 1);
        assert_eq!(record_id_to_string(&current[0].object), beta);
        assert_eq!(current[0].source_chat_id, chat_id2);

        std::fs::remove_dir_all(dir).ok();
    }

    /// upsert_entity must be idempotent: same canonical_name → same id and
    /// `was_new = false` on the second call. New aliases on a second call
    /// must merge into the existing row.
    #[tokio::test]
    async fn entity_upsert_is_idempotent_and_merges_aliases() {
        let dir = tempdir_for_test();
        let store = MemoryStore::open(&dir).await.expect("open");

        let first = store
            .upsert_entity("Bill Gates", "person", vec!["Bill".into()])
            .await
            .expect("first upsert");
        assert!(first.was_new, "first call should create");
        assert_eq!(first.id, "entity:bill_gates");

        // Same canonical_name again — should match, not create.
        let second = store
            .upsert_entity("Bill Gates", "person", vec!["William".into()])
            .await
            .expect("second upsert");
        assert!(!second.was_new, "second call must reuse existing entity");
        assert_eq!(second.id, first.id);

        // Aliases merged: confirm William landed.
        let mut res = store
            .handle()
            .query("SELECT aliases FROM entity:bill_gates;")
            .await
            .expect("query aliases");
        let aliases: Vec<Vec<String>> = res.take("aliases").expect("take aliases");
        let first_row = aliases.first().expect("one row").clone();
        assert!(first_row.contains(&"Bill".to_string()));
        assert!(first_row.contains(&"William".to_string()));

        // Lookup by alias must resolve to the same id (third call uses alias only).
        let by_alias = store
            .upsert_entity("William", "person", vec![])
            .await
            .expect("alias upsert");
        assert!(!by_alias.was_new, "alias lookup must reuse entity");
        assert_eq!(by_alias.id, first.id);

        std::fs::remove_dir_all(dir).ok();
    }

    /// lookup_entity resolves by canonical name or alias and reports note_path.
    #[tokio::test]
    async fn lookup_entity_resolves_by_name_and_alias() {
        let dir = tempdir_for_test();
        let store = MemoryStore::open(&dir).await.expect("open");

        // Unknown entity → None.
        assert!(store.lookup_entity("Nobody").await.expect("miss").is_none());

        store
            .upsert_entity("Bill Gates", "person", vec!["Bill".into()])
            .await
            .expect("upsert");

        let by_name = store
            .lookup_entity("Bill Gates")
            .await
            .expect("by name")
            .expect("found");
        assert_eq!(by_name.id, "entity:bill_gates");
        assert_eq!(by_name.entity_type, "person");
        assert_eq!(by_name.canonical_name, "Bill Gates");
        assert!(by_name.note_path.is_none(), "fresh entity has no note yet");

        // Alias resolves to the same record.
        let by_alias = store
            .lookup_entity("Bill")
            .await
            .expect("by alias")
            .expect("found");
        assert_eq!(by_alias.id, by_name.id);

        std::fs::remove_dir_all(dir).ok();
    }

    /// set_entity_note_path links a note; delete_fact removes a single edge.
    #[tokio::test]
    async fn set_note_path_and_delete_fact() {
        use chrono::Utc;
        let dir = tempdir_for_test();
        let store = MemoryStore::open(&dir).await.expect("open");

        let john = store
            .upsert_entity("John", "person", vec![])
            .await
            .expect("john")
            .id;
        let acme = store
            .upsert_entity("Acme", "organization", vec![])
            .await
            .expect("acme")
            .id;

        store
            .set_entity_note_path(&john, "People/John.md")
            .await
            .expect("set note_path");
        let looked = store
            .lookup_entity("John")
            .await
            .expect("lookup")
            .expect("found");
        assert_eq!(looked.note_path.as_deref(), Some("People/John.md"));

        let fact_id = store
            .relate_fact(FactWriteInput {
                subject_id: john.clone(),
                predicate: "works_at".into(),
                object_id: acme.clone(),
                valid_from: Utc::now(),
                source_chat_id: "chat_message:x".into(),
                confidence: 0.9,
            })
            .await
            .expect("relate");
        assert_eq!(store.current_facts(&john).await.expect("current").len(), 1);

        store.delete_fact(&fact_id).await.expect("delete_fact");
        assert!(
            store
                .current_facts(&john)
                .await
                .expect("current2")
                .is_empty(),
            "delete_fact removes exactly the written edge"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    /// relate_fact must atomically supersede prior current facts with the
    /// same (subject, predicate) and a different object. Both rows stay
    /// queryable (history preserved); only the latest has valid_to = NONE.
    #[tokio::test]
    async fn relate_fact_supersedes_and_preserves_history() {
        use chrono::TimeZone;
        let dir = tempdir_for_test();
        let store = MemoryStore::open(&dir).await.expect("open");

        // Three entities to play with.
        let john = store
            .upsert_entity("John Smith", "person", vec![])
            .await
            .expect("john")
            .id;
        let acme = store
            .upsert_entity("Acme Corp", "organization", vec![])
            .await
            .expect("acme")
            .id;
        let beta = store
            .upsert_entity("Beta Corp", "organization", vec![])
            .await
            .expect("beta")
            .id;

        // John worked at Acme starting 2024-01-01.
        let acme_from = chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        store
            .relate_fact(FactWriteInput {
                subject_id: john.clone(),
                predicate: "works_at".into(),
                object_id: acme.clone(),
                valid_from: acme_from,
                source_chat_id: "msg_001".into(),
                confidence: 0.95,
            })
            .await
            .expect("write acme fact");

        // Then moved to Beta on 2026-03-15.
        let beta_from = chrono::Utc.with_ymd_and_hms(2026, 3, 15, 0, 0, 0).unwrap();
        store
            .relate_fact(FactWriteInput {
                subject_id: john.clone(),
                predicate: "works_at".into(),
                object_id: beta.clone(),
                valid_from: beta_from,
                source_chat_id: "msg_017".into(),
                confidence: 0.95,
            })
            .await
            .expect("write beta fact");

        // Current facts about John should be exactly one row: works_at -> Beta.
        let current = store.current_facts(&john).await.expect("current");
        assert_eq!(
            current.len(),
            1,
            "expected exactly one current fact, got {current:?}"
        );
        assert_eq!(current[0].predicate, "works_at");
        assert_eq!(record_id_to_string(&current[0].object), beta);

        // Total fact-edges should still be 2 (history preserved).
        let mut res = store
            .handle()
            .query("SELECT count() AS c FROM fact GROUP ALL;")
            .await
            .expect("count facts");
        let counts: Vec<i64> = res.take("c").expect("take c");
        assert_eq!(counts.first().copied(), Some(2), "history not preserved");

        // The old Acme edge should now have valid_to set to beta_from. Use
        // literal IDs since string binds don't compare equal to record refs.
        let john_key = john.strip_prefix("entity:").unwrap();
        let acme_key = acme.strip_prefix("entity:").unwrap();
        let sql = format!(
            "SELECT valid_to FROM fact \
             WHERE in = entity:{john_key} AND out = entity:{acme_key};"
        );
        let mut res = store.handle().query(sql).await.expect("query old fact");
        let valid_tos: Vec<Option<chrono::DateTime<chrono::Utc>>> =
            res.take("valid_to").expect("take valid_to");
        assert_eq!(
            valid_tos.first().and_then(|v| *v),
            Some(beta_from),
            "old Acme edge should have valid_to backfilled"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    /// A second write of the same (subject, predicate, object) should NOT
    /// supersede the existing one — same object means no contradiction.
    /// Currently relate_fact still creates a new edge (history of the same
    /// fact restated) — verify the supersession only fires for object changes.
    #[tokio::test]
    async fn relate_fact_does_not_supersede_same_object() {
        use chrono::TimeZone;
        let dir = tempdir_for_test();
        let store = MemoryStore::open(&dir).await.expect("open");

        let john = store
            .upsert_entity("John", "person", vec![])
            .await
            .expect("john")
            .id;
        let acme = store
            .upsert_entity("Acme", "organization", vec![])
            .await
            .expect("acme")
            .id;

        let t1 = chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let t2 = chrono::Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap();
        store
            .relate_fact(FactWriteInput {
                subject_id: john.clone(),
                predicate: "works_at".into(),
                object_id: acme.clone(),
                valid_from: t1,
                source_chat_id: "msg_001".into(),
                confidence: 0.95,
            })
            .await
            .expect("first");
        store
            .relate_fact(FactWriteInput {
                subject_id: john.clone(),
                predicate: "works_at".into(),
                object_id: acme.clone(),
                valid_from: t2,
                source_chat_id: "msg_002".into(),
                confidence: 0.95,
            })
            .await
            .expect("second");

        // The first edge should still be current (valid_to IS NONE) because the
        // second is the same object; supersession SQL excludes same-object.
        let current = store.current_facts(&john).await.expect("current");
        assert_eq!(
            current.len(),
            2,
            "both restatements of same fact remain current"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn slugify_handles_punctuation_and_spaces() {
        assert_eq!(slugify("Bill Gates"), "bill_gates");
        assert_eq!(slugify("J.P. Morgan & Co."), "j_p_morgan_co");
        assert_eq!(slugify("Q2 Planning"), "q2_planning");
        assert_eq!(slugify("  leading and trailing  "), "leading_and_trailing");
        assert_eq!(slugify("---"), "");
    }
}
