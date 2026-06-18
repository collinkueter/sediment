use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::Manager;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormationNote {
    pub relative_path: String,
    /// Last modified time as a Unix epoch seconds value, for stable JSON serialisation.
    pub modified_secs: i64,
}

#[derive(Debug, Default)]
pub struct FormationState {
    inner: Mutex<Option<PathBuf>>,
}

impl FormationState {
    pub fn set(&self, path: PathBuf) {
        *self.inner.lock().expect("formation state poisoned") = Some(path);
    }

    pub fn get(&self) -> Option<PathBuf> {
        self.inner.lock().expect("formation state poisoned").clone()
    }

    pub fn require(&self) -> Result<PathBuf, crate::error::AppError> {
        self.get()
            .ok_or_else(|| crate::error::AppError::Other("no formation is open".into()))
    }
}

/// On-disk app config saved at `$APP_CONFIG_DIR/config.json`. Holds what we
/// need to restore on next launch plus the user's conversational-engine choice.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AppConfig {
    pub last_formation_path: Option<PathBuf>,
    #[serde(default)]
    pub onboarding_complete: bool,
    /// User-chosen directory for downloaded models. `None` keeps Ollama's own
    /// storage location; when set a Sediment-spawned Ollama daemon stores
    /// models under `<models_dir>/ollama` (shared across formations).
    #[serde(default)]
    pub models_dir: Option<PathBuf>,
    /// The conversational-agent engine (ADR-0009 §5, ADR-0012): `"claude-code"`
    /// (default) or `"copilot"`. `None` is treated as `"claude-code"`.
    #[serde(default)]
    pub conversation_engine: Option<String>,
    /// Model alias for the Claude Code engine (`"sonnet"`, `"opus"`, …) or a
    /// full model id. `None` falls back to `core::claude_code::DEFAULT_MODEL`.
    #[serde(default)]
    pub claude_code_model: Option<String>,
    /// Model for the GitHub Copilot engine (`"claude-haiku-4.5"`, `"gpt-5-mini"`,
    /// …). `None` falls back to `core::copilot::DEFAULT_MODEL` (ADR-0012).
    #[serde(default)]
    pub copilot_model: Option<String>,
    /// How note search is powered: `"ollama"` (default — semantic search via the
    /// local embedding model) or `"none"` (keyword/BM25 search, no local model).
    /// `None` resolves to `"ollama"`. See `core::embedding::EmbeddingProvider`.
    #[serde(default)]
    pub embedding_provider: Option<String>,
    /// The Agent's conversational tone: `"stoic"`, `"warm"` (default), or
    /// `"sassy"`. A parameter of the one behaviour prompt — it changes reply
    /// wording only, never what gets recorded. `None` resolves to `"warm"`.
    /// See `core::agent_tone::AgentTone`.
    #[serde(default)]
    pub agent_tone: Option<String>,
    /// Base URL the on-device model files are downloaded from during setup
    /// (ADR-0016). `None` uses the Hugging Face default; set it to a mirror for
    /// locked-down environments. The five files are fetched from
    /// `<base>/<file>`. Also overridable via the `SEDIMENT_MODEL_BASE_URL`
    /// environment variable. See `commands::models::download_bundled_model`.
    #[serde(default)]
    pub bundled_model_url: Option<String>,
}

impl AppConfig {
    pub fn config_file(app_handle: &tauri::AppHandle) -> Result<PathBuf, tauri::Error> {
        let dir = app_handle.path().app_config_dir()?;
        std::fs::create_dir_all(&dir).ok();
        Ok(dir.join("config.json"))
    }

    pub fn load(app_handle: &tauri::AppHandle) -> Self {
        let path = match Self::config_file(app_handle) {
            Ok(p) => p,
            Err(_) => return Self::default(),
        };
        let Ok(bytes) = std::fs::read(&path) else {
            return Self::default();
        };
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    pub fn save(&self, app_handle: &tauri::AppHandle) -> Result<(), crate::error::AppError> {
        let path = Self::config_file(app_handle)?;
        let bytes = serde_json::to_vec_pretty(self)?;
        atomic_write(&path, &bytes)
    }
}

/// Write `bytes` to `path` via a temp file + rename so a crash mid-write can't truncate the
/// destination. Same filesystem only (which it always is for `<path>.tmp`).
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), crate::error::AppError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|s| s.to_str()).unwrap_or("")
    ));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
