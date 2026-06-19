//! App-level settings commands: the conversational-engine selector (ADR-0009 §5,
//! ADR-0012 — Claude Code CLI or GitHub Copilot CLI) and the shared models
//! directory.

use crate::core::agent_tone::AgentTone;
use crate::core::claude_code;
use crate::core::embedding::EmbeddingProvider;
use crate::core::formation_state::AppConfig;
use crate::error::{AppError, AppResult};
use serde::Serialize;
use std::path::PathBuf;

/// The configured shared models directory, or `None` for Ollama's default.
/// A string so the settings UI can show it.
#[tauri::command]
pub fn get_models_dir(app: tauri::AppHandle) -> Option<String> {
    AppConfig::load(&app)
        .models_dir
        .map(|p| p.to_string_lossy().into_owned())
}

/// Set the shared models directory, or clear it (`None` / empty) to fall back
/// to Ollama's default location.
#[tauri::command]
pub fn set_models_dir(dir: Option<String>, app: tauri::AppHandle) -> AppResult<()> {
    let mut cfg = AppConfig::load(&app);
    cfg.models_dir = dir
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
        .map(PathBuf::from);
    cfg.save(&app)
}

// ---- Note-search backend: local embedding model vs keyword (BM25) ----------

/// The configured note-search backend: `"ollama"` (semantic, the default) or
/// `"none"` (keyword/BM25 — no local model). The settings UI reads this to show
/// the current mode and to decide whether the embedding-model setup is needed.
#[tauri::command]
pub fn get_embedding_provider(app: tauri::AppHandle) -> String {
    EmbeddingProvider::from_config(AppConfig::load(&app).embedding_provider.as_deref())
        .as_str()
        .to_string()
}

/// Persist the note-search backend. Accepts `"ollama"`, `"bundled"`, or
/// `"none"` (with `"keyword"` as an alias for `"none"`); else rejected.
#[tauri::command]
pub fn set_embedding_provider(provider: String, app: tauri::AppHandle) -> AppResult<()> {
    let normalized = match provider.trim() {
        "ollama" => "ollama",
        "bundled" => "bundled",
        "none" | "keyword" => "none",
        other => {
            return Err(AppError::other(format!(
                "unknown embedding provider: {other} (expected \"ollama\", \"bundled\", or \"none\")"
            )));
        }
    };
    let mut cfg = AppConfig::load(&app);
    cfg.embedding_provider = Some(normalized.to_string());
    cfg.save(&app)
}

// ---- Ollama endpoint: run-it-yourself (Docker/Podman/remote) ---------------

/// The configured custom Ollama endpoint, or `None` when Sediment manages a
/// local daemon. The `SEDIMENT_OLLAMA_URL` env var takes precedence so a
/// locked-down deployment can force it; the settings UI shows the effective
/// value either way.
#[tauri::command]
pub fn get_ollama_url(app: tauri::AppHandle) -> Option<String> {
    crate::core::ollama_sidecar::resolved_endpoint(AppConfig::load(&app).ollama_url)
}

/// Point Sediment at an Ollama the user runs themselves — e.g. in Docker/Podman,
/// or on a remote host — or clear it (`None` / empty) to fall back to the bundled
/// local daemon. Validates the URL, persists it, and reconfigures the live
/// sidecar so the change takes effect without a restart. A scheme defaults to
/// `http://` when omitted (`"dockerhost:11434"` → `"http://dockerhost:11434"`).
#[tauri::command]
pub fn set_ollama_url(
    url: Option<String>,
    sidecar: tauri::State<'_, crate::core::ollama_sidecar::OllamaSidecar>,
    app: tauri::AppHandle,
) -> AppResult<()> {
    let normalized = match url.map(|u| u.trim().to_string()).filter(|u| !u.is_empty()) {
        Some(raw) => {
            Some(crate::core::ollama_sidecar::validate_endpoint(&raw).map_err(AppError::other)?)
        }
        None => None,
    };
    let mut cfg = AppConfig::load(&app);
    cfg.ollama_url = normalized.clone();
    cfg.save(&app)?;
    // Reconfigure the managed singleton (used by the indexer + Ollama commands).
    // The env var, if set, still wins on the next resolve — but a user editing
    // this setting in the UI is the common case, so honour their value live.
    sidecar.set_endpoint(crate::core::ollama_sidecar::resolved_endpoint(normalized));
    Ok(())
}

// ---- ADR-0009: the conversational-agent engine selector --------------------

/// Probe the user's machine for a `claude` binary and its authentication
/// state. Safe to call at any time — it never spends a generation token. The
/// settings and onboarding UIs call this to show the Claude Code engine's
/// install/login status.
#[tauri::command]
pub async fn detect_claude_code() -> claude_code::ClaudeCodeStatus {
    claude_code::detect().await
}

/// Probe the user's machine for a `copilot` binary (ADR-0012). Copilot has no
/// cheap auth-status command, so this reports install only; login state surfaces
/// on the first turn. Makes no network request.
#[tauri::command]
pub fn detect_copilot() -> crate::core::copilot::CopilotStatus {
    crate::core::copilot::detect()
}

/// The conversational-engine selection the settings UI reads.
#[derive(Debug, Serialize)]
pub struct ConversationEngineConfig {
    /// The active engine: `"claude-code"` (default) or `"copilot"`.
    pub engine: String,
    /// Model alias for the Claude Code engine, or `None` (the default).
    pub claude_code_model: Option<String>,
    /// Model for the GitHub Copilot engine, or `None` (the default).
    pub copilot_model: Option<String>,
}

/// The current conversational-engine setup, for the settings screen. `None`
/// (the field was never written) resolves to the Claude Code default.
#[tauri::command]
pub fn get_conversation_engine(app: tauri::AppHandle) -> ConversationEngineConfig {
    let cfg = AppConfig::load(&app);
    let engine = match cfg.conversation_engine.as_deref() {
        Some("copilot") => "copilot".to_string(),
        _ => "claude-code".to_string(),
    };
    ConversationEngineConfig {
        engine,
        claude_code_model: cfg.claude_code_model,
        copilot_model: cfg.copilot_model,
    }
}

/// Persist the conversational-engine choice and the chosen engine's model.
/// Returns an error when `engine` is not `"claude-code"` or `"copilot"`.
#[tauri::command]
pub fn set_conversation_engine(
    engine: String,
    model: Option<String>,
    app: tauri::AppHandle,
) -> AppResult<()> {
    let engine = engine.trim();
    match engine {
        "claude-code" | "copilot" => {}
        other => {
            return Err(AppError::other(format!(
                "unknown conversation engine: {other} (expected \"claude-code\" or \"copilot\")"
            )));
        }
    }

    let mut cfg = AppConfig::load(&app);
    cfg.conversation_engine = Some(engine.to_string());
    // Trim the model; an empty string means "use the default". The model targets
    // the selected engine's own field.
    let model = model
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty());
    match engine {
        "copilot" => cfg.copilot_model = model,
        _ => cfg.claude_code_model = model,
    }
    cfg.save(&app)
}

// ---- ADR-0009 §8: the Agent's conversational tone -------------------------

/// The Agent's configured tone: `"stoic"`, `"warm"` (default), or `"sassy"`.
/// A parameter of the one behaviour prompt — it changes reply wording only,
/// never what the Agent records. `None`/unrecognised resolves to `"warm"`.
#[tauri::command]
pub fn get_agent_tone(app: tauri::AppHandle) -> String {
    AgentTone::from_config(AppConfig::load(&app).agent_tone.as_deref())
        .as_str()
        .to_string()
}

/// Persist the Agent's tone. Accepts `"stoic"`, `"warm"`, or `"sassy"`; any
/// other value is rejected. The change takes effect on the next turn — Claude
/// re-sends the system prompt each turn, and the warm Copilot session recycles
/// when the tone changes (see `core::copilot`).
#[tauri::command]
pub fn set_agent_tone(tone: String, app: tauri::AppHandle) -> AppResult<()> {
    let normalized = match tone.trim() {
        "stoic" => "stoic",
        "warm" => "warm",
        "sassy" => "sassy",
        other => {
            return Err(AppError::other(format!(
                "unknown agent tone: {other} (expected \"stoic\", \"warm\", or \"sassy\")"
            )));
        }
    };
    let mut cfg = AppConfig::load(&app);
    cfg.agent_tone = Some(normalized.to_string());
    cfg.save(&app)
}
