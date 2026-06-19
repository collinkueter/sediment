//! On-device ASR + speaker-model provisioning commands (ADR-0017 §2/§6, plan M3).
//!
//! The voice/meeting twin of `commands::models`: a readiness check the live-capture
//! UI gates on, and explicit acquisition flows (network download or offline folder
//! import) for the two on-device models `core::asr_model` owns — the
//! streaming-zipformer transducer and the speaker-embedding model. Acquisition is
//! the *only* place these models touch the network; the capture runtime loads them
//! strictly from disk (ADR-0016 posture). Gated on `local-asr` (validation links
//! the native sherpa-onnx runtime).

use crate::commands::models::ModelProgress;
use crate::core::asr_model;
use crate::error::{AppError, AppResult};
use futures::StreamExt;
use serde::Serialize;
use tauri::ipc::Channel;
use tokio::io::AsyncWriteExt;

/// Whether the on-device transcription + speaker models are installed. The live
/// Session UI gates Start on `all_present`, showing a download/import setup screen
/// when a model is missing instead of opening a Session that can't transcribe.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AsrReadiness {
    pub asr_present: bool,
    pub speaker_present: bool,
    pub all_present: bool,
    /// Human-readable size hint for the setup screen.
    pub size_hint: String,
}

/// Report whether the ASR and speaker models are on disk.
#[tauri::command]
pub async fn check_asr_readiness() -> AppResult<AsrReadiness> {
    let asr = asr_model::asr_present();
    let speaker = asr_model::speaker_present();
    Ok(AsrReadiness {
        asr_present: asr,
        speaker_present: speaker,
        all_present: asr && speaker,
        size_hint: "~0.3 GB".into(),
    })
}

/// Stream one URL to `dest`, emitting byte progress under `label`.
async fn download_file(
    client: &reqwest::Client,
    url: &str,
    dest: &std::path::Path,
    label: &str,
    on_progress: &Channel<ModelProgress>,
) -> AppResult<()> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| AppError::other(format!("download {label}: {e}")))?
        .error_for_status()
        .map_err(|e| AppError::other(format!("download {label}: {e}")))?;
    let total = resp.content_length().unwrap_or(0);
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| AppError::other(format!("create staging dir: {e}")))?;
    }
    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| AppError::other(format!("create {label}: {e}")))?;
    let mut downloaded: u64 = 0;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| AppError::other(format!("download {label}: {e}")))?;
        file.write_all(&chunk)
            .await
            .map_err(|e| AppError::other(format!("write {label}: {e}")))?;
        downloaded += chunk.len() as u64;
        let _ = on_progress.send(ModelProgress {
            model: label.to_string(),
            phase: format!("downloading {label}"),
            completed: downloaded,
            total,
            done: false,
        });
    }
    file.flush()
        .await
        .map_err(|e| AppError::other(format!("flush {label}: {e}")))?;
    Ok(())
}

/// Download both on-device models into Sediment's model directory, streaming
/// per-file byte progress. Files land in staging dirs, are validated by loading a
/// session, then atomically promoted — a partial download never leaves a
/// half-installed model the capture path would trip over.
#[tauri::command]
pub async fn download_asr_model(on_progress: Channel<ModelProgress>) -> AppResult<()> {
    let client = reqwest::Client::new();

    // ASR transducer (four files) → staging → validate → promote.
    let base = asr_model::asr_base_url();
    let asr_staging = asr_model::asr_staging_dir();
    if asr_staging.exists() {
        std::fs::remove_dir_all(&asr_staging)
            .map_err(|e| AppError::other(format!("clear ASR staging: {e}")))?;
    }
    for file in asr_model::ASR_FILES {
        download_file(
            &client,
            &format!("{base}/{file}"),
            &asr_staging.join(file),
            file,
            &on_progress,
        )
        .await?;
    }
    asr_model::promote_asr_staging().await?;

    // Speaker-embedding model (one file) → staging → validate → promote.
    let speaker_staging = asr_model::speaker_staging_dir();
    if speaker_staging.exists() {
        std::fs::remove_dir_all(&speaker_staging)
            .map_err(|e| AppError::other(format!("clear speaker staging: {e}")))?;
    }
    download_file(
        &client,
        &asr_model::speaker_url(),
        &speaker_staging.join(asr_model::SPEAKER_FILE),
        asr_model::SPEAKER_FILE,
        &on_progress,
    )
    .await?;
    asr_model::promote_speaker_staging().await?;

    let _ = on_progress.send(ModelProgress {
        model: asr_model::ASR_MODEL_NAME.into(),
        phase: "complete".into(),
        completed: 0,
        total: 0,
        done: true,
    });
    Ok(())
}

/// Install the ASR model from a user-chosen folder (offline / air-gapped path, and
/// the way to seed from a local copy such as the M0 spike). The folder must contain
/// the four streaming-transducer files; the speaker model is installed too when
/// present in the same folder. Both are validated before install.
#[tauri::command]
pub async fn import_asr_model(source_dir: String) -> AppResult<()> {
    let src = std::path::PathBuf::from(source_dir.trim());
    if !src.is_dir() {
        return Err(AppError::other(format!("Not a folder: {}", src.display())));
    }
    asr_model::import_asr_from_dir(src.clone()).await?;
    // The speaker model is optional in an import folder — install it if it's there.
    if src.join(asr_model::SPEAKER_FILE).is_file() {
        asr_model::import_speaker_from_dir(src).await?;
    }
    Ok(())
}
