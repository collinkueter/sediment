//! Tauri commands exposing Ollama lifecycle and model listing. ADR-0009
//! retired the Ollama chat path; Ollama now backs only note-search embeddings.

use crate::core::ollama_sidecar::{OllamaSidecar, OllamaStatus};
use crate::error::{AppError, AppResult};
use ollama_rs::models::LocalModel;
use serde::Serialize;
use tauri::State;

#[derive(Debug, Serialize)]
pub struct ModelSummary {
    pub name: String,
    pub size: u64,
    pub modified_at: String,
}

impl From<LocalModel> for ModelSummary {
    fn from(m: LocalModel) -> Self {
        Self {
            name: m.name,
            size: m.size,
            modified_at: m.modified_at,
        }
    }
}

/// Read-only probe — does NOT spawn anything.
#[tauri::command]
pub async fn ollama_status(sidecar: State<'_, OllamaSidecar>) -> AppResult<OllamaStatus> {
    Ok(sidecar.status().await)
}

/// Spawn `ollama serve` if needed; block (up to 8s) for it to answer.
#[tauri::command]
pub async fn ollama_ensure_running(
    sidecar: State<'_, OllamaSidecar>,
    app: tauri::AppHandle,
) -> AppResult<OllamaStatus> {
    sidecar
        .ensure_running(crate::commands::models::ollama_models_dir(&app))
        .await
}

/// Installed local models. Empty list is a normal state (nothing pulled yet).
#[tauri::command]
pub async fn ollama_list_models(sidecar: State<'_, OllamaSidecar>) -> AppResult<Vec<ModelSummary>> {
    let client = sidecar.client();
    let models = client
        .list_local_models()
        .await
        .map_err(|e| AppError::other(format!("list models: {e}")))?;
    Ok(models.into_iter().map(Into::into).collect())
}
