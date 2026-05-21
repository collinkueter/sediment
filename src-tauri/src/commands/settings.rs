//! App-level settings commands. Currently the BYOK (bring-your-own-key) cloud
//! provider configuration — see `core::cloud`.

use crate::core::cloud::CloudProvider;
use crate::core::formation_state::AppConfig;
use crate::error::{AppError, AppResult};
use serde::Serialize;

/// BYOK config as the settings UI sees it. The API key itself is never sent
/// back to the front end — `has_key` only reports whether one is stored.
#[derive(Debug, Serialize)]
pub struct ByokConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub has_key: bool,
}

/// The current BYOK setup, for the settings screen.
#[tauri::command]
pub fn get_byok_config(app: tauri::AppHandle) -> ByokConfig {
    let cfg = AppConfig::load(&app);
    ByokConfig {
        provider: cfg.byok_provider,
        model: cfg.byok_model,
        has_key: cfg
            .byok_api_key
            .map(|k| !k.trim().is_empty())
            .unwrap_or(false),
    }
}

/// Save the BYOK config. A `None` provider clears BYOK entirely (back to local
/// generation). When `provider` is set, the stored key is replaced only if
/// `api_key` is a non-empty string — passing `None`/empty keeps the existing
/// key so the user can change the model without re-entering the secret.
#[tauri::command]
pub fn set_byok_config(
    provider: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
    app: tauri::AppHandle,
) -> AppResult<()> {
    let mut cfg = AppConfig::load(&app);
    match provider {
        None => {
            cfg.byok_provider = None;
            cfg.byok_api_key = None;
            cfg.byok_model = None;
        }
        Some(p) => {
            // Reject an unrecognised provider before persisting it.
            if CloudProvider::parse(&p).is_none() {
                return Err(AppError::other(format!("unknown cloud provider: {p}")));
            }
            cfg.byok_provider = Some(p);
            cfg.byok_model = model.filter(|m| !m.trim().is_empty());
            if let Some(key) = api_key.filter(|k| !k.trim().is_empty()) {
                cfg.byok_api_key = Some(key);
            }
        }
    }
    cfg.save(&app)
}
