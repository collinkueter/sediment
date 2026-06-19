//! Ollama lifecycle: detect installation, ensure `ollama serve` is running, and
//! own the `Ollama` client used by chat / embedding / pull commands.
//!
//! Two modes, selected by [`OllamaSidecar::set_endpoint`]:
//!   - **Local (default)** — Ollama is installed natively. Auto-spawn philosophy:
//!     if `ollama` is on PATH and not currently listening, we `Command::spawn()`
//!     it once and drop the handle. The child reparents to init when our process
//!     exits, so Ollama outlives the app cleanly.
//!   - **External** — the user runs Ollama themselves and points Sediment at the
//!     endpoint (e.g. Ollama in Docker/Podman, or a remote host). This is for
//!     locked-down environments where direct model downloads are blocked but a
//!     container image can be pulled through approved channels. In this mode we
//!     never require the `ollama` binary on PATH and never spawn anything — we
//!     only speak HTTP to the URL the user gave us.

use crate::core::cli_launch;
use crate::error::{AppError, AppResult};
use ollama_rs::generation::embeddings::request::GenerateEmbeddingsRequest;
use ollama_rs::Ollama;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

/// Default embedding model. nomic-embed-text emits 768-d vectors that match the
/// HNSW index dimension in core::memory.
pub const DEFAULT_EMBED_MODEL: &str = "nomic-embed-text";

/// Where Ollama lives when the user hasn't pointed Sediment at a custom endpoint.
const DEFAULT_ENDPOINT: &str = "http://localhost:11434";
const SPAWN_WAIT_MS: u64 = 8000;
const POLL_INTERVAL_MS: u64 = 200;

/// Connection + installation status surfaced to the React side.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OllamaStatus {
    /// Local mode: `ollama` is on PATH. External mode: the configured endpoint
    /// answers (there is no binary to find — reachability is the readiness test).
    pub installed: bool,
    pub running: bool,
    /// What to do when Ollama isn't ready — install the binary (local mode) or
    /// start the container/service serving the endpoint (external mode).
    pub install_hint: Option<String>,
}

/// Where Ollama is reached and whether Sediment manages its lifecycle.
#[derive(Debug, Clone)]
struct Endpoint {
    /// Normalised base URL (`scheme://host[:port]`, no trailing slash).
    base_url: String,
    /// True when the user pointed Sediment at an Ollama they run themselves
    /// (Docker/Podman/remote). In that mode we never require the binary and
    /// never spawn — we only talk HTTP.
    external: bool,
}

impl Default for Endpoint {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_ENDPOINT.to_string(),
            external: false,
        }
    }
}

#[derive(Default)]
pub struct OllamaSidecar {
    /// The endpoint Sediment talks to. Interior-mutable so the managed singleton
    /// can be reconfigured at runtime when the user edits the setting.
    endpoint: Mutex<Endpoint>,
}

impl OllamaSidecar {
    /// Point the sidecar at a user-supplied endpoint (e.g. Ollama in Docker), or
    /// clear it (`None` / blank) to fall back to the local default. A `Some`
    /// value switches the sidecar into external mode — no binary requirement, no
    /// auto-spawn. Invalid input falls back to the local default.
    pub fn set_endpoint(&self, url: Option<String>) {
        let endpoint = match url.as_deref().and_then(normalize_endpoint) {
            Some(base_url) => Endpoint {
                base_url,
                external: true,
            },
            None => Endpoint::default(),
        };
        *self.endpoint.lock().expect("ollama endpoint poisoned") = endpoint;
    }

    fn endpoint(&self) -> Endpoint {
        self.endpoint
            .lock()
            .expect("ollama endpoint poisoned")
            .clone()
    }

    /// Probe readiness. Pure read; never spawns.
    pub async fn status(&self) -> OllamaStatus {
        let endpoint = self.endpoint();
        let running = is_running(&endpoint.base_url).await;
        if endpoint.external {
            // User-managed (Docker/Podman/remote): there is no local binary to
            // detect, so reachability of the endpoint IS the readiness test.
            OllamaStatus {
                installed: running,
                running,
                install_hint: if running {
                    None
                } else {
                    Some(format!(
                        "Can't reach the Ollama endpoint at {}. Start the container/service serving it \
                         (e.g. `docker run -d -p 11434:11434 ollama/ollama`) and pull the embedding model.",
                        endpoint.base_url
                    ))
                },
            }
        } else {
            let installed = is_installed();
            OllamaStatus {
                installed,
                running,
                install_hint: if installed {
                    None
                } else {
                    Some(
                        "Install Ollama from https://ollama.com/download and re-launch."
                            .to_string(),
                    )
                },
            }
        }
    }

    /// Ensure Ollama is reachable.
    ///
    /// External mode: never spawns — just probes the configured endpoint and
    /// errors with actionable guidance if it doesn't answer. `models_dir` is
    /// ignored (the user's container owns its own storage).
    ///
    /// Local mode: spawns `ollama serve` if needed and polls the health endpoint
    /// until it answers (or 8s pass). `models_dir`, when set, is exported as
    /// `OLLAMA_MODELS` so a daemon WE spawn stores models there. It has no effect
    /// when Ollama is already running — an existing daemon keeps whatever storage
    /// location it started with.
    pub async fn ensure_running(&self, models_dir: Option<PathBuf>) -> AppResult<OllamaStatus> {
        let endpoint = self.endpoint();
        if endpoint.external {
            if is_running(&endpoint.base_url).await {
                return Ok(self.status().await);
            }
            return Err(AppError::other(format!(
                "Ollama endpoint {} is not reachable. Start the container/service serving it and try again.",
                endpoint.base_url
            )));
        }

        if !is_installed() {
            return Err(AppError::other(
                "Ollama not found on PATH. Install from https://ollama.com/download.",
            ));
        }
        if is_running(&endpoint.base_url).await {
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
            if is_running(&endpoint.base_url).await {
                return Ok(self.status().await);
            }
        }
        Err(AppError::other(
            "Spawned `ollama serve` but health endpoint never responded.",
        ))
    }

    /// Build a client for the configured endpoint. Cheap (just URL parsing on a
    /// shared `reqwest::Client`); doesn't spawn the daemon — callers should call
    /// `ensure_running` first if they need a guarantee.
    pub fn client(&self) -> Ollama {
        Ollama::try_new(self.endpoint().base_url).unwrap_or_default()
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

/// The effective endpoint override: the `SEDIMENT_OLLAMA_URL` env var wins (so a
/// locked-down deployment can force it), else the persisted `AppConfig.ollama_url`
/// passed in. `None` means use the local default (auto-spawn). Blank values are
/// treated as unset.
pub fn resolved_endpoint(config_value: Option<String>) -> Option<String> {
    if let Ok(env) = std::env::var("SEDIMENT_OLLAMA_URL") {
        let env = env.trim();
        if !env.is_empty() {
            return Some(env.to_string());
        }
    }
    config_value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Validate and normalise a user-supplied endpoint, returning the base URL or an
/// error message. Used by the `set_ollama_url` settings command so bad input is
/// rejected with feedback instead of silently falling back.
pub fn validate_endpoint(raw: &str) -> Result<String, String> {
    let base = normalize_endpoint(raw).ok_or_else(|| "Endpoint is empty.".to_string())?;
    Ollama::try_new(base.clone()).map_err(|e| format!("Invalid Ollama endpoint: {e}"))?;
    Ok(base)
}

/// Normalise a user-supplied endpoint into a base URL: ensure a scheme (default
/// `http://`) and drop any trailing slash. Returns `None` for blank input.
fn normalize_endpoint(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let with_scheme = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("http://{raw}")
    };
    Some(with_scheme.trim_end_matches('/').to_string())
}

fn is_installed() -> bool {
    // `which ollama` on macOS/Linux, `where ollama` on Windows. Ollama installs a
    // native `ollama.exe` on PATH there, so the daemon spawn (`Command::new("ollama")`)
    // works directly once it is found.
    cli_launch::is_on_path("ollama")
}

async fn is_running(base_url: &str) -> bool {
    // Short timeout so the UI doesn't stall while we probe.
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let url = format!("{base_url}/api/tags");
    client.get(&url).send().await.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_endpoints() {
        assert_eq!(
            normalize_endpoint("localhost:11434").as_deref(),
            Some("http://localhost:11434")
        );
        assert_eq!(
            normalize_endpoint("http://host:11434/").as_deref(),
            Some("http://host:11434")
        );
        assert_eq!(
            normalize_endpoint("https://ollama.internal:443").as_deref(),
            Some("https://ollama.internal:443")
        );
        assert_eq!(normalize_endpoint("   ").as_deref(), None);
    }

    #[test]
    fn validates_endpoints() {
        assert!(validate_endpoint("localhost:11434").is_ok());
        assert_eq!(
            validate_endpoint("localhost:11434").unwrap(),
            "http://localhost:11434"
        );
        assert!(validate_endpoint("").is_err());
        assert!(validate_endpoint("http://").is_err());
    }

    #[test]
    fn set_endpoint_toggles_external_mode() {
        let sidecar = OllamaSidecar::default();
        assert!(!sidecar.endpoint().external);
        assert_eq!(sidecar.endpoint().base_url, DEFAULT_ENDPOINT);

        sidecar.set_endpoint(Some("dockerhost:11434".to_string()));
        let ep = sidecar.endpoint();
        assert!(ep.external);
        assert_eq!(ep.base_url, "http://dockerhost:11434");

        // Blank clears back to the local default.
        sidecar.set_endpoint(Some("  ".to_string()));
        assert!(!sidecar.endpoint().external);
        sidecar.set_endpoint(None);
        assert!(!sidecar.endpoint().external);
    }
}
