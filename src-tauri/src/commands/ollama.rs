//! Tauri commands exposing Ollama lifecycle, model listing, and streaming
//! completions. Streaming uses Tauri 2's `Channel<T>` so each invocation gets
//! its own ordered token pipe back to the JS side.

use crate::core::ollama_sidecar::{OllamaSidecar, OllamaStatus};
use crate::error::{AppError, AppResult};
use futures::StreamExt;
use ollama_rs::generation::completion::request::GenerationRequest;
use ollama_rs::models::LocalModel;
use serde::Serialize;
use tauri::ipc::Channel;
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
pub async fn ollama_ensure_running(sidecar: State<'_, OllamaSidecar>) -> AppResult<OllamaStatus> {
    sidecar.ensure_running().await
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

/// Stream a completion. Tokens arrive on `on_token`. Returns once the model
/// signals `done` (or on error). The JS side accumulates tokens into a bubble.
#[tauri::command]
pub async fn ollama_generate(
    model: String,
    prompt: String,
    on_token: Channel<String>,
    sidecar: State<'_, OllamaSidecar>,
) -> AppResult<()> {
    let client = sidecar.client();
    let request = GenerationRequest::new(model, prompt);
    let mut stream = client
        .generate_stream(request)
        .await
        .map_err(|e| AppError::other(format!("start generation: {e}")))?;

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.map_err(|e| AppError::other(format!("stream error: {e}")))?;
        for response in chunk {
            if !response.response.is_empty() {
                on_token
                    .send(response.response)
                    .map_err(|e| AppError::other(format!("channel send: {e}")))?;
            }
        }
    }
    Ok(())
}
