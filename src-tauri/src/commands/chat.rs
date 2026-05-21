//! Chat-driven commands.
//!
//! - `chat_write`: a Write-mode message is persisted and run through the
//!   extraction pipeline; its facts are routed, diffed, and written to a
//!   staging entry for review — nothing touches SurrealDB until a Keep
//!   (spec principle #3, "AI proposes, human disposes").
//! - `chat_ask`: an Ask-mode question runs hybrid retrieval (vector search
//!   over note chunks + best-effort graph facts) and streams a cited answer.

use crate::commands::extraction::{MIN_ENTITY_CONFIDENCE, MIN_RELATION_CONFIDENCE};
use crate::commands::formation::APP_DIR;
use crate::commands::staging::assemble_note_change;
use crate::core::extraction::{
    default_relation_schema, extract_entities_and_relations, ExtractedEntity, FactExtractor,
    GlinerExtractor, ModelPaths, ENTITY_LABELS, SELF_NAME,
};
use crate::core::formation_state::{AppConfig, FormationState};
use crate::core::hardware::Tier;
use crate::core::llm_extractor::LlmExtractor;
use crate::core::memory::{slugify, ChunkHit, MemoryHandle, MemoryStore};
use crate::core::models::chat_model_for_tier;
use crate::core::ollama_sidecar::{OllamaSidecar, DEFAULT_EMBED_MODEL};
use crate::core::router::route_fact_unique;
use crate::core::staging::{self, StagedFact, StagingEntry};
use crate::error::{AppError, AppResult};
use futures::StreamExt;
use ollama_rs::generation::completion::request::GenerationRequest;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use tauri::ipc::Channel;
use tauri::{Emitter, State};

/// How many note chunks to pull into the answer context.
const RETRIEVAL_K: usize = 5;

/// The note every fact about the note-taker is filed into. One stable path
/// means "I" / "we" / "my" statements across many messages accrete in a single
/// place instead of scattering by pronoun.
pub(crate) const SELF_NOTE_PATH: &str = "People/Me.md";

/// The Ollama chat model to generate with, resolved from the user's tier
/// (spec §5). BYOK / an unset tier fall back to the Lite model.
fn resolve_chat_model(app: &tauri::AppHandle) -> String {
    let tier = AppConfig::load(app)
        .selected_tier
        .as_deref()
        .and_then(Tier::parse)
        .unwrap_or(Tier::Standard);
    chat_model_for_tier(tier).to_string()
}

/// The BYOK cloud setup, if the user configured a provider and an API key in
/// settings. `chat_ask` generates against the cloud when this is `Some`,
/// otherwise it streams from the local model.
fn byok_cloud_config(app: &tauri::AppHandle) -> Option<crate::core::cloud::CloudConfig> {
    use crate::core::cloud::{CloudConfig, CloudProvider};
    let cfg = AppConfig::load(app);
    let provider = CloudProvider::parse(cfg.byok_provider.as_deref()?)?;
    let api_key = cfg.byok_api_key.filter(|k| !k.trim().is_empty())?;
    let model = cfg
        .byok_model
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| provider.default_model().to_string());
    Some(CloudConfig {
        provider,
        api_key,
        model,
    })
}

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
    /// Relations the extractor proposed but dropped below the confidence
    /// floor. Surfaced (not silently swallowed) so a thin result is legible.
    pub skipped_low_confidence: usize,
    /// Relations whose subject or object entity the extractor never surfaced,
    /// leaving the triple unresolvable.
    pub skipped_unresolved: usize,
}

/// Write-mode chat turn. Runs the message through a `FactExtractor`, persists
/// the message, resolves + routes each fact to its subject's note, renders the
/// markdown diffs, and writes them to a staging entry. **Nothing is committed
/// to SurrealDB or to note files** — the user reviews and Keeps the entry from
/// the staging tray (see `commands::staging`).
#[tauri::command]
pub async fn chat_write(
    message: String,
    session_id: String,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
    sidecar: State<'_, OllamaSidecar>,
    app: tauri::AppHandle,
) -> AppResult<ChatWriteResult> {
    let formation_root = formation.require()?;
    let memory_dir = formation_root.join(APP_DIR).join("memory");
    let store = memory.get_or_init(&memory_dir).await?;
    let app_dir = formation_root.join(APP_DIR);

    // Best-effort: bring the daemon up so LLM extraction can run.
    let _ = sidecar.ensure_running().await;

    // LlmExtractor is primary (ADR-0006); it falls back to GLiNER internally on
    // any failure — daemon down, model missing, malformed JSON — so
    // `run_chat_write` is unaware which extractor produced the facts.
    let extractor = LlmExtractor::new(
        sidecar.client().clone(),
        resolve_chat_model(&app),
        GlinerExtractor::new(ModelPaths::under_app_dir(&app_dir)),
    );
    let result = run_chat_write(&message, &session_id, &extractor, &formation_root, store).await?;

    if let Some(entry) = &result.staged {
        if let Err(e) = app.emit("staging-created", entry) {
            tracing::warn!("emit staging-created failed: {e}");
        }
    }
    Ok(result)
}

/// Plain-function core of `chat_write`, decoupled from Tauri `State` so it can
/// be driven end-to-end in tests with any `FactExtractor` (a `ScriptedExtractor`
/// makes the Write pipeline deterministic). Beyond the chat-message row this
/// makes **no** SurrealDB or note-file writes — committing is the staging
/// tray's job.
pub async fn run_chat_write(
    message: &str,
    session_id: &str,
    extractor: &dyn FactExtractor,
    formation_root: &Path,
    store: &MemoryStore,
) -> AppResult<ChatWriteResult> {
    // Extract first: a failed extraction must leave no orphan chat row behind.
    let extraction = extractor.extract_facts(message).await?;

    let source_chat_id = store
        .insert_chat_message("user", message, session_id)
        .await?;

    // Entities above the confidence floor, keyed by `match_key` so a relation
    // endpoint resolves even when the model's case/spacing drifts from the
    // entity it listed. First mention of a name wins.
    let mut entity_by_name: HashMap<String, ExtractedEntity> = HashMap::new();
    for ent in extraction.entities {
        if ent.confidence < MIN_ENTITY_CONFIDENCE {
            continue;
        }
        entity_by_name.entry(match_key(&ent.name)).or_insert(ent);
    }

    let now = chrono::Utc::now();
    let mut skipped_low_confidence = 0usize;
    let mut skipped_unresolved = 0usize;

    // Resolve each relation into a StagedFact, grouped by its target note.
    let mut by_note: Vec<(String, Vec<StagedFact>)> = Vec::new();
    for rel in extraction.relations {
        if rel.confidence < MIN_RELATION_CONFIDENCE {
            skipped_low_confidence += 1;
            continue;
        }
        let (Some(subj), Some(obj)) = (
            entity_by_name.get(&match_key(&rel.subject)),
            entity_by_name.get(&match_key(&rel.object)),
        ) else {
            skipped_unresolved += 1; // an endpoint the extractor never surfaced
            continue;
        };
        let subject = resolve_entity(store, subj).await?;
        let object = resolve_entity(store, obj).await?;
        let note_path = subject_note_path(&subject, formation_root);
        let fact = StagedFact {
            subject_id: subject.id,
            subject_name: subject.name,
            subject_type: subject.entity_type,
            predicate: rel.predicate,
            object_id: object.id,
            object_name: object.name,
            object_type: object.entity_type,
            valid_from: rel.valid_from.unwrap_or(now),
            valid_from_explicit: rel.valid_from.is_some(),
            valid_to: rel.valid_to,
            confidence: rel.confidence as f64,
            explicit_coexist: false,
        };
        match by_note.iter_mut().find(|(p, _)| p == &note_path) {
            Some((_, facts)) => facts.push(fact),
            None => by_note.push((note_path, vec![fact])),
        }
    }

    // Render each affected note's diff and flag any contradicting current
    // facts. A fully-idempotent message stages nothing.
    let mut changes = Vec::new();
    for (note_path, facts) in by_note {
        if let Some(change) =
            assemble_note_change(store, formation_root, &note_path, &facts, &source_chat_id).await?
        {
            changes.push(change);
        }
    }

    let staged = if changes.is_empty() {
        None
    } else {
        let entry = StagingEntry {
            id: StagingEntry::new_id(),
            created: chrono::Utc::now(),
            chat_message_id: source_chat_id.clone(),
            chat_excerpt: excerpt(message),
            status: "pending".to_string(),
            changes,
        };
        staging::write(&formation_root.join(APP_DIR).join("staging"), &entry)?;
        Some(entry)
    };

    Ok(ChatWriteResult {
        source_chat_id,
        staged,
        skipped_low_confidence,
        skipped_unresolved,
    })
}

/// A subject/object entity resolved against the graph. Entities not yet stored
/// get a locally-derived slug id; only the note-taker and already-linked
/// entities carry a `note_path`.
struct ResolvedEntity {
    id: String,
    name: String,
    entity_type: String,
    note_path: Option<String>,
    is_self: bool,
}

/// Resolve an extracted entity against the graph without writing. The
/// note-taker (`is_self`) always resolves to the canonical "Me" person so
/// every first-person statement lands on the same entity; other entities fall
/// back to their surface name + type when not yet stored.
async fn resolve_entity(store: &MemoryStore, ext: &ExtractedEntity) -> AppResult<ResolvedEntity> {
    let canonical = if ext.is_self {
        SELF_NAME
    } else {
        ext.name.as_str()
    };
    let resolved = match store.lookup_entity(canonical).await? {
        Some(found) => ResolvedEntity {
            id: found.id,
            name: found.canonical_name,
            entity_type: found.entity_type,
            note_path: found.note_path,
            is_self: ext.is_self,
        },
        None => ResolvedEntity {
            id: format!("entity:{}", slugify(canonical)),
            name: canonical.to_string(),
            entity_type: ext.entity_type.clone(),
            note_path: None,
            is_self: ext.is_self,
        },
    };
    Ok(resolved)
}

/// The note a subject entity's facts are filed in. The note-taker always
/// routes to `SELF_NOTE_PATH`; everyone else uses their linked note or a fresh
/// one under their type's folder, collision-suffixed so two same-name entities
/// never share a file (`core::router`).
fn subject_note_path(entity: &ResolvedEntity, formation_root: &Path) -> String {
    if entity.is_self {
        return SELF_NOTE_PATH.to_string();
    }
    let (path, _) = route_fact_unique(
        &entity.entity_type,
        &entity.name,
        entity.note_path.as_deref(),
        formation_root,
    );
    path
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

/// Normalise a name for intra-extraction matching. A small model emits the
/// same entity with drifting case and spacing across its entity list and the
/// relation endpoints ("Josh" vs "josh" vs " Josh "), so matching a relation's
/// endpoint to an extracted entity must be case-insensitive and trimmed.
fn match_key(name: &str) -> String {
    name.trim().to_lowercase()
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

/// Ask-mode chat turn. Retrieves relevant note chunks (vector search) plus
/// best-effort graph facts, then streams a cited answer through `on_token`.
/// The question and answer are persisted only once generation succeeds, so a
/// failed turn can be retried without leaving an orphan message.
#[tauri::command]
pub async fn chat_ask(
    query: String,
    session_id: String,
    on_token: Channel<String>,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
    sidecar: State<'_, OllamaSidecar>,
    app: tauri::AppHandle,
) -> AppResult<ChatAskResult> {
    let formation_root = formation.require()?;
    let memory_dir = formation_root.join(APP_DIR).join("memory");
    let store = memory.get_or_init(&memory_dir).await?;

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

    // --- Generate the answer ---
    // BYOK: when the user configured a cloud provider + key, generate there
    // in one non-streaming request. Otherwise stream from the local model.
    let answer = match byok_cloud_config(&app) {
        Some(cloud) => {
            let text = crate::core::cloud::generate(&cloud, &prompt).await?;
            on_token
                .send(text.clone())
                .map_err(|e| AppError::other(format!("channel send: {e}")))?;
            text
        }
        None => {
            let client = sidecar.client();
            let mut stream = client
                .generate_stream(GenerationRequest::new(resolve_chat_model(&app), prompt))
                .await
                .map_err(|e| AppError::other(format!("start answer generation: {e}")))?;
            let mut answer = String::new();
            while let Some(chunk_result) = stream.next().await {
                let chunk =
                    chunk_result.map_err(|e| AppError::other(format!("stream error: {e}")))?;
                for response in chunk {
                    if !response.response.is_empty() {
                        answer.push_str(&response.response);
                        on_token
                            .send(response.response)
                            .map_err(|e| AppError::other(format!("channel send: {e}")))?;
                    }
                }
            }
            answer
        }
    };

    // Persist the question and answer together now that generation has
    // succeeded — a failed turn leaves no orphan row and can be retried.
    let source_chat_id = store
        .insert_chat_message("user", &query, &session_id)
        .await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpt_truncates_long_messages() {
        let short = "John founded Acme.";
        assert_eq!(excerpt(short), short);
        let long = "x".repeat(200);
        let e = excerpt(&long);
        assert!(e.ends_with('…'));
        assert_eq!(e.chars().count(), 141);
    }
}

/// End-to-end extraction tests. Layer 1 of the test strategy (ADR-0006): a
/// `ScriptedExtractor` replays a hand-authored ground-truth `Extraction`, so
/// the whole Write pipeline — resolve → route → stage → commit → note files —
/// is exercised deterministically, with no ONNX or LLM dependency.
///
/// Each `Fixture` is one "test conversation": a message, the structured
/// extraction a correct extractor must produce from it, and the note + staging
/// state the pipeline must yield. The set deliberately spans the complications
/// the pipeline has to survive — self-reference, tense, coreference,
/// multi-person facts, empty messages, and low-confidence / unresolvable
/// relations. The standup message also feeds the `#[ignore]` live recall test.
#[cfg(test)]
mod e2e_extraction {
    use super::*;
    use crate::commands::staging::commit_changes;
    use crate::core::extraction::{
        ExtractedEntity, ExtractedRelation, Extraction, ScriptedExtractor,
    };
    use crate::core::ollama_sidecar::OllamaSidecar;
    use crate::core::watcher::FormationWatcher;
    use chrono::TimeZone;
    use std::path::PathBuf;

    /// The example message this whole effort is anchored on.
    const STANDUP_MESSAGE: &str = "Standup with the platform team today, we're \
        finally ripping the legacy auth service off Redis before the next \
        release. Josh mentioned he worked at Cloudflare back in 2019, which \
        explains why he keeps pushing edge caching over a central store. Don't \
        let me forget to finish my CBT today.";

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir()
            .join("sediment-test-e2e")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&p).expect("tempdir");
        p
    }

    // --- Extraction builders -------------------------------------------------

    /// A non-self entity at full confidence.
    fn ent(name: &str, entity_type: &str) -> ExtractedEntity {
        ExtractedEntity {
            name: name.to_string(),
            entity_type: entity_type.to_string(),
            is_self: false,
            confidence: 0.9,
        }
    }

    /// The note-taker entity ("I" / "we" / "my", already resolved).
    fn me() -> ExtractedEntity {
        ExtractedEntity {
            name: SELF_NAME.to_string(),
            entity_type: "person".to_string(),
            is_self: true,
            confidence: 0.95,
        }
    }

    /// A current (open-ended) relation at full confidence.
    fn rel(subject: &str, predicate: &str, object: &str) -> ExtractedRelation {
        ExtractedRelation {
            subject: subject.to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            valid_from: None,
            valid_to: None,
            confidence: 0.9,
        }
    }

    /// Midnight UTC on Jan 1 of `year` — a year-granularity validity bound.
    fn jan(year: i32) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.with_ymd_and_hms(year, 1, 1, 0, 0, 0).unwrap()
    }

    // --- Fixture model -------------------------------------------------------

    /// One end-to-end extraction scenario.
    struct Fixture {
        /// The complication this scenario stresses.
        scenario: &'static str,
        /// The chat message a user would type.
        message: &'static str,
        /// Ground truth — the extraction fed through the pipeline.
        extraction: Extraction,
        /// Whether the message should produce a staging entry at all.
        expect_staged: bool,
        /// Notes expected on disk after commit, each with substrings it must
        /// contain.
        expect_notes: Vec<(&'static str, Vec<&'static str>)>,
        /// Substrings that must appear in NONE of `expect_notes`.
        forbid: Vec<&'static str>,
        /// Expected `(skipped_low_confidence, skipped_unresolved)` counts.
        expect_skipped: (usize, usize),
    }

    /// Drive one fixture through the full pipeline and assert its expectations.
    async fn check(fx: Fixture) {
        let Fixture {
            scenario,
            message,
            extraction,
            expect_staged,
            expect_notes,
            forbid,
            expect_skipped,
        } = fx;
        let root = tempdir();
        let store = MemoryStore::open(&root.join(APP_DIR).join("memory"))
            .await
            .unwrap_or_else(|e| panic!("[{scenario}] open store: {e}"));

        let extractor = ScriptedExtractor { extraction };
        let written = run_chat_write(message, "sess-e2e", &extractor, &root, &store)
            .await
            .unwrap_or_else(|e| panic!("[{scenario}] run_chat_write: {e}"));

        assert_eq!(
            (written.skipped_low_confidence, written.skipped_unresolved),
            expect_skipped,
            "[{scenario}] skip counts"
        );
        assert_eq!(
            written.staged.is_some(),
            expect_staged,
            "[{scenario}] expected staged = {expect_staged}"
        );

        if let Some(entry) = written.staged {
            commit_changes(
                &root,
                &store,
                &OllamaSidecar::default(),
                &FormationWatcher::default(),
                entry,
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("[{scenario}] commit: {e}"));
        }

        for (note, musts) in &expect_notes {
            let content = std::fs::read_to_string(root.join(note))
                .unwrap_or_else(|e| panic!("[{scenario}] note {note} not written: {e}"));
            for must in musts {
                assert!(
                    content.contains(must),
                    "[{scenario}] {note} is missing {must:?}\n--- {note} ---\n{content}"
                );
            }
            for bad in &forbid {
                assert!(
                    !content.contains(bad),
                    "[{scenario}] {note} must not contain {bad:?}\n--- {note} ---\n{content}"
                );
            }
        }
        std::fs::remove_dir_all(root).ok();
    }

    // --- Fixtures ------------------------------------------------------------

    /// Self-reference, past-tense employment, a task, an event, an opinion, and
    /// a project — the original message, every complication at once.
    fn standup() -> Fixture {
        let worked_at = ExtractedRelation {
            valid_from: Some(jan(2019)),
            valid_to: Some(chrono::Utc.with_ymd_and_hms(2019, 12, 31, 0, 0, 0).unwrap()),
            ..rel("Josh", "works_at", "Cloudflare")
        };
        Fixture {
            scenario: "standup — self, tense, task, event, opinion, project at once",
            message: STANDUP_MESSAGE,
            extraction: Extraction {
                entities: vec![
                    me(),
                    ent("Josh", "person"),
                    ent("Cloudflare", "organization"),
                    ent("edge caching", "topic"),
                    ent("Standup", "meeting"),
                    ent("CBT", "task"),
                    ent("2026-05-20", "date"),
                    ent("Legacy auth Redis migration", "project"),
                ],
                relations: vec![
                    rel(SELF_NAME, "attended", "Standup"),
                    rel(SELF_NAME, "owns_task", "CBT"),
                    rel(SELF_NAME, "leads", "Legacy auth Redis migration"),
                    worked_at,
                    rel("Josh", "advocates_for", "edge caching"),
                    rel("CBT", "due_on", "2026-05-20"),
                ],
            },
            expect_staged: true,
            expect_notes: vec![
                (
                    "People/Me.md",
                    vec![
                        "- Attended Standup",
                        "- Owns task CBT",
                        "- Leads Legacy auth Redis migration",
                    ],
                ),
                (
                    "People/Josh.md",
                    vec![
                        "- Worked at Cloudflare (2019)",
                        "- Advocates for edge caching",
                    ],
                ),
                ("Tasks/CBT.md", vec!["- Due on 2026-05-20"]),
            ],
            // The closed interval must render past tense — never present.
            forbid: vec!["Works at Cloudflare"],
            expect_skipped: (0, 0),
        }
    }

    /// A message with nothing to extract: stage nothing, write no note (rather
    /// than create an empty one).
    fn no_facts() -> Fixture {
        Fixture {
            scenario: "empty — a message with no extractable facts",
            message: "Long day. Honestly not sure what I want to capture here — \
                      just clearing my head before I log off.",
            extraction: Extraction::default(),
            expect_staged: false,
            expect_notes: vec![],
            forbid: vec![],
            expect_skipped: (0, 0),
        }
    }

    /// Coreference across two people: a correct extractor binds "she" → Priya
    /// and "him" → Devon. The fixture supplies the resolved names; the test
    /// proves the pipeline files the person→person fact on the right note.
    fn coreference() -> Fixture {
        Fixture {
            scenario: "coreference — pronouns resolved across two people",
            message: "Caught up with Priya. She said her manager Devon signed \
                      off on the Q3 budget, and she reports to him now.",
            extraction: Extraction {
                entities: vec![
                    ent("Priya", "person"),
                    ent("Devon", "person"),
                    ent("Q3 budget", "topic"),
                ],
                relations: vec![
                    rel("Devon", "approved", "Q3 budget"),
                    rel("Priya", "reports_to", "Devon"),
                ],
            },
            expect_staged: true,
            expect_notes: vec![
                ("People/Devon.md", vec!["- Approved Q3 budget"]),
                ("People/Priya.md", vec!["- Reports to Devon"]),
            ],
            forbid: vec![],
            expect_skipped: (0, 0),
        }
    }

    /// One past employer and one current employer in a single note: the closed
    /// interval renders "Worked at", the open one "Works at".
    fn tense_mix() -> Fixture {
        let past_job = ExtractedRelation {
            valid_to: Some(jan(2024)),
            ..rel("Dana", "works_at", "Stripe")
        };
        Fixture {
            scenario: "tense — past and present employer in one note",
            message: "Ran into Dana. She used to work at Stripe but moved on — \
                      she's at Linear now.",
            extraction: Extraction {
                entities: vec![
                    ent("Dana", "person"),
                    ent("Stripe", "organization"),
                    ent("Linear", "organization"),
                ],
                relations: vec![past_job, rel("Dana", "works_at", "Linear")],
            },
            expect_staged: true,
            expect_notes: vec![(
                "People/Dana.md",
                vec!["- Worked at Stripe", "- Works at Linear"],
            )],
            forbid: vec!["Works at Stripe", "Worked at Linear"],
            expect_skipped: (0, 0),
        }
    }

    /// Below-threshold and unresolvable relations must be counted and surfaced
    /// — not silently dropped (the ADR-0003 gap) and not written to a note.
    fn low_signal() -> Fixture {
        let tentative = ExtractedRelation {
            confidence: 0.4, // below MIN_RELATION_CONFIDENCE
            ..rel(SELF_NAME, "collaborates_with", "someone")
        };
        Fixture {
            scenario: "low-signal — skipped low-confidence and unresolvable relations",
            message: "Met someone at the meetup tonight. We might collaborate \
                      down the line — we'll see.",
            extraction: Extraction {
                entities: vec![me(), ent("the meetup", "event")],
                relations: vec![
                    rel(SELF_NAME, "attended", "the meetup"),
                    tentative,
                    // object "Ghost" is not in `entities` — unresolvable.
                    rel(SELF_NAME, "knows", "Ghost"),
                ],
            },
            expect_staged: true,
            expect_notes: vec![("People/Me.md", vec!["- Attended the meetup"])],
            forbid: vec!["Collaborates", "Knows"],
            expect_skipped: (1, 1),
        }
    }

    /// An organization as the fact's subject — exercises routing to the
    /// `Organizations/` folder, not just the `People/` path the other fixtures
    /// take.
    fn org_subject() -> Fixture {
        Fixture {
            scenario: "org-subject — a fact about an organization routes to Organizations/",
            message: "Acme acquired Beta Corp this quarter.",
            extraction: Extraction {
                entities: vec![
                    ent("Acme", "organization"),
                    ent("Beta Corp", "organization"),
                ],
                relations: vec![rel("Acme", "acquired", "Beta Corp")],
            },
            expect_staged: true,
            expect_notes: vec![("Organizations/Acme.md", vec!["- Acquired Beta Corp"])],
            forbid: vec![],
            expect_skipped: (0, 0),
        }
    }

    /// The extractor lists the entity as "Priya" but writes the relation's
    /// endpoint as "  PRIYA  " — the case/spacing drift a small model produces.
    /// Resolution must still bind the two (gap G2); without the `match_key`
    /// normalisation the fact would be dropped as unresolvable.
    fn case_drift() -> Fixture {
        Fixture {
            scenario: "case-drift — a relation endpoint's case/spacing differs from the entity",
            message: "Priya owns the Q3 budget now.",
            extraction: Extraction {
                entities: vec![ent("Priya", "person"), ent("Q3 budget", "topic")],
                relations: vec![rel("  PRIYA  ", "manages", "q3 budget")],
            },
            expect_staged: true,
            expect_notes: vec![("People/Priya.md", vec!["- Manages Q3 budget"])],
            forbid: vec![],
            expect_skipped: (0, 0),
        }
    }

    // --- One test per fixture (a failure names the scenario) -----------------

    #[tokio::test]
    async fn fixture_standup_message() {
        check(standup()).await;
    }

    #[tokio::test]
    async fn fixture_message_with_no_facts() {
        check(no_facts()).await;
    }

    #[tokio::test]
    async fn fixture_coreference_across_people() {
        check(coreference()).await;
    }

    #[tokio::test]
    async fn fixture_past_and_present_tense() {
        check(tense_mix()).await;
    }

    #[tokio::test]
    async fn fixture_low_confidence_and_unresolvable() {
        check(low_signal()).await;
    }

    #[tokio::test]
    async fn fixture_organization_subject_routes_to_orgs_folder() {
        check(org_subject()).await;
    }

    #[tokio::test]
    async fn fixture_relation_endpoints_resolve_case_insensitively() {
        check(case_drift()).await;
    }

    /// A complication a single-message fixture cannot express: a later message
    /// that contradicts an earlier one. The second turn must flag the conflict
    /// pre-commit, and after commit the superseded employer must no longer be a
    /// current graph fact.
    #[tokio::test]
    async fn a_later_message_supersedes_an_earlier_employer() {
        let root = tempdir();
        let store = MemoryStore::open(&root.join(APP_DIR).join("memory"))
            .await
            .expect("open store");
        let sidecar = OllamaSidecar::default();
        let watcher = FormationWatcher::default();

        // Turn 1: the note-taker starts at Acme.
        let turn1 = Extraction {
            entities: vec![me(), ent("Acme", "organization")],
            relations: vec![rel(SELF_NAME, "works_at", "Acme")],
        };
        let w1 = run_chat_write(
            "Started at Acme this week.",
            "sess",
            &ScriptedExtractor { extraction: turn1 },
            &root,
            &store,
        )
        .await
        .expect("turn 1 write");
        commit_changes(
            &root,
            &store,
            &sidecar,
            &watcher,
            w1.staged.expect("turn 1 staged"),
            None,
        )
        .await
        .expect("commit turn 1");

        // Turn 2: a move to Globex — contradicts the current Acme fact.
        let turn2 = Extraction {
            entities: vec![me(), ent("Globex", "organization")],
            relations: vec![rel(SELF_NAME, "works_at", "Globex")],
        };
        let w2 = run_chat_write(
            "Update — I'm at Globex now.",
            "sess",
            &ScriptedExtractor { extraction: turn2 },
            &root,
            &store,
        )
        .await
        .expect("turn 2 write");
        let entry2 = w2.staged.expect("turn 2 staged");

        // Pre-commit: the new employer is flagged as conflicting with the old.
        let me_change = entry2
            .changes
            .iter()
            .find(|c| c.note_path == SELF_NOTE_PATH)
            .expect("a staged change for the note-taker");
        assert_eq!(
            me_change.conflicts.len(),
            1,
            "the new employer must flag a conflict with the old one"
        );
        assert_eq!(me_change.conflicts[0].existing_object_name, "Acme");

        commit_changes(&root, &store, &sidecar, &watcher, entry2, None)
            .await
            .expect("commit turn 2");

        // Post-commit: exactly one current employer — Acme was superseded.
        let current = store
            .current_facts("entity:me")
            .await
            .expect("current facts");
        assert_eq!(
            current.len(),
            1,
            "exactly one current employer after supersession; got {current:?}"
        );
        assert_eq!(current[0].predicate, "works_at");

        std::fs::remove_dir_all(root).ok();
    }

    /// Whole-batch idempotence across turns: committing a message, then sending
    /// the identical message again, stages nothing — every fact is already on
    /// the note, so `assemble_note_change` filters the batch down to empty.
    #[tokio::test]
    async fn re_sending_the_same_message_stages_nothing() {
        let root = tempdir();
        let store = MemoryStore::open(&root.join(APP_DIR).join("memory"))
            .await
            .expect("open store");
        let sidecar = OllamaSidecar::default();
        let watcher = FormationWatcher::default();

        let extraction = || Extraction {
            entities: vec![me(), ent("Acme", "organization")],
            relations: vec![rel(SELF_NAME, "works_at", "Acme")],
        };
        let message = "I work at Acme.";

        let w1 = run_chat_write(
            message,
            "sess",
            &ScriptedExtractor {
                extraction: extraction(),
            },
            &root,
            &store,
        )
        .await
        .expect("turn 1 write");
        commit_changes(
            &root,
            &store,
            &sidecar,
            &watcher,
            w1.staged.expect("turn 1 staged"),
            None,
        )
        .await
        .expect("commit turn 1");

        // Turn 2: the identical message — every fact is already filed.
        let w2 = run_chat_write(
            message,
            "sess",
            &ScriptedExtractor {
                extraction: extraction(),
            },
            &root,
            &store,
        )
        .await
        .expect("turn 2 write");
        assert!(
            w2.staged.is_none(),
            "a re-sent message must stage nothing; got {:?}",
            w2.staged
        );

        std::fs::remove_dir_all(root).ok();
    }

    /// A second fact about an entity whose note already exists — and which the
    /// user has hand-edited since — appends under `## Facts` without disturbing
    /// the prose the user added outside the managed section.
    #[tokio::test]
    async fn an_update_preserves_hand_edited_prose() {
        let root = tempdir();
        let store = MemoryStore::open(&root.join(APP_DIR).join("memory"))
            .await
            .expect("open store");
        let sidecar = OllamaSidecar::default();
        let watcher = FormationWatcher::default();

        // Turn 1: a fact about Dana creates People/Dana.md.
        let w1 = run_chat_write(
            "Dana works at Stripe.",
            "sess",
            &ScriptedExtractor {
                extraction: Extraction {
                    entities: vec![ent("Dana", "person"), ent("Stripe", "organization")],
                    relations: vec![rel("Dana", "works_at", "Stripe")],
                },
            },
            &root,
            &store,
        )
        .await
        .expect("turn 1 write");
        commit_changes(
            &root,
            &store,
            &sidecar,
            &watcher,
            w1.staged.expect("turn 1 staged"),
            None,
        )
        .await
        .expect("commit turn 1");

        // The user hand-edits the note, adding a prose section of their own.
        let dana = root.join("People/Dana.md");
        let edited = format!(
            "{}\n## Notes\n\nDana gives great design feedback.\n",
            std::fs::read_to_string(&dana).expect("read Dana note"),
        );
        std::fs::write(&dana, &edited).expect("hand-edit Dana note");

        // Turn 2: a second fact about Dana routes as an update to that note.
        let w2 = run_chat_write(
            "Dana is into Rust.",
            "sess",
            &ScriptedExtractor {
                extraction: Extraction {
                    entities: vec![ent("Dana", "person"), ent("Rust", "topic")],
                    relations: vec![rel("Dana", "interested_in", "Rust")],
                },
            },
            &root,
            &store,
        )
        .await
        .expect("turn 2 write");
        commit_changes(
            &root,
            &store,
            &sidecar,
            &watcher,
            w2.staged.expect("turn 2 staged"),
            None,
        )
        .await
        .expect("commit turn 2");

        let content = std::fs::read_to_string(&dana).expect("read Dana note");
        assert!(
            content.contains("- Works at Stripe"),
            "turn 1 fact survives"
        );
        assert!(
            content.contains("- Interested in Rust"),
            "turn 2 fact is appended"
        );
        assert!(
            content.contains("Dana gives great design feedback."),
            "the user's hand-edited prose is preserved\n--- Dana.md ---\n{content}"
        );

        std::fs::remove_dir_all(root).ok();
    }

    /// Phase 3 R4: a freshly-mentioned entity whose name nearly matches an
    /// existing one carries a disambiguation suggestion; accepting it re-points
    /// the fact onto the existing entity and re-routes the change to its note.
    #[tokio::test]
    async fn a_near_name_match_offers_a_disambiguation_then_merges() {
        use crate::commands::staging::{run_apply_disambiguation, staging_dir};
        let root = tempdir();
        let store = MemoryStore::open(&root.join(APP_DIR).join("memory"))
            .await
            .expect("open store");
        let sidecar = OllamaSidecar::default();
        let watcher = FormationWatcher::default();

        // Seed an existing person, committed so they own a note in the graph.
        let seed = run_chat_write(
            "John Smith works at Acme.",
            "sess",
            &ScriptedExtractor {
                extraction: Extraction {
                    entities: vec![ent("John Smith", "person"), ent("Acme", "organization")],
                    relations: vec![rel("John Smith", "works_at", "Acme")],
                },
            },
            &root,
            &store,
        )
        .await
        .expect("seed write");
        commit_changes(
            &root,
            &store,
            &sidecar,
            &watcher,
            seed.staged.expect("seed staged"),
            None,
        )
        .await
        .expect("seed commit");

        // A new message names "Jon Smith" — a near-match, resolved as new.
        let w = run_chat_write(
            "Jon Smith joined the chess club.",
            "sess",
            &ScriptedExtractor {
                extraction: Extraction {
                    entities: vec![
                        ent("Jon Smith", "person"),
                        ent("chess club", "organization"),
                    ],
                    relations: vec![rel("Jon Smith", "member_of", "chess club")],
                },
            },
            &root,
            &store,
        )
        .await
        .expect("write");
        let entry = w.staged.expect("staged");

        // The new note carries a "did you mean John Smith?" suggestion.
        let change = entry
            .changes
            .iter()
            .find(|c| c.note_path == "People/Jon Smith.md")
            .expect("a change for the new Jon Smith note");
        let suggestion = change
            .suggestions
            .iter()
            .find(|s| s.endpoint == "subject")
            .expect("a subject disambiguation suggestion");
        assert_eq!(suggestion.candidate_name, "John Smith");

        // Accept it — the fact re-routes onto John Smith's existing note.
        run_apply_disambiguation(
            &root,
            &store,
            &entry.id,
            "People/Jon Smith.md",
            0,
            "subject",
        )
        .await
        .expect("apply disambiguation");

        let merged = staging::read_one(&staging_dir(&root), &entry.id).expect("re-read entry");
        assert!(
            merged
                .changes
                .iter()
                .any(|c| c.note_path == "People/John Smith.md"),
            "the fact moved onto John Smith's existing note"
        );
        assert!(
            !merged
                .changes
                .iter()
                .any(|c| c.note_path == "People/Jon Smith.md"),
            "the Jon Smith note is gone after the merge"
        );

        std::fs::remove_dir_all(root).ok();
    }

    /// The seven things a correct extraction of `STANDUP_MESSAGE` must surface.
    /// Used by the live test to score recall; the deterministic test above
    /// already proves the pipeline handles all of them.
    fn recall_checklist(ex: &Extraction) -> Vec<(&'static str, bool)> {
        let names: Vec<String> = ex.entities.iter().map(|e| e.name.to_lowercase()).collect();
        let has_entity = |kw: &str| names.iter().any(|n| n.contains(kw));
        vec![
            (
                "note-taker resolved as self",
                ex.entities.iter().any(|e| e.is_self),
            ),
            ("Josh", has_entity("josh")),
            (
                "Cloudflare job is PAST tense",
                ex.relations.iter().any(|r| {
                    r.object.to_lowercase().contains("cloudflare") && r.valid_to.is_some()
                }),
            ),
            (
                "edge-caching stance",
                has_entity("caching")
                    || ex
                        .relations
                        .iter()
                        .any(|r| r.object.to_lowercase().contains("caching")),
            ),
            ("CBT task", has_entity("cbt")),
            ("standup meeting", has_entity("standup")),
            (
                "Redis migration",
                has_entity("redis") || has_entity("auth") || has_entity("migration"),
            ),
        ]
    }

    /// Layer 2 (ADR-0006): runs the *real* `LlmExtractor` over the standup
    /// message and scores recall against `recall_checklist`. `#[ignore]` —
    /// it needs a running Ollama and is non-deterministic, so it never gates
    /// CI. Run it manually to see how a given model tier actually performs:
    ///
    ///   cargo test --lib -- --ignored --nocapture live_llm_extraction
    ///
    /// Override the model with `SEDIMENT_CHAT_MODEL` (default: llama3.2:3b).
    #[tokio::test]
    #[ignore]
    async fn live_llm_extraction_recall() {
        use crate::core::llm_extractor::LlmExtractor;
        use ollama_rs::Ollama;

        let model =
            std::env::var("SEDIMENT_CHAT_MODEL").unwrap_or_else(|_| "llama3.2:3b".to_string());
        eprintln!("live extraction model: {model}");

        // The GLiNER fallback is pointed at an empty dir: if the LLM path
        // fails we want a loud, clear failure rather than silent degradation.
        let no_gliner = ModelPaths::under_app_dir(&tempdir());
        let extractor =
            LlmExtractor::new(Ollama::default(), model, GlinerExtractor::new(no_gliner));

        let extraction = extractor
            .extract_facts(STANDUP_MESSAGE)
            .await
            .expect("live extraction (is Ollama running?)");

        eprintln!("--- entities ({}) ---", extraction.entities.len());
        for e in &extraction.entities {
            eprintln!(
                "  {} [{}]{}",
                e.name,
                e.entity_type,
                if e.is_self { " SELF" } else { "" }
            );
        }
        eprintln!("--- relations ({}) ---", extraction.relations.len());
        for r in &extraction.relations {
            eprintln!(
                "  {} --{}--> {}{}",
                r.subject,
                r.predicate,
                r.object,
                if r.valid_to.is_some() { " (past)" } else { "" }
            );
        }

        let checklist = recall_checklist(&extraction);
        let hits = checklist.iter().filter(|(_, ok)| *ok).count();
        eprintln!("--- recall: {hits}/{} ---", checklist.len());
        for (label, ok) in &checklist {
            eprintln!("  [{}] {label}", if *ok { "x" } else { " " });
        }

        assert!(
            !extraction.entities.is_empty() && !extraction.relations.is_empty(),
            "the LLM extractor produced an empty extraction"
        );
        assert!(
            hits >= 3,
            "live extraction recall {hits}/7 is below the floor of 3 — \
             see the printed checklist; tune the prompt or use a larger model"
        );
    }
}
