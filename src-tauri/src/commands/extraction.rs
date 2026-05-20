//! Tauri commands exposing the entity extraction pipeline. Phase 1 scope: a
//! smoke command that returns NER spans for a single message + label set.
//! Real Write-pipeline integration lands in Phase 2 once SurrealDB writes are
//! wired into the extraction output.

use crate::commands::formation::APP_DIR;
use crate::core::extraction::{
    default_relation_schema, extract_entities_and_relations, EntityExtractor, EntitySpan,
    GlinerExtractor, ModelPaths, RelationSpan, ENTITY_LABELS,
};
use crate::core::formation_state::FormationState;
use crate::core::memory::{FactWriteInput, MemoryHandle, UpsertedEntity};
use crate::error::{AppError, AppResult};
use serde::Serialize;
use std::collections::HashMap;
use tauri::State;

#[derive(Debug, Serialize)]
pub struct ExtractionResult {
    pub model_ready: bool,
    pub bootstrap_hint: Option<String>,
    pub spans: Vec<EntitySpan>,
}

/// Run NER on `text` with the provided `labels`. If the model files aren't on
/// disk, returns `model_ready: false` and the bootstrap hint instead of erroring.
#[tauri::command]
pub fn extract_entities(
    text: String,
    labels: Vec<String>,
    state: State<'_, FormationState>,
) -> AppResult<ExtractionResult> {
    let formation = state.require()?;
    let paths = ModelPaths::under_app_dir(&formation.join(APP_DIR));

    if !paths.exist() {
        return Ok(ExtractionResult {
            model_ready: false,
            bootstrap_hint: Some(crate::core::extraction::model_bootstrap_hint(&paths)),
            spans: vec![],
        });
    }

    let extractor = GlinerExtractor::new(paths);
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let spans = extractor
        .extract(&[text.as_str()], &label_refs)
        .map_err(|e| AppError::other(format!("extraction failed: {e}")))?;
    let flat: Vec<EntitySpan> = spans.into_iter().flatten().collect();

    Ok(ExtractionResult {
        model_ready: true,
        bootstrap_hint: None,
        spans: flat,
    })
}

/// Each entity span emitted by `extract_and_upsert`, enriched with the
/// resolved SurrealDB record id and an idempotence flag.
#[derive(Debug, Serialize)]
pub struct UpsertedSpan {
    pub text: String,
    pub class: String,
    pub probability: f32,
    pub entity_id: String,
    pub was_new: bool,
}

/// Minimum span confidence we'll accept into the store. Anything noisier
/// creates entities you'd immediately want to discard from the staging tray.
pub const MIN_ENTITY_CONFIDENCE: f32 = 0.5;
/// Same for relations — RE is generally noisier than NER, so we set the bar
/// a touch higher. Tunable per-tier in Phase 2 follow-ups.
pub const MIN_RELATION_CONFIDENCE: f32 = 0.6;

/// Run NER over `text` (using the canonical Sediment entity-type set), then
/// upsert each detected entity into SurrealDB. Idempotent — re-running on the
/// same text returns the same entity ids with `was_new = false`.
#[tauri::command]
pub async fn extract_and_upsert(
    text: String,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
) -> AppResult<Vec<UpsertedSpan>> {
    let formation_root = formation.require()?;
    let app_dir = formation_root.join(APP_DIR);
    let paths = ModelPaths::under_app_dir(&app_dir);
    if !paths.exist() {
        return Err(AppError::other(
            crate::core::extraction::model_bootstrap_hint(&paths),
        ));
    }

    let extractor = GlinerExtractor::new(paths);
    let spans = extractor
        .extract(&[text.as_str()], ENTITY_LABELS)
        .map_err(|e| AppError::other(format!("extraction failed: {e}")))?;
    let flat: Vec<EntitySpan> = spans.into_iter().flatten().collect();

    let memory_dir = app_dir.join("memory");
    let store = memory.get_or_init(&memory_dir).await?;

    let mut results = Vec::with_capacity(flat.len());
    for span in flat {
        if span.probability < MIN_ENTITY_CONFIDENCE {
            continue;
        }
        let upserted: UpsertedEntity = store
            .upsert_entity(&span.text, &span.class, Vec::new())
            .await?;
        results.push(UpsertedSpan {
            text: span.text,
            class: span.class,
            probability: span.probability,
            entity_id: upserted.id,
            was_new: upserted.was_new,
        });
    }
    Ok(results)
}

#[derive(Debug, Serialize)]
pub struct FactWritten {
    pub fact_id: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub confidence: f32,
}

#[derive(Debug, Serialize)]
pub struct ExtractFactsResult {
    pub entities: Vec<UpsertedSpan>,
    pub facts: Vec<FactWritten>,
    pub skipped_low_confidence: usize,
    pub skipped_unresolved_entity: usize,
}

/// Full extraction pipeline: NER + RE on `text`, upsert every entity, write
/// every relation as a bi-temporal fact. Returns a structured summary the
/// chat pane can render. `source_chat_id` flows into each fact's provenance.
#[tauri::command]
pub async fn extract_facts(
    text: String,
    source_chat_id: String,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
) -> AppResult<ExtractFactsResult> {
    let formation_root = formation.require()?;
    let memory_dir = formation_root.join(APP_DIR).join("memory");
    let store = memory.get_or_init(&memory_dir).await?;
    run_extract_facts(&text, &source_chat_id, &formation_root, store).await
}

/// Plain-function core of `extract_facts`, callable from other commands
/// (notably `chat_write`) without going through Tauri's `State` injection.
pub async fn run_extract_facts(
    text: &str,
    source_chat_id: &str,
    formation_root: &std::path::Path,
    store: &crate::core::memory::MemoryStore,
) -> AppResult<ExtractFactsResult> {
    let app_dir = formation_root.join(APP_DIR);
    let paths = ModelPaths::under_app_dir(&app_dir);
    if !paths.exist() {
        return Err(AppError::other(
            crate::core::extraction::model_bootstrap_hint(&paths),
        ));
    }

    let extractor = GlinerExtractor::new(paths);
    let schema = default_relation_schema();
    let (entities, relations): (Vec<EntitySpan>, Vec<RelationSpan>) =
        extract_entities_and_relations(&extractor, text, ENTITY_LABELS, &schema)?;

    let mut skipped_low_confidence = 0usize;
    let mut skipped_unresolved_entity = 0usize;

    // Phase 1: upsert entities, build a name → entity_id map for relation lookup.
    let mut name_to_id: HashMap<String, String> = HashMap::new();
    let mut upserted_spans: Vec<UpsertedSpan> = Vec::new();
    for span in entities {
        if span.probability < MIN_ENTITY_CONFIDENCE {
            skipped_low_confidence += 1;
            continue;
        }
        let upserted = store
            .upsert_entity(&span.text, &span.class, Vec::new())
            .await?;
        name_to_id.insert(span.text.clone(), upserted.id.clone());
        upserted_spans.push(UpsertedSpan {
            text: span.text,
            class: span.class,
            probability: span.probability,
            entity_id: upserted.id,
            was_new: upserted.was_new,
        });
    }

    // Phase 2: write facts. Both subject and object texts must resolve to
    // entities we just upserted; otherwise the relation is unresolvable
    // (e.g. RE proposed a subject that NER didn't surface).
    let valid_from = chrono::Utc::now();
    let mut facts_written: Vec<FactWritten> = Vec::new();
    for rel in relations {
        if rel.probability < MIN_RELATION_CONFIDENCE {
            skipped_low_confidence += 1;
            continue;
        }
        let Some(subject_id) = name_to_id.get(&rel.subject).cloned() else {
            skipped_unresolved_entity += 1;
            continue;
        };
        let Some(object_id) = name_to_id.get(&rel.object).cloned() else {
            skipped_unresolved_entity += 1;
            continue;
        };
        let fact_id = store
            .relate_fact(FactWriteInput {
                subject_id,
                predicate: rel.predicate.clone(),
                object_id,
                valid_from,
                source_chat_id: source_chat_id.to_string(),
                confidence: rel.probability as f64,
            })
            .await?;
        facts_written.push(FactWritten {
            fact_id,
            subject: rel.subject,
            predicate: rel.predicate,
            object: rel.object,
            confidence: rel.probability,
        });
    }

    Ok(ExtractFactsResult {
        entities: upserted_spans,
        facts: facts_written,
        skipped_low_confidence,
        skipped_unresolved_entity,
    })
}

/// NER + RE over `text` with **no SurrealDB writes** — the staging-pipeline
/// entry point (Phase 3 decision #7). `chat_write` routes and diffs the
/// returned spans into a `StagingEntry`; nothing is persisted to the graph
/// until the user Keeps. Errors with the bootstrap hint when the GLiNER model
/// files are absent, matching `run_extract_facts`.
pub fn extract_facts_only(
    text: &str,
    formation_root: &std::path::Path,
) -> AppResult<(Vec<EntitySpan>, Vec<RelationSpan>)> {
    let paths = ModelPaths::under_app_dir(&formation_root.join(APP_DIR));
    if !paths.exist() {
        return Err(AppError::other(
            crate::core::extraction::model_bootstrap_hint(&paths),
        ));
    }
    let extractor = GlinerExtractor::new(paths);
    let schema = default_relation_schema();
    extract_entities_and_relations(&extractor, text, ENTITY_LABELS, &schema)
}
