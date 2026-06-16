//! Model provisioning: a launch-time readiness check for the local embedding
//! model, plus a streamed Ollama model downloader.
//!
//! ADR-0009 retired GLiNER and the hardware-tier strategy; the agent runs on an
//! external CLI. The only model Sediment still provisions locally is the Ollama
//! embedding model (`nomic-embed-text`) that backs `search_notes` retrieval.
//!
//! The UI runs `check_model_readiness` on launch and, if the embedding model is
//! missing, shows a one-click setup screen that drives `pull_ollama_model`.

use crate::core::formation_state::AppConfig;
use crate::core::ollama_sidecar::{OllamaSidecar, DEFAULT_EMBED_MODEL};
use crate::error::{AppError, AppResult};
use futures::StreamExt;
use serde::Serialize;
use std::path::PathBuf;
use tauri::ipc::Channel;
use tauri::State;

/// The directory a Sediment-spawned Ollama daemon should use for its models —
/// `<models_dir>/ollama` when a shared models directory is configured, else
/// `None` so Ollama keeps its own default location.
pub(crate) fn ollama_models_dir(app: &tauri::AppHandle) -> Option<PathBuf> {
    AppConfig::load(app)
        .models_dir
        .map(|dir| dir.join("ollama"))
}

/// One model Sediment needs locally, and whether it is installed.
#[derive(Debug, Serialize)]
pub struct ModelRequirement {
    /// `"embed"` — the only local model class after ADR-0009.
    pub kind: String,
    /// Ollama pull tag.
    pub id: String,
    pub label: String,
    pub size_hint: String,
    pub present: bool,
}

/// Result of the launch-time model check.
#[derive(Debug, Serialize)]
pub struct ModelReadiness {
    /// False when `ollama` is not on PATH — the embedding model can't be
    /// pulled until the user installs Ollama.
    pub ollama_installed: bool,
    pub requirements: Vec<ModelRequirement>,
    pub all_present: bool,
}

/// A progress tick emitted while pulling a model.
#[derive(Debug, Clone, Serialize)]
pub struct ModelProgress {
    pub model: String,
    /// Human-readable phase ("pulling manifest", "complete").
    pub phase: String,
    pub completed: u64,
    pub total: u64,
    pub done: bool,
}

/// Check whether the local embedding model is installed, matched against
/// `ollama list`.
#[tauri::command]
pub async fn check_model_readiness(
    sidecar: State<'_, OllamaSidecar>,
    app: tauri::AppHandle,
) -> AppResult<ModelReadiness> {
    // Ollama: probe install, ensure the daemon is up (best-effort), then list.
    let status = sidecar.status().await;
    let local: Vec<String> = if status.installed {
        let _ = sidecar.ensure_running(ollama_models_dir(&app)).await;
        sidecar
            .client()
            .list_local_models()
            .await
            .map(|v| v.into_iter().map(|m| m.name).collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    // Ollama lists an untagged pull as `<name>:latest`.
    let has = |req: &str| {
        local
            .iter()
            .any(|n| n == req || *n == format!("{req}:latest"))
    };

    let requirements = vec![ModelRequirement {
        kind: "embed".into(),
        id: DEFAULT_EMBED_MODEL.into(),
        label: format!("Embedding model · {DEFAULT_EMBED_MODEL}"),
        size_hint: "~0.3 GB".into(),
        present: has(DEFAULT_EMBED_MODEL),
    }];

    let all_present = requirements.iter().all(|r| r.present);
    Ok(ModelReadiness {
        ollama_installed: status.installed,
        requirements,
        all_present,
    })
}

/// Pull an Ollama model, streaming progress to `on_progress`. Spawns the
/// Ollama daemon first if needed.
#[tauri::command]
pub async fn pull_ollama_model(
    model: String,
    on_progress: Channel<ModelProgress>,
    sidecar: State<'_, OllamaSidecar>,
    app: tauri::AppHandle,
) -> AppResult<()> {
    sidecar.ensure_running(ollama_models_dir(&app)).await?;
    let mut stream = sidecar
        .client()
        .pull_model_stream(model.clone(), false)
        .await
        .map_err(|e| AppError::other(format!("pull {model}: {e}")))?;

    while let Some(item) = stream.next().await {
        let status = item.map_err(|e| AppError::other(format!("pull {model}: {e}")))?;
        let _ = on_progress.send(ModelProgress {
            model: model.clone(),
            phase: status.message,
            completed: status.completed.unwrap_or(0),
            total: status.total.unwrap_or(0),
            done: false,
        });
    }
    let _ = on_progress.send(ModelProgress {
        model,
        phase: "complete".into(),
        completed: 0,
        total: 0,
        done: true,
    });
    Ok(())
}
