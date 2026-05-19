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

/// On-disk app config saved at `$APP_CONFIG_DIR/config.json`. Holds only the bits we need to
/// restore on next launch; full settings live in a richer struct (added in M5+).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AppConfig {
    pub last_formation_path: Option<PathBuf>,
    #[serde(default)]
    pub onboarding_complete: bool,
    #[serde(default)]
    pub selected_tier: Option<String>,
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
