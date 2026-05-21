//! Model provisioning: a launch-time readiness check for the active tier's
//! models, plus streamed downloaders for the two model backends.
//!
//! - Ollama chat / embedding models — pulled via `ollama pull` (streamed).
//! - GLiNER extraction model — downloaded straight from HuggingFace into the
//!   open formation's `.chat-notes/models/`.
//!
//! The UI runs `check_model_readiness` on launch and, if anything is missing,
//! shows a one-click setup screen that drives the two download commands.

use crate::commands::formation::APP_DIR;
use crate::core::extraction::ModelPaths;
use crate::core::formation_state::{AppConfig, FormationState};
use crate::core::hardware::Tier;
use crate::core::models::{models_for_tier, size_hint};
use crate::core::ollama_sidecar::OllamaSidecar;
use crate::error::{AppError, AppResult};
use futures::StreamExt;
use serde::Serialize;
use std::path::Path;
use tauri::ipc::Channel;
use tauri::State;
use tokio::io::AsyncWriteExt;

const GLINER_TOKENIZER_URL: &str =
    "https://huggingface.co/onnx-community/gliner-multitask-large-v0.5/resolve/main/tokenizer.json";
const GLINER_ONNX_URL: &str =
    "https://huggingface.co/onnx-community/gliner-multitask-large-v0.5/resolve/main/onnx/model.onnx";

/// One model the active tier needs, and whether it is installed.
#[derive(Debug, Serialize)]
pub struct ModelRequirement {
    /// `"chat"` | `"embed"` | `"gliner"`.
    pub kind: String,
    /// Ollama pull tag for chat/embed models; `"gliner"` for the ONNX model.
    pub id: String,
    pub label: String,
    pub size_hint: String,
    pub present: bool,
}

/// Result of the launch-time model check for the active tier.
#[derive(Debug, Serialize)]
pub struct ModelReadiness {
    pub tier: String,
    /// False when `ollama` is not on PATH — the chat/embed models can't be
    /// pulled until the user installs Ollama.
    pub ollama_installed: bool,
    pub requirements: Vec<ModelRequirement>,
    pub all_present: bool,
}

/// A progress tick emitted while pulling or downloading a model.
#[derive(Debug, Clone, Serialize)]
pub struct ModelProgress {
    pub model: String,
    /// Human-readable phase ("pulling manifest", "GLiNER model", "complete").
    pub phase: String,
    pub completed: u64,
    pub total: u64,
    pub done: bool,
}

/// Check whether every model the active tier needs is installed. Ollama models
/// are matched against `ollama list`; the GLiNER model against the open
/// formation's `.chat-notes/models/` directory.
#[tauri::command]
pub async fn check_model_readiness(
    formation: State<'_, FormationState>,
    sidecar: State<'_, OllamaSidecar>,
    app: tauri::AppHandle,
) -> AppResult<ModelReadiness> {
    let root = formation.require()?;
    let tier = AppConfig::load(&app)
        .selected_tier
        .as_deref()
        .and_then(Tier::parse)
        .unwrap_or(Tier::Standard);
    let models = models_for_tier(tier);

    // Ollama: probe install, ensure the daemon is up (best-effort), then list.
    let status = sidecar.status().await;
    let local: Vec<String> = if status.installed {
        let _ = sidecar.ensure_running().await;
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

    let mut requirements = Vec::new();
    if let Some(chat) = models.chat {
        requirements.push(ModelRequirement {
            kind: "chat".into(),
            id: chat.into(),
            label: format!("Chat model · {chat}"),
            size_hint: size_hint(chat).into(),
            present: has(chat),
        });
    }
    requirements.push(ModelRequirement {
        kind: "embed".into(),
        id: models.embed.into(),
        label: format!("Embedding model · {}", models.embed),
        size_hint: size_hint(models.embed).into(),
        present: has(models.embed),
    });
    if models.needs_gliner {
        let paths = ModelPaths::under_app_dir(&root.join(APP_DIR));
        requirements.push(ModelRequirement {
            kind: "gliner".into(),
            id: "gliner".into(),
            label: "Extraction model · GLiNER multitask".into(),
            size_hint: "~1.6 GB".into(),
            present: paths.exist(),
        });
    }

    let all_present = requirements.iter().all(|r| r.present);
    Ok(ModelReadiness {
        tier: format!("{tier:?}"),
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
) -> AppResult<()> {
    sidecar.ensure_running().await?;
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

/// Download the GLiNER multitask ONNX model into the open formation's
/// `.chat-notes/models/`, streaming progress. Idempotent — files already on
/// disk are skipped.
#[tauri::command]
pub async fn download_gliner_model(
    on_progress: Channel<ModelProgress>,
    formation: State<'_, FormationState>,
) -> AppResult<()> {
    let root = formation.require()?;
    let paths = ModelPaths::under_app_dir(&root.join(APP_DIR));

    download_file(
        GLINER_TOKENIZER_URL,
        &paths.tokenizer(),
        "GLiNER tokenizer",
        &on_progress,
    )
    .await?;
    download_file(GLINER_ONNX_URL, &paths.onnx(), "GLiNER model", &on_progress).await?;

    let _ = on_progress.send(ModelProgress {
        model: "gliner".into(),
        phase: "complete".into(),
        completed: 0,
        total: 0,
        done: true,
    });
    Ok(())
}

/// Stream `url` to `dest` via a `.part` temp file + rename. Emits a progress
/// tick every ~4 MB. Skips the download entirely if `dest` already exists.
async fn download_file(
    url: &str,
    dest: &Path,
    label: &str,
    channel: &Channel<ModelProgress>,
) -> AppResult<()> {
    if dest.is_file() {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let resp = reqwest::get(url)
        .await
        .map_err(|e| AppError::other(format!("download {label}: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::other(format!(
            "download {label}: HTTP {}",
            resp.status()
        )));
    }
    let total = resp.content_length().unwrap_or(0);

    let tmp = dest.with_extension("part");
    let mut file = tokio::fs::File::create(&tmp).await?;
    let mut stream = resp.bytes_stream();
    let mut completed: u64 = 0;
    let mut last_emit: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| AppError::other(format!("download {label}: {e}")))?;
        file.write_all(&chunk).await?;
        completed += chunk.len() as u64;
        if completed - last_emit >= 4_000_000 {
            last_emit = completed;
            let _ = channel.send(ModelProgress {
                model: "gliner".into(),
                phase: label.into(),
                completed,
                total,
                done: false,
            });
        }
    }
    file.flush().await?;
    drop(file);
    tokio::fs::rename(&tmp, dest).await?;
    Ok(())
}
