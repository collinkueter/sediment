//! Model provisioning: a launch-time readiness check for the local embedding
//! model, plus the per-provider acquisition flows.
//!
//! ADR-0009 retired GLiNER and the hardware-tier strategy; the agent runs on an
//! external CLI. What's left to provision locally is the embedding model that
//! backs `search_notes` retrieval, via whichever provider is active:
//!   - **Ollama** — `pull_ollama_model` streams a pull through the daemon.
//!   - **Bundled** (in-process, ADR-0016) — `download_bundled_model` fetches the
//!     model files into Sediment's model dir, or `import_bundled_model` installs
//!     them from a folder (offline). The runtime then loads them locally with no
//!     network.
//!
//! The UI runs `check_model_readiness` on launch; it reports the active provider
//! and whether its model is installed, and shows the matching setup screen when
//! it isn't.

use crate::core::embedding::EmbeddingProvider;
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
    /// The active note-search provider: `"ollama"`, `"bundled"`, or `"none"`.
    /// The setup screen renders a different acquisition flow per provider
    /// (Ollama pull vs on-device download/import).
    pub provider: String,
    /// False when `ollama` is not on PATH — the embedding model can't be
    /// pulled until the user installs Ollama. Always false for non-Ollama
    /// providers.
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
    // Each provider has its own readiness contract:
    //  - None (keyword): no model, always ready.
    //  - Bundled (in-process): the on-device model files must be installed on
    //    disk — readiness reflects their presence so setup gates a missing model
    //    instead of failing later during indexing/search.
    //  - Ollama: probe install + `ollama list` below.
    let provider =
        EmbeddingProvider::from_config(AppConfig::load(&app).embedding_provider.as_deref());
    match provider {
        EmbeddingProvider::None => {
            return Ok(ModelReadiness {
                provider: provider.as_str().into(),
                ollama_installed: false,
                requirements: Vec::new(),
                all_present: true,
            });
        }
        EmbeddingProvider::Bundled => {
            let model_present = crate::core::bundled_embed::present();
            // Under `local-asr` the embedder loads ONNX Runtime dynamically, so the
            // model files alone aren't enough — the runtime lib must be provisioned
            // too. Report it as a requirement so a missing runtime surfaces as
            // not-ready (with an actionable setup screen) instead of a silent
            // "ready" whose every embed then fails.
            #[cfg(feature = "local-asr")]
            let runtime_present = crate::core::ort_runtime::ready();
            #[cfg(not(feature = "local-asr"))]
            let runtime_present = true;
            let mut requirements = vec![ModelRequirement {
                kind: "embed".into(),
                id: "nomic-embed-text-v1.5".into(),
                label: "On-device embedding model · nomic-embed-text-v1.5".into(),
                size_hint: "~0.5 GB".into(),
                present: model_present,
            }];
            if !runtime_present {
                requirements.push(ModelRequirement {
                    kind: "embed".into(),
                    id: "onnxruntime".into(),
                    label: "On-device runtime · ONNX Runtime".into(),
                    size_hint: "~25 MB".into(),
                    present: false,
                });
            }
            return Ok(ModelReadiness {
                provider: provider.as_str().into(),
                ollama_installed: false,
                all_present: model_present && runtime_present,
                requirements,
            });
        }
        EmbeddingProvider::Ollama => {}
    }

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
        provider: EmbeddingProvider::Ollama.as_str().into(),
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

// ---- On-device (bundled) model provisioning (ADR-0016) ---------------------

/// Where the on-device model files are fetched from when no mirror is set. The
/// five files live under this base; see `core::bundled_embed::MODEL_FILES`.
const DEFAULT_MODEL_BASE_URL: &str =
    "https://huggingface.co/nomic-ai/nomic-embed-text-v1.5/resolve/main";

/// The configured download base URL: `SEDIMENT_MODEL_BASE_URL` (env) wins, then
/// `AppConfig.bundled_model_url`, else the Hugging Face default. Trailing slash
/// trimmed so `<base>/<file>` is well-formed.
fn model_base_url(app: &tauri::AppHandle) -> String {
    if let Ok(env) = std::env::var("SEDIMENT_MODEL_BASE_URL") {
        let env = env.trim().trim_end_matches('/');
        if !env.is_empty() {
            return env.to_string();
        }
    }
    AppConfig::load(app)
        .bundled_model_url
        .map(|u| u.trim().trim_end_matches('/').to_string())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL_BASE_URL.to_string())
}

/// Download the on-device embedding model into Sediment's model directory,
/// streaming per-file byte progress through `on_progress`. Files land in a
/// staging directory, are validated by loading a session, then atomically
/// promoted — so a failed or partial download never leaves a half-installed
/// model that indexing would trip over. This is the only place model
/// acquisition reaches the network.
#[tauri::command]
pub async fn download_bundled_model(
    on_progress: Channel<ModelProgress>,
    app: tauri::AppHandle,
) -> AppResult<()> {
    use crate::core::bundled_embed;
    use tokio::io::AsyncWriteExt;
    // `futures::StreamExt` is already in scope at module level (used by
    // `pull_ollama_model`), providing `.next()` on the byte stream below.

    let base = model_base_url(&app);
    let staging = bundled_embed::staging_dir();
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .map_err(|e| AppError::other(format!("clear staging dir: {e}")))?;
    }

    let client = reqwest::Client::new();
    for rel in bundled_embed::MODEL_FILES {
        let url = format!("{base}/{rel}");
        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| AppError::other(format!("download {rel}: {e}")))?
            .error_for_status()
            .map_err(|e| AppError::other(format!("download {rel}: {e}")))?;
        let total = resp.content_length().unwrap_or(0);

        let dest = staging.join(rel);
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| AppError::other(format!("create staging dir: {e}")))?;
        }
        let mut file = tokio::fs::File::create(&dest)
            .await
            .map_err(|e| AppError::other(format!("create {rel}: {e}")))?;

        let mut downloaded: u64 = 0;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| AppError::other(format!("download {rel}: {e}")))?;
            file.write_all(&chunk)
                .await
                .map_err(|e| AppError::other(format!("write {rel}: {e}")))?;
            downloaded += chunk.len() as u64;
            let _ = on_progress.send(ModelProgress {
                model: rel.to_string(),
                phase: format!("downloading {rel}"),
                completed: downloaded,
                total,
                done: false,
            });
        }
        file.flush()
            .await
            .map_err(|e| AppError::other(format!("flush {rel}: {e}")))?;
    }

    // Under `local-asr` the embedder loads ONNX Runtime dynamically; provision the
    // runtime lib now so validation (which loads an ORT session) has it.
    #[cfg(feature = "local-asr")]
    {
        if let Err(e) = crate::core::ort_runtime::ensure().await {
            tracing::warn!("ort runtime provisioning failed: {e}");
        }
    }

    // Validate the staged files (load a session) and install them atomically.
    bundled_embed::promote_staging().await?;
    // Warm the model so the first search doesn't pay the load cost. Non-fatal.
    let _ = bundled_embed::warmup().await;

    let _ = on_progress.send(ModelProgress {
        model: "nomic-embed-text-v1.5".into(),
        phase: "complete".into(),
        completed: 0,
        total: 0,
        done: true,
    });
    Ok(())
}

/// Install the on-device model from a user-chosen folder (the offline path for
/// locked-down environments — no network). The folder must contain the model
/// files, either in the repo layout (`onnx/model.onnx` plus the four JSON files)
/// or flat by basename. Files are validated by loading a session before they are
/// installed, so a bad/incomplete folder is rejected with an actionable error.
#[tauri::command]
pub async fn import_bundled_model(source_dir: String) -> AppResult<()> {
    use crate::core::bundled_embed;
    let src = std::path::PathBuf::from(source_dir.trim());
    if !src.is_dir() {
        return Err(AppError::other(format!("Not a folder: {}", src.display())));
    }
    // Under `local-asr` the embedder validates by loading an ONNX Runtime session,
    // which needs the runtime lib. Prefer one in the same folder (true air-gapped),
    // else fetch it — otherwise the offline import would fail at validation.
    #[cfg(feature = "local-asr")]
    {
        if !crate::core::ort_runtime::ready()
            && crate::core::ort_runtime::import_from_dir(&src).is_err()
        {
            if let Err(e) = crate::core::ort_runtime::ensure().await {
                tracing::warn!("import_bundled_model: ort runtime provisioning failed: {e}");
            }
        }
    }
    bundled_embed::install_from_dir(src).await?;
    let _ = bundled_embed::warmup().await;
    Ok(())
}
