//! Embedded SurrealDB wrapper. Owns the connection, applies the schema on first
//! launch, exposes typed helpers for the bi-temporal-fact write/query patterns
//! the conversational agent's graph tools (`core::formation_tools`) use.

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
    ///
    /// The `SurrealKv` engine takes a filesystem path directly. An earlier
    /// `surrealkv://<path>` URL form was mis-parsed by the endpoint layer and
    /// left a stray `surrealkv:/` directory in the process cwd (refinement R5).
    pub async fn open(memory_dir: &Path) -> AppResult<Self> {
        std::fs::create_dir_all(memory_dir)?;

        let db: Surreal<Db> = Surreal::new::<SurrealKv>(memory_dir)
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
        Ok(self
            .relate_fact_with(fact, true)
            .await?
            .fact_id()
            .to_string())
    }

    /// Write a bi-temporal fact edge, controlling supersession.
    ///
    /// Same-object refinement (always, regardless of `supersede`): a closed
    /// write (`valid_to` set) whose `(subject, predicate, object)` already
    /// exists as a CURRENT edge closes that edge in place — it is the same
    /// relationship gaining an end date (a tense correction). No new edge is
    /// created; the result is `FactWrite::ClosedInPlace`.
    ///
    /// Otherwise, with `supersede = true` (atomic, in one SurrealDB query):
    /// 1. Any existing fact with the same `(subject, predicate)`,
    ///    `valid_to IS NONE`, AND a different `object` gets its
    ///    `valid_to = $valid_from`. History is preserved; the older edge
    ///    stays queryable for point-in-time reads.
    /// 2. The new edge is RELATEd.
    ///
    /// With `supersede = false`, step 1 is skipped — the new fact coexists
    /// with the contradicting current fact. This backs the "Keep both"
    /// conflict resolution (concurrent employment; ADR-0004 refinement R3).
    pub(crate) async fn relate_fact_with(
        &self,
        fact: FactWriteInput,
        supersede: bool,
    ) -> AppResult<FactWrite> {
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

        let new_valid_to = fact.valid_to;

        // A closed write whose (subject, predicate, object) already exists as
        // a CURRENT edge is the same relationship gaining an end date — a
        // tense correction ("works at" then "worked at" the same employer).
        // Close that edge in place rather than leaving a stale current edge
        // beside a new closed one. Independent of `supersede`, which governs
        // only the different-object job-change case.
        if let Some(valid_to) = new_valid_to {
            let find = format!(
                "SELECT id FROM fact \
                   WHERE in = entity:{subject_key} \
                     AND out = entity:{object_key} \
                     AND predicate = $predicate \
                     AND valid_to IS NONE \
                   LIMIT 1;"
            );
            let mut res = self
                .db
                .query(find)
                .bind(("predicate", fact.predicate.clone()))
                .await
                .map_err(|e| AppError::other(format!("relate_fact find current: {e}")))?;
            let existing: Vec<IdRow> = res
                .take(0)
                .map_err(|e| AppError::other(format!("relate_fact find take: {e}")))?;
            if let Some(row) = existing.into_iter().next() {
                let id = record_id_to_string(&row.id);
                let key = id.strip_prefix("fact:").unwrap_or(&id);
                self.db
                    .query("UPDATE type::record('fact', $key) SET valid_to = $valid_to;")
                    .bind(("key", key.to_string()))
                    .bind(("valid_to", valid_to))
                    .await
                    .map_err(|e| AppError::other(format!("relate_fact close: {e}")))?
                    .check()
                    .map_err(|e| AppError::other(format!("relate_fact close check: {e}")))?;
                return Ok(FactWrite::ClosedInPlace(id));
            }
        }

        // A historical fact (explicit valid_to) is never "current": it must
        // not supersede the live fact, nor be superseded by a later write.
        let supersede = supersede && new_valid_to.is_none();

        let relate = format!(
            "RELATE entity:{subject_key} -> fact -> entity:{object_key} \
               SET predicate = $predicate, \
                   valid_from = $valid_from, \
                   valid_to = $valid_to, \
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
            .bind(("valid_to", new_valid_to))
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
        Ok(FactWrite::Created(record_id_to_string(&fact_row.id)))
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

    /// The last `limit` `chat_message` rows for `session_id`, **oldest-first**,
    /// each as `(role, content)`.
    ///
    /// ADR-0009 §5: Sediment owns the conversation transcript. `chat_turn`
    /// assembles its recent-window history from this — the rows are fetched
    /// newest-first (so `LIMIT` keeps the *recent* tail) then reversed so the
    /// engine reads them in chronological order.
    pub async fn recent_messages(
        &self,
        session_id: &str,
        limit: usize,
    ) -> AppResult<Vec<(String, String)>> {
        // `LIMIT` cannot be bound as a parameter — clamp and splice it.
        // SurrealDB requires every `ORDER BY` idiom to appear in the
        // projection, so `timestamp` and `id` are selected even though only
        // `role` and `content` are returned to the caller.
        let limit_clamped = limit.clamp(1, 500);
        let sql = format!(
            "SELECT role, content, timestamp, id FROM chat_message \
             WHERE session_id = $sid \
             ORDER BY timestamp DESC, id DESC \
             LIMIT {limit_clamped};"
        );
        let mut res = self
            .db
            .query(sql)
            .bind(("sid", session_id.to_string()))
            .await
            .map_err(|e| AppError::other(format!("recent_messages: {e}")))?;
        let mut rows: Vec<ChatMessageRow> = res
            .take(0)
            .map_err(|e| AppError::other(format!("recent_messages take: {e}")))?;
        rows.reverse(); // newest-first query → oldest-first result
        Ok(rows.into_iter().map(|r| (r.role, r.content)).collect())
    }

    /// Delete one `chat_message` by record id. Used when a turn is interrupted
    /// and **redirected**: the abandoned user message is rolled back out of the
    /// transcript so the next turn's continuity window doesn't carry a prompt the
    /// user chose to change direction away from. Idempotent — a missing row is a
    /// no-op.
    pub async fn delete_chat_message(&self, id: &str) -> AppResult<()> {
        let key = id.strip_prefix("chat_message:").unwrap_or(id);
        self.db
            .query("DELETE type::record('chat_message', $key);")
            .bind(("key", key.to_string()))
            .await
            .map_err(|e| AppError::other(format!("delete_chat_message: {e}")))?
            .check()
            .map_err(|e| AppError::other(format!("delete_chat_message check: {e}")))?;
        Ok(())
    }

    /// The `fact` record ids whose `source_chat_id` equals `source_chat_id` —
    /// the graph Facts a single turn recorded.
    ///
    /// ADR-0009 §6: graph writes are discrete (they pass through the MCP
    /// server, which stamps each Fact with the turn's user `chat_message` id),
    /// so the per-turn audit entry learns its recorded Facts by querying this
    /// rather than by diffing.
    pub async fn facts_by_source(&self, source_chat_id: &str) -> AppResult<Vec<String>> {
        let mut res = self
            .db
            .query("SELECT id FROM fact WHERE source_chat_id = $src;")
            .bind(("src", source_chat_id.to_string()))
            .await
            .map_err(|e| AppError::other(format!("facts_by_source: {e}")))?;
        let rows: Vec<IdRow> = res
            .take(0)
            .map_err(|e| AppError::other(format!("facts_by_source take: {e}")))?;
        Ok(rows.iter().map(|r| record_id_to_string(&r.id)).collect())
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
    /// (if linked to a note). Used by the conversational agent to decide
    /// whether a fact updates an existing note or creates a new one.
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

    /// The most recently touched entities (by `updated_at`) — the people, orgs,
    /// and projects currently "in play". Drives the Working Set (ADR-0011 §3).
    /// `updated_at` is projected only to satisfy SurrealDB's ORDER-BY rule.
    pub async fn recent_entities(&self, limit: usize) -> AppResult<Vec<EntityLookup>> {
        let limit = limit.clamp(1, 100);
        let sql = format!(
            "SELECT id, entity_type, canonical_name, note_path, updated_at FROM entity \
             ORDER BY updated_at DESC LIMIT {limit};"
        );
        let mut res = self
            .db
            .query(sql)
            .await
            .map_err(|e| AppError::other(format!("recent_entities: {e}")))?;
        let rows: Vec<EntityRow> = res
            .take(0)
            .map_err(|e| AppError::other(format!("recent_entities take: {e}")))?;
        Ok(rows
            .into_iter()
            .map(|r| EntityLookup {
                id: record_id_to_string(&r.id),
                entity_type: r.entity_type,
                canonical_name: r.canonical_name,
                note_path: r.note_path,
            })
            .collect())
    }

    /// Enroll a speaker-embedding `sample` as `entity_id`'s **Voiceprint**,
    /// updating a running centroid (ADR-0017 §6/Q4): the first sample seeds it,
    /// later samples average in by count. The matching primitive for "name the
    /// speaker" — same shape as the note/entity `embedding`, different signal.
    // M4 Voiceprint scaffolding (ADR-0017 §6): tested, wired in when diarization lands.
    #[allow(dead_code)]
    pub async fn enroll_voiceprint(&self, entity_id: &str, sample: &[f32]) -> AppResult<()> {
        if sample.is_empty() {
            return Err(AppError::other("enroll_voiceprint: empty sample"));
        }
        let key = entity_id.strip_prefix("entity:").unwrap_or(entity_id);
        let mut res = self
            .db
            .query(format!(
                "SELECT voiceprint, voiceprint_n FROM entity:{key};"
            ))
            .await
            .map_err(|e| AppError::other(format!("enroll_voiceprint read: {e}")))?;
        let rows: Vec<VoiceprintRow> = res
            .take(0)
            .map_err(|e| AppError::other(format!("enroll_voiceprint take: {e}")))?;
        let Some(row) = rows.into_iter().next() else {
            return Err(AppError::other(format!(
                "enroll_voiceprint: no entity {entity_id}"
            )));
        };
        // Average the sample into the existing centroid, unless absent / wrong dim.
        let (centroid, n) = match (row.voiceprint, row.voiceprint_n) {
            (Some(c), Some(n)) if c.len() == sample.len() && n > 0 => (c, n as f32),
            _ => (Vec::new(), 0.0),
        };
        let updated: Vec<f32> = if centroid.is_empty() {
            sample.to_vec()
        } else {
            centroid
                .iter()
                .zip(sample)
                .map(|(c, s)| (c * n + s) / (n + 1.0))
                .collect()
        };
        self.db
            .query(format!(
                "UPDATE entity:{key} SET voiceprint = $vp, voiceprint_n = $n;"
            ))
            .bind(("vp", updated))
            .bind(("n", n as i64 + 1))
            .await
            .map_err(|e| AppError::other(format!("enroll_voiceprint write: {e}")))?
            .check()
            .map_err(|e| AppError::other(format!("enroll_voiceprint check: {e}")))?;
        Ok(())
    }

    /// The best `person` Entity whose **Voiceprint** matches `sample` by cosine
    /// similarity at or above `threshold` (ADR-0017 §6). `None` when nothing clears
    /// the bar — the caller leaves the speaker "Unknown speaker N" rather than
    /// guess. Naming a Fact to a below-threshold match is never done (ADR-0017
    /// Gap B); this returns the *label* candidate only.
    #[allow(dead_code)]
    pub async fn match_voiceprint(
        &self,
        sample: &[f32],
        threshold: f32,
    ) -> AppResult<Option<VoiceprintMatch>> {
        let mut res = self
            .db
            .query(
                "SELECT id, canonical_name, voiceprint FROM entity \
                 WHERE entity_type = 'person' AND voiceprint != NONE;",
            )
            .await
            .map_err(|e| AppError::other(format!("match_voiceprint: {e}")))?;
        let rows: Vec<VoiceprintMatchRow> = res
            .take(0)
            .map_err(|e| AppError::other(format!("match_voiceprint take: {e}")))?;
        let mut best: Option<VoiceprintMatch> = None;
        for r in rows {
            let Some(vp) = r.voiceprint else { continue };
            let score = cosine(sample, &vp);
            if score >= threshold && best.as_ref().map(|b| score > b.score).unwrap_or(true) {
                best = Some(VoiceprintMatch {
                    entity_id: record_id_to_string(&r.id),
                    canonical_name: r.canonical_name,
                    score,
                });
            }
        }
        Ok(best)
    }

    /// Enroll `sample` as the Voiceprint of the `person` named `name`, creating the
    /// person Entity if it does not exist yet (lazy enrolment, ADR-0015/§6). This is
    /// the "that was Sarah" path: naming a live speaker attaches their segment's
    /// embedding so future meetings recognise them.
    #[allow(dead_code)]
    pub async fn enroll_voiceprint_named(&self, name: &str, sample: &[f32]) -> AppResult<()> {
        let entity = self.upsert_entity(name, "person", vec![]).await?;
        self.enroll_voiceprint(&entity.id, sample).await
    }

    /// Every enrolled **Voiceprint** as `(canonical_name, centroid)` — the seed a
    /// live Session's diarizer loads so a known voice is named on its first segment
    /// (ADR-0017 §6). Persons without a voiceprint are skipped.
    #[allow(dead_code)]
    pub async fn all_voiceprints(&self) -> AppResult<Vec<(String, Vec<f32>)>> {
        let mut res = self
            .db
            .query(
                "SELECT id, canonical_name, voiceprint FROM entity \
                 WHERE entity_type = 'person' AND voiceprint != NONE;",
            )
            .await
            .map_err(|e| AppError::other(format!("all_voiceprints: {e}")))?;
        let rows: Vec<VoiceprintMatchRow> = res
            .take(0)
            .map_err(|e| AppError::other(format!("all_voiceprints take: {e}")))?;
        Ok(rows
            .into_iter()
            .filter_map(|r| r.voiceprint.map(|vp| (r.canonical_name, vp)))
            .collect())
    }

    /// Formation-relative paths of the most recently (re)indexed notes (by
    /// `indexed_at`) — a cheap proxy for "recently edited", since the indexer
    /// re-indexes a note on every change. Drives the Working Set (ADR-0011 §3).
    pub async fn recent_notes(&self, limit: usize) -> AppResult<Vec<String>> {
        let limit = limit.clamp(1, 100);
        let sql = format!(
            "SELECT note_path, indexed_at FROM note_index_state \
             ORDER BY indexed_at DESC LIMIT {limit};"
        );
        let mut res = self
            .db
            .query(sql)
            .await
            .map_err(|e| AppError::other(format!("recent_notes: {e}")))?;
        res.take((0, "note_path"))
            .map_err(|e| AppError::other(format!("recent_notes take: {e}")))
    }

    /// Record a new Open Loop (ADR-0011 §5) — an unresolved question or
    /// stated-but-unfulfilled intention the agent noticed. Returns its
    /// `open_loop:<id>` record id.
    pub async fn record_open_loop(
        &self,
        title: &str,
        context: Option<&str>,
        source_chat_id: &str,
    ) -> AppResult<String> {
        // A controlled, splice-safe id (slug + short random), like Task ids.
        let slug = slugify(title);
        let base = slug.chars().take(40).collect::<String>();
        let base = base.trim_end_matches('_');
        let base = if base.is_empty() { "loop" } else { base };
        let key = format!("{base}_{}", &uuid::Uuid::new_v4().simple().to_string()[..6]);
        self.db
            .query(format!(
                "CREATE open_loop:{key} SET title = $title, context = $context, \
                 source_chat_id = $src, created = time::now();"
            ))
            .bind(("title", title.to_string()))
            .bind(("context", context.map(str::to_string)))
            .bind(("src", source_chat_id.to_string()))
            .await
            .map_err(|e| AppError::other(format!("record_open_loop: {e}")))?
            .check()
            .map_err(|e| AppError::other(format!("record_open_loop check: {e}")))?;
        Ok(format!("open_loop:{key}"))
    }

    /// Resolve an Open Loop — set `archived_at` so it stops surfacing. Used by
    /// the agent's `close_open_loop` tool and the UI dismiss command.
    pub async fn close_open_loop(&self, loop_id: &str) -> AppResult<()> {
        let key = open_loop_key(loop_id);
        self.db
            .query(format!(
                "UPDATE open_loop:{key} SET archived_at = time::now();"
            ))
            .await
            .map_err(|e| AppError::other(format!("close_open_loop: {e}")))?
            .check()
            .map_err(|e| AppError::other(format!("close_open_loop check: {e}")))?;
        Ok(())
    }

    /// Mark an Open Loop as just surfaced in conversation — drives the rider's
    /// per-loop cooldown (ADR-0011 §4) so the same loop doesn't repeat.
    pub async fn mark_loop_surfaced(&self, loop_id: &str) -> AppResult<()> {
        let key = open_loop_key(loop_id);
        self.db
            .query(format!(
                "UPDATE open_loop:{key} SET last_surfaced_at = time::now();"
            ))
            .await
            .map_err(|e| AppError::other(format!("mark_loop_surfaced: {e}")))?
            .check()
            .map_err(|e| AppError::other(format!("mark_loop_surfaced check: {e}")))?;
        Ok(())
    }

    /// Active Open Loops to surface: not archived and created within
    /// `decay_days`, least-recently-surfaced first so the rider rotates through
    /// them rather than repeating one.
    pub async fn list_active_open_loops(
        &self,
        limit: usize,
        decay_days: i64,
    ) -> AppResult<Vec<OpenLoop>> {
        let limit = limit.clamp(1, 100);
        let cutoff = chrono::Utc::now() - chrono::Duration::days(decay_days.max(1));
        let sql = format!(
            "SELECT id, title, context, last_surfaced_at, created FROM open_loop \
             WHERE archived_at IS NONE AND created > $cutoff \
             ORDER BY last_surfaced_at ASC, created DESC LIMIT {limit};"
        );
        let mut res = self
            .db
            .query(sql)
            .bind(("cutoff", cutoff))
            .await
            .map_err(|e| AppError::other(format!("list_active_open_loops: {e}")))?;
        let rows: Vec<OpenLoopRow> = res
            .take(0)
            .map_err(|e| AppError::other(format!("list_active_open_loops take: {e}")))?;
        Ok(rows
            .into_iter()
            .map(|r| OpenLoop {
                id: record_id_to_string(&r.id),
                title: r.title,
                context: r.context,
            })
            .collect())
    }

    /// Current facts a new `(subject, predicate, object)` would contradict:
    /// same subject + predicate, a DIFFERENT object, still valid. These are
    /// the facts `relate_fact` would silently supersede — the agent surfaces
    /// them in conversation so the user resolves Update / Keep both / Discard.
    /// A brand-new subject entity (not yet stored) simply matches nothing.
    pub async fn find_conflicts(
        &self,
        subject_id: &str,
        predicate: &str,
        new_object_id: &str,
    ) -> AppResult<Vec<FactConflict>> {
        let subject_key = subject_id.strip_prefix("entity:").unwrap_or(subject_id);
        let object_key = new_object_id
            .strip_prefix("entity:")
            .unwrap_or(new_object_id);
        let sql = format!(
            "SELECT out.canonical_name AS object_name, \
             predicate, valid_from, source_chat_id \
             FROM fact \
             WHERE in = entity:{subject_key} \
               AND predicate = $predicate \
               AND out != entity:{object_key} \
               AND valid_to IS NONE;"
        );
        let mut res = self
            .db
            .query(sql)
            .bind(("predicate", predicate.to_string()))
            .await
            .map_err(|e| AppError::other(format!("find_conflicts: {e}")))?;
        let rows: Vec<ConflictRow> = res
            .take(0)
            .map_err(|e| AppError::other(format!("find_conflicts take: {e}")))?;
        Ok(rows
            .into_iter()
            .map(|r| FactConflict {
                object_name: r.object_name,
                predicate: r.predicate,
                valid_from: r.valid_from,
                source_chat_id: r.source_chat_id,
            })
            .collect())
    }

    /// Delete a fact edge by record id. The audit log's per-Fact revert uses
    /// this to remove exactly the facts a turn recorded — the ids are tracked,
    /// never re-derived (ADR-0009 §6).
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

    /// Top-K keyword search over `note_chunk.text` — the no-embedding-model path
    /// (EmbeddingProvider::None). Case-insensitive substring term matching: a
    /// chunk matches if its text contains any query term, ranked by how many
    /// distinct terms it contains. `distance` carries the negated match count so
    /// "lower is more relevant" stays consistent with the cosine path. No
    /// full-text index is needed, which keeps it portable across DB versions and
    /// is plenty for personal-scale formations.
    pub async fn search_chunks_text(&self, query: &str, k: usize) -> AppResult<Vec<ChunkHit>> {
        // Tokenise into lowercase alphanumeric terms (drop 1-char noise).
        let terms: Vec<String> = query
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| t.len() >= 2)
            .map(str::to_string)
            .collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }

        // Candidate chunks: any chunk whose lowercased text contains ≥1 term.
        // Terms are bound as params ($t0, $t1, …) so the query stays injection-safe.
        let where_clause = (0..terms.len())
            .map(|i| format!("string::lowercase(text) CONTAINS $t{i}"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let sql = format!(
            "SELECT note_path, chunk_idx, text, 0.0f AS distance \
             FROM note_chunk WHERE {where_clause};"
        );
        let mut q = self.db.query(sql);
        for (i, term) in terms.iter().enumerate() {
            q = q.bind((format!("t{i}"), term.clone()));
        }
        let mut res = q
            .await
            .map_err(|e| AppError::other(format!("text search: {e}")))?;
        let mut hits: Vec<ChunkHit> = res
            .take(0)
            .map_err(|e| AppError::other(format!("take text hits: {e}")))?;

        // Rank by distinct-term match count; encode it as a negative distance.
        for hit in &mut hits {
            let lower = hit.text.to_lowercase();
            let matches = terms.iter().filter(|t| lower.contains(*t)).count();
            hit.distance = -(matches as f32);
        }
        hits.sort_by(|a, b| a.distance.total_cmp(&b.distance));
        hits.truncate(k.clamp(1, 100));
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
    /// The chunk's embedding, or `None` in keyword-search mode (no model) —
    /// stored as SurrealDB `NULL`, which the HNSW index simply skips.
    pub embedding: Option<Vec<f32>>,
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

/// Row shape for `lookup_entity` — the fields the conversational agent needs.
#[derive(Debug, Clone, serde::Deserialize, SurrealValue)]
struct EntityRow {
    pub id: RecordId,
    pub entity_type: String,
    pub canonical_name: String,
    pub note_path: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, SurrealValue)]
#[allow(dead_code)]
struct VoiceprintRow {
    pub voiceprint: Option<Vec<f32>>,
    pub voiceprint_n: Option<i64>,
}

#[derive(Debug, Clone, serde::Deserialize, SurrealValue)]
#[allow(dead_code)]
struct VoiceprintMatchRow {
    pub id: RecordId,
    pub canonical_name: String,
    pub voiceprint: Option<Vec<f32>>,
}

/// A speaker-recognition hit (ADR-0017 §6): the matched person and the cosine
/// score that cleared the threshold.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct VoiceprintMatch {
    pub entity_id: String,
    pub canonical_name: String,
    pub score: f32,
}

/// Cosine similarity of two equal-length vectors; `0.0` on length mismatch or a
/// zero vector. Pure — the matching maths behind [`MemoryStore::match_voiceprint`].
#[allow(dead_code)]
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|y| y * y).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

/// Entity resolution result used by the conversational agent's graph tools.
/// The `id` is the flat `entity:<slug>` string; `note_path` is `Some` once
/// the entity has been filed into a note.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EntityLookup {
    pub id: String,
    pub entity_type: String,
    pub canonical_name: String,
    pub note_path: Option<String>,
}

/// What a `relate_fact_with` call did to the graph, so a commit can record
/// the exact inverse for undo.
#[derive(Debug, Clone)]
pub enum FactWrite {
    /// A new `fact` edge was RELATEd. Undo deletes it.
    Created(String),
    /// An existing CURRENT edge was closed in place — a later message refined
    /// the same relationship with an end date (a tense correction). Undo
    /// reopens it: a matched edge was always current before the write, so
    /// clearing `valid_to` restores it exactly.
    ClosedInPlace(String),
}

impl FactWrite {
    /// The record id of the affected edge, whichever kind of write it was.
    pub fn fact_id(&self) -> &str {
        match self {
            FactWrite::Created(id) | FactWrite::ClosedInPlace(id) => id,
        }
    }
}

/// Caller-supplied data for a fact write. Strings instead of typed enums
/// because the predicate vocabulary is agent-driven (ADR-0009) and may
/// expand without recompiling the storage layer.
#[derive(Debug, Clone)]
pub struct FactWriteInput {
    pub subject_id: String,
    pub predicate: String,
    pub object_id: String,
    pub valid_from: chrono::DateTime<chrono::Utc>,
    /// Explicit end of validity. `Some(_)` writes a historical (closed) edge —
    /// it is not "current" and does not supersede the current fact.
    pub valid_to: Option<chrono::DateTime<chrono::Utc>>,
    pub source_chat_id: String,
    pub confidence: f64,
}

/// Minimal row shape for queries that only need the record id back
/// (CREATE/RELATE returns, existence checks).
#[derive(Debug, Clone, serde::Deserialize, SurrealValue)]
struct IdRow {
    pub id: RecordId,
}

/// Row shape for `recent_messages`. `timestamp` and `id` are selected only to
/// satisfy SurrealDB's "ORDER BY idiom must be projected" rule — the caller
/// uses just `role` and `content`.
#[derive(Debug, Clone, serde::Deserialize, SurrealValue)]
struct ChatMessageRow {
    pub role: String,
    pub content: String,
    #[allow(dead_code)]
    pub timestamp: chrono::DateTime<chrono::Utc>,
    #[allow(dead_code)]
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

/// Raw row shape for `find_conflicts`. `out.canonical_name` traverses the
/// `out` record link to fetch the contradicting object's display name.
#[derive(Debug, Clone, serde::Deserialize, SurrealValue)]
struct ConflictRow {
    pub object_name: String,
    pub predicate: String,
    pub valid_from: chrono::DateTime<chrono::Utc>,
    pub source_chat_id: String,
}

/// An existing current fact that a new fact would contradict. Surfaced by the
/// agent's `find_contradiction` graph tool (ADR-0009 §5).
#[derive(Debug, Clone)]
pub struct FactConflict {
    pub object_name: String,
    pub predicate: String,
    pub valid_from: chrono::DateTime<chrono::Utc>,
    pub source_chat_id: String,
}

/// An Open Loop (ADR-0011 §5) surfaced to the agent and the UI. The `id` lets
/// the agent close it (`close_open_loop`) once it's resolved.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenLoop {
    pub id: String,
    pub title: String,
    pub context: Option<String>,
}

/// Row shape for the active-loop query. `last_surfaced_at` / `created` are
/// projected only to satisfy SurrealDB's ORDER-BY rule.
#[derive(Debug, Clone, serde::Deserialize, SurrealValue)]
struct OpenLoopRow {
    pub id: RecordId,
    pub title: String,
    pub context: Option<String>,
}

/// Format a SurrealDB RecordId as the `table:key` string we hand back to JS.
/// We only ever produce string-keyed ids in this app, so the other RecordIdKey
/// variants are best-effort fallbacks.
pub(crate) fn record_id_to_string(rid: &RecordId) -> String {
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

/// Splice-safe key for an `open_loop:<id>` reference — strips the table prefix
/// and keeps only `[a-z0-9_]` (the charset `record_open_loop` generates), so the
/// key can be spliced into an `open_loop:<key>` record id without injection.
fn open_loop_key(loop_id: &str) -> String {
    loop_id
        .strip_prefix("open_loop:")
        .unwrap_or(loop_id)
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect()
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
-- Speaker embedding for voice recognition in a meeting Session (ADR-0017 §6).
-- A `person` Entity's Voiceprint — same primitive as `embedding`, different
-- signal. A running centroid (ADR-0017 Q4); enrolled progressively and lazily.
DEFINE FIELD IF NOT EXISTS voiceprint     ON entity TYPE option<array<float>>;
DEFINE FIELD IF NOT EXISTS voiceprint_n   ON entity TYPE option<int>;
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

-- Note chunks for retrieval. `embedding` is optional: the no-local-model
-- search mode (EmbeddingProvider::None) stores text-only chunks and searches
-- them with the BM25 full-text index below. OVERWRITE (not IF NOT EXISTS) so
-- formations created under the old required-`array<float>` schema migrate to
-- the optional type — a safe widening that leaves existing vectors intact.
DEFINE TABLE IF NOT EXISTS note_chunk SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS note_path ON note_chunk TYPE string;
DEFINE FIELD IF NOT EXISTS chunk_idx ON note_chunk TYPE int;
DEFINE FIELD IF NOT EXISTS text      ON note_chunk TYPE string;
DEFINE FIELD OVERWRITE     embedding ON note_chunk TYPE option<array<float>>;
DEFINE INDEX IF NOT EXISTS chunk_embedding ON note_chunk FIELDS embedding
    HNSW DIMENSION 768 DIST COSINE;
-- The no-embedding-model path searches `text` with case-insensitive term
-- matching (see `search_chunks_text`); no full-text index is required.

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

-- Tasks: reminders (ADR-0007). The scheduling-side mirror of the `## Tasks`
-- checklist in Tasks.md. Distinct from the `task` *entity* type above.
DEFINE TABLE IF NOT EXISTS task SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS title          ON task TYPE string;
DEFINE FIELD IF NOT EXISTS status         ON task TYPE string
    ASSERT $value IN ['open','done'];
DEFINE FIELD IF NOT EXISTS due            ON task TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS remind_at      ON task TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS notified       ON task TYPE bool DEFAULT false;
DEFINE FIELD IF NOT EXISTS created        ON task TYPE datetime;
DEFINE FIELD IF NOT EXISTS completed_at   ON task TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS source_chat_id ON task TYPE option<string>;
DEFINE INDEX IF NOT EXISTS task_remind    ON task FIELDS remind_at;

-- Open Loops (ADR-0011 §5): unresolved questions / stated commitments the agent
-- noticed. Soft and conversational — distinct from a scheduled `task`. Surfaced
-- in conversation until resolved (`archived_at` set) or aged out of the window.
DEFINE TABLE IF NOT EXISTS open_loop SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS title            ON open_loop TYPE string;
DEFINE FIELD IF NOT EXISTS context          ON open_loop TYPE option<string>;
DEFINE FIELD IF NOT EXISTS created          ON open_loop TYPE datetime;
DEFINE FIELD IF NOT EXISTS source_chat_id   ON open_loop TYPE string;
DEFINE FIELD IF NOT EXISTS archived_at      ON open_loop TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS last_surfaced_at ON open_loop TYPE option<datetime>;
DEFINE INDEX IF NOT EXISTS open_loop_active ON open_loop FIELDS archived_at;
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

    #[test]
    fn cosine_is_one_for_identical_zero_for_orthogonal_and_mismatch() {
        assert!((cosine(&[1.0, 2.0, 3.0], &[1.0, 2.0, 3.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert_eq!(cosine(&[1.0, 2.0], &[1.0]), 0.0); // length mismatch
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0); // zero vector
    }

    /// Voiceprints (ADR-0017 §6): enrol a speaker embedding on a person, match a
    /// near-identical sample above threshold, reject a dissimilar one, and confirm
    /// enrolment keeps a running centroid (Q4).
    #[tokio::test]
    async fn voiceprint_enroll_match_and_centroid() {
        let dir = tempdir_for_test();
        let store = MemoryStore::open(&dir).await.expect("open store");

        let sarah = store
            .upsert_entity("Sarah Chen", "person", vec![])
            .await
            .unwrap();

        // No voiceprints yet → no match.
        assert!(store
            .match_voiceprint(&[1.0, 0.0, 0.0], 0.8)
            .await
            .unwrap()
            .is_none());

        store
            .enroll_voiceprint(&sarah.id, &[1.0, 0.0, 0.0])
            .await
            .unwrap();

        // A near-identical sample matches Sarah above threshold.
        let hit = store
            .match_voiceprint(&[0.96, 0.10, 0.0], 0.8)
            .await
            .unwrap()
            .expect("should match Sarah");
        assert_eq!(hit.entity_id, sarah.id);
        assert_eq!(hit.canonical_name, "Sarah Chen");
        assert!(hit.score >= 0.8);

        // An orthogonal sample clears nothing.
        assert!(store
            .match_voiceprint(&[0.0, 0.0, 1.0], 0.8)
            .await
            .unwrap()
            .is_none());

        // Second enrolment averages into the centroid (Q4 running centroid):
        // (1,0,0) then (0,1,0) → (0.5,0.5,0), which favours a diagonal sample.
        store
            .enroll_voiceprint(&sarah.id, &[0.0, 1.0, 0.0])
            .await
            .unwrap();
        let diag = store
            .match_voiceprint(&[1.0, 1.0, 0.0], 0.9)
            .await
            .unwrap()
            .expect("centroid should match the diagonal");
        assert_eq!(diag.entity_id, sarah.id);

        std::fs::remove_dir_all(dir).ok();
    }

    /// Open Loops (ADR-0011 §5): record → list active; closing archives it;
    /// a loop aged past the decay window falls out of the active set.
    #[tokio::test]
    async fn open_loop_record_list_close_and_decay() {
        let dir = tempdir_for_test();
        let store = MemoryStore::open(&dir).await.expect("open store");

        let id = store
            .record_open_loop(
                "Decide on the vendor",
                Some("Acme vs Beta"),
                "chat_message:1",
            )
            .await
            .unwrap();
        let active = store.list_active_open_loops(10, 14).await.unwrap();
        assert!(active
            .iter()
            .any(|l| l.id == id && l.title == "Decide on the vendor"));

        store.close_open_loop(&id).await.unwrap();
        let active = store.list_active_open_loops(10, 14).await.unwrap();
        assert!(
            !active.iter().any(|l| l.id == id),
            "closed loop is not active"
        );

        // A loop aged past the window falls out (back-date `created` directly).
        let stale = store
            .record_open_loop("Stale thread", None, "chat_message:2")
            .await
            .unwrap();
        let key = stale.strip_prefix("open_loop:").unwrap();
        let old = chrono::Utc::now() - chrono::Duration::days(30);
        store
            .db
            .query(format!("UPDATE open_loop:{key} SET created = $old;"))
            .bind(("old", old))
            .await
            .unwrap();
        let active = store.list_active_open_loops(10, 14).await.unwrap();
        assert!(
            !active.iter().any(|l| l.id == stale),
            "decayed loop excluded"
        );
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
                        embedding: Some(vec_a.clone()),
                    },
                    NoteChunkInput {
                        note_path: "People/John.md".into(),
                        chunk_idx: 1,
                        text: "His kid plays baseball.".into(),
                        embedding: Some(vec_b.clone()),
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

    /// Keyword/BM25 fallback: store text-only chunks (no embeddings, the
    /// no-local-model mode) and verify `search_chunks_text` ranks the chunk
    /// containing the query term first.
    #[tokio::test]
    async fn note_chunk_text_search_round_trip() {
        let dir = tempdir_for_test();
        let store = MemoryStore::open(&dir).await.expect("open");

        store
            .replace_note_chunks(
                "People/John.md",
                vec![
                    NoteChunkInput {
                        note_path: "People/John.md".into(),
                        chunk_idx: 0,
                        text: "John works at Acme as an engineer.".into(),
                        embedding: None,
                    },
                    NoteChunkInput {
                        note_path: "People/John.md".into(),
                        chunk_idx: 1,
                        text: "His kid plays baseball on weekends.".into(),
                        embedding: None,
                    },
                ],
            )
            .await
            .expect("insert text-only chunks");

        let hits = store
            .search_chunks_text("baseball", 5)
            .await
            .expect("text search");
        assert!(!hits.is_empty(), "expected a keyword hit, got none");
        assert_eq!(
            hits[0].chunk_idx, 1,
            "expected the baseball chunk first; got {hits:?}"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    /// End-to-end graph + provenance round-trip: persist a chat message,
    /// upsert entities, relate a fact citing that message, then supersede it.
    /// Verifies provenance and the supersession chain hold together without
    /// touching the embedding path.
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
                valid_to: None,
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
                valid_to: None,
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

    /// delete_fact removes a single edge.
    #[tokio::test]
    async fn delete_fact_removes_one_edge() {
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

        let fact_id = store
            .relate_fact(FactWriteInput {
                subject_id: john.clone(),
                predicate: "works_at".into(),
                object_id: acme.clone(),
                valid_from: Utc::now(),
                valid_to: None,
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

    /// find_conflicts flags a contradicting current fact, ignoring same-object
    /// restatements and unrelated predicates.
    #[tokio::test]
    async fn find_conflicts_flags_contradicting_current_facts() {
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
        let beta = store
            .upsert_entity("Beta", "organization", vec![])
            .await
            .expect("beta")
            .id;

        store
            .relate_fact(FactWriteInput {
                subject_id: john.clone(),
                predicate: "works_at".into(),
                object_id: acme.clone(),
                valid_from: Utc::now(),
                valid_to: None,
                source_chat_id: "chat_message:1".into(),
                confidence: 0.9,
            })
            .await
            .expect("relate");

        // A new works_at -> Beta contradicts the current works_at -> Acme.
        let conflicts = store
            .find_conflicts(&john, "works_at", &beta)
            .await
            .expect("find");
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].object_name, "Acme");

        // Same object is a restatement, not a conflict.
        assert!(store
            .find_conflicts(&john, "works_at", &acme)
            .await
            .expect("same object")
            .is_empty());
        // A different predicate is not a conflict.
        assert!(store
            .find_conflicts(&john, "advises", &beta)
            .await
            .expect("other predicate")
            .is_empty());

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
                valid_to: None,
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
                valid_to: None,
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
                valid_to: None,
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
                valid_to: None,
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

    /// A closed write of an existing CURRENT (subject, predicate, object)
    /// edge closes that edge in place — a tense correction — rather than
    /// leaving a stale current edge beside a new closed one.
    #[tokio::test]
    async fn relate_fact_closes_an_existing_current_edge_in_place() {
        use chrono::TimeZone;
        let dir = tempdir_for_test();
        let store = MemoryStore::open(&dir).await.expect("open");

        let josh = store
            .upsert_entity("Josh", "person", vec![])
            .await
            .expect("josh")
            .id;
        let cloudflare = store
            .upsert_entity("Cloudflare", "organization", vec![])
            .await
            .expect("cloudflare")
            .id;

        let from = chrono::Utc.with_ymd_and_hms(2019, 1, 1, 0, 0, 0).unwrap();
        // Turn 1: Josh works at Cloudflare — a current (open) edge.
        let first = store
            .relate_fact(FactWriteInput {
                subject_id: josh.clone(),
                predicate: "works_at".into(),
                object_id: cloudflare.clone(),
                valid_from: from,
                valid_to: None,
                source_chat_id: "msg_001".into(),
                confidence: 0.95,
            })
            .await
            .expect("first");

        // Turn 2: the same employment, now closed — "worked at".
        let ends = chrono::Utc.with_ymd_and_hms(2019, 12, 31, 0, 0, 0).unwrap();
        let outcome = store
            .relate_fact_with(
                FactWriteInput {
                    subject_id: josh.clone(),
                    predicate: "works_at".into(),
                    object_id: cloudflare.clone(),
                    valid_from: from,
                    valid_to: Some(ends),
                    source_chat_id: "msg_002".into(),
                    confidence: 0.95,
                },
                true,
            )
            .await
            .expect("second");

        // It closed the existing edge in place — same id, no parallel edge.
        match &outcome {
            FactWrite::ClosedInPlace(id) => {
                assert_eq!(id, &first, "the original edge was the one closed")
            }
            other => panic!("expected ClosedInPlace, got {other:?}"),
        }
        assert!(
            store
                .current_facts(&josh)
                .await
                .expect("current")
                .is_empty(),
            "no current employer remains after the tense correction"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    /// `recent_messages` returns the session's last `limit` rows oldest-first,
    /// scoped to one session, with the recent tail kept when `limit` is small.
    #[tokio::test]
    async fn recent_messages_returns_session_tail_oldest_first() {
        let dir = tempdir_for_test();
        let store = MemoryStore::open(&dir).await.expect("open");

        // Two sessions interleaved — recent_messages must scope to one.
        for (role, text) in [
            ("user", "first"),
            ("assistant", "first reply"),
            ("user", "second"),
            ("assistant", "second reply"),
            ("user", "third"),
        ] {
            store
                .insert_chat_message(role, text, "sess-A")
                .await
                .expect("insert A");
        }
        store
            .insert_chat_message("user", "other session", "sess-B")
            .await
            .expect("insert B");

        // The whole session, oldest-first.
        let all = store.recent_messages("sess-A", 20).await.expect("all");
        assert_eq!(all.len(), 5, "all five sess-A rows, sess-B excluded");
        assert_eq!(all[0], ("user".to_string(), "first".to_string()));
        assert_eq!(all[4], ("user".to_string(), "third".to_string()));

        // A small limit keeps the most recent tail — still oldest-first.
        let tail = store.recent_messages("sess-A", 2).await.expect("tail");
        assert_eq!(tail.len(), 2);
        assert_eq!(
            tail,
            vec![
                ("assistant".to_string(), "second reply".to_string()),
                ("user".to_string(), "third".to_string()),
            ]
        );

        // An unknown session yields an empty history, not an error.
        assert!(store
            .recent_messages("no-such-session", 20)
            .await
            .expect("empty")
            .is_empty());

        std::fs::remove_dir_all(dir).ok();
    }

    /// `facts_by_source` returns exactly the `fact` ids stamped with a given
    /// `source_chat_id` — the Facts one turn recorded.
    #[tokio::test]
    async fn facts_by_source_returns_facts_a_turn_recorded() {
        use chrono::Utc;
        let dir = tempdir_for_test();
        let store = MemoryStore::open(&dir).await.expect("open");

        let josh = store
            .upsert_entity("Josh", "person", vec![])
            .await
            .expect("josh")
            .id;
        let acme = store
            .upsert_entity("Acme", "organization", vec![])
            .await
            .expect("acme")
            .id;
        let rust = store
            .upsert_entity("Rust", "topic", vec![])
            .await
            .expect("rust")
            .id;

        // Turn 1 records two facts; turn 2 records one — distinct provenance.
        let f1 = store
            .relate_fact(FactWriteInput {
                subject_id: josh.clone(),
                predicate: "works_at".into(),
                object_id: acme.clone(),
                valid_from: Utc::now(),
                valid_to: None,
                source_chat_id: "chat_message:turn1".into(),
                confidence: 0.9,
            })
            .await
            .expect("f1");
        let f2 = store
            .relate_fact(FactWriteInput {
                subject_id: josh.clone(),
                predicate: "interested_in".into(),
                object_id: rust.clone(),
                valid_from: Utc::now(),
                valid_to: None,
                source_chat_id: "chat_message:turn1".into(),
                confidence: 0.9,
            })
            .await
            .expect("f2");
        let f3 = store
            .relate_fact(FactWriteInput {
                subject_id: acme.clone(),
                predicate: "located_in".into(),
                object_id: rust.clone(),
                valid_from: Utc::now(),
                valid_to: None,
                source_chat_id: "chat_message:turn2".into(),
                confidence: 0.9,
            })
            .await
            .expect("f3");

        let turn1 = store
            .facts_by_source("chat_message:turn1")
            .await
            .expect("turn1");
        assert_eq!(turn1.len(), 2, "turn 1 recorded exactly two facts");
        assert!(turn1.contains(&f1) && turn1.contains(&f2));
        assert!(!turn1.contains(&f3), "turn 2's fact is not in turn 1");

        let turn2 = store
            .facts_by_source("chat_message:turn2")
            .await
            .expect("turn2");
        assert_eq!(turn2, vec![f3]);

        assert!(store
            .facts_by_source("chat_message:never")
            .await
            .expect("none")
            .is_empty());

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
