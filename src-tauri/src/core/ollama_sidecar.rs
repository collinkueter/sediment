//! Ollama lifecycle: detect installation, ensure `ollama serve` is running, and
//! own the long-lived `Ollama` client used by chat / embedding / pull commands.
//!
//! Auto-spawn philosophy: if `ollama` is on PATH and not currently listening,
//! we `Command::spawn()` it once and drop the handle. The child reparents to
//! init when our process exits, so Ollama outlives the app cleanly — restart
//! cost on the next launch is amortised.

use crate::error::{AppError, AppResult};
use ollama_rs::generation::embeddings::request::GenerateEmbeddingsRequest;
use ollama_rs::Ollama;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;
use tokio::sync::OnceCell;

/// Default embedding model. nomic-embed-text emits 768-d vectors that match the
/// HNSW index dimension in core::memory.
pub const DEFAULT_EMBED_MODEL: &str = "nomic-embed-text";

const HEALTH_URL: &str = "http://localhost:11434/api/tags";
const SPAWN_WAIT_MS: u64 = 8000;
const POLL_INTERVAL_MS: u64 = 200;

/// Connection + installation status surfaced to the React side.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OllamaStatus {
    pub installed: bool,
    pub running: bool,
    /// First-line install hint when `ollama` isn't on PATH.
    pub install_hint: Option<String>,
}

#[derive(Default)]
pub struct OllamaSidecar {
    /// Lazily-constructed shared client. Cheap to clone (it's just `reqwest::Client` inside).
    client: OnceCell<Ollama>,
}

impl OllamaSidecar {
    /// Probe local install + running state. Pure read; never spawns.
    pub async fn status(&self) -> OllamaStatus {
        let installed = is_installed();
        let running = is_running().await;
        OllamaStatus {
            installed,
            running,
            install_hint: if installed {
                None
            } else {
                Some("Install Ollama from https://ollama.com/download and re-launch.".to_string())
            },
        }
    }

    /// Ensure `ollama serve` is reachable. Spawns the daemon if needed and
    /// polls the health endpoint until it answers (or 8s pass).
    ///
    /// `models_dir`, when set, is exported as `OLLAMA_MODELS` so a daemon WE
    /// spawn stores models there (the user's shared models directory). It has
    /// no effect when Ollama is already running — an existing daemon (system
    /// service, menu-bar app) keeps whatever storage location it started with.
    pub async fn ensure_running(&self, models_dir: Option<PathBuf>) -> AppResult<OllamaStatus> {
        if !is_installed() {
            return Err(AppError::other(
                "Ollama not found on PATH. Install from https://ollama.com/download.",
            ));
        }
        if is_running().await {
            return Ok(self.status().await);
        }
        // Spawn-and-detach. We don't keep the Child handle — Ollama is a long-
        // lived server we want to outlive any one app launch.
        let mut cmd = Command::new("ollama");
        cmd.arg("serve")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null());
        if let Some(dir) = models_dir {
            std::fs::create_dir_all(&dir).ok();
            cmd.env("OLLAMA_MODELS", &dir);
        }
        cmd.spawn()
            .map_err(|e| AppError::other(format!("spawn ollama serve: {e}")))?;

        let total_attempts = SPAWN_WAIT_MS / POLL_INTERVAL_MS;
        for _ in 0..total_attempts {
            tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
            if is_running().await {
                return Ok(self.status().await);
            }
        }
        Err(AppError::other(
            "Spawned `ollama serve` but health endpoint never responded.",
        ))
    }

    /// Get (or lazily initialise) the shared client. Doesn't spawn the daemon —
    /// callers should call `ensure_running` first if they need a guarantee.
    pub fn client(&self) -> &Ollama {
        // Synchronous init is fine because Ollama::default() is just URL parsing.
        self.client.get_or_init_blocking(Ollama::default)
    }

    /// Embed `text` with the configured model. Returns a single dense vector.
    pub async fn embed(&self, model: &str, text: &str) -> AppResult<Vec<f32>> {
        let client = self.client();
        let request = GenerateEmbeddingsRequest::new(model.to_string(), text.into());
        let response = client
            .generate_embeddings(request)
            .await
            .map_err(|e| AppError::other(format!("embed: {e}")))?;
        response
            .embeddings
            .into_iter()
            .next()
            .ok_or_else(|| AppError::other("Ollama returned no embedding vectors"))
    }
}

trait OnceCellSyncInit<T> {
    fn get_or_init_blocking<F: FnOnce() -> T>(&self, init: F) -> &T;
}

impl<T> OnceCellSyncInit<T> for OnceCell<T> {
    fn get_or_init_blocking<F: FnOnce() -> T>(&self, init: F) -> &T {
        if let Some(v) = self.get() {
            return v;
        }
        // `set` only fails if the cell was filled by a concurrent racer — that's fine.
        let _ = self.set(init());
        self.get().expect("OnceCell set or pre-populated")
    }
}

fn is_installed() -> bool {
    Command::new("which")
        .arg("ollama")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

async fn is_running() -> bool {
    // Short timeout so the UI doesn't stall while we probe.
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    client.get(HEALTH_URL).send().await.is_ok()
}
