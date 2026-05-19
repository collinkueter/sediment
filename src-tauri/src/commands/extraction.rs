//! Tauri commands exposing the entity extraction pipeline. Phase 1 scope: a
//! smoke command that returns NER spans for a single message + label set.
//! Real Write-pipeline integration lands in Phase 2 once SurrealDB writes are
//! wired into the extraction output.

use crate::commands::formation::APP_DIR;
use crate::core::extraction::{
    EntityExtractor, EntitySpan, GlinerExtractor, ModelPaths, ENTITY_LABELS,
};
use crate::core::formation_state::FormationState;
use crate::core::memory::{MemoryHandle, UpsertedEntity};
use crate::error::{AppError, AppResult};
use serde::Serialize;
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
        // Skip noise: anything below 0.5 prob would create garbage entities.
        if span.probability < 0.5 {
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
