use serde::Serialize;
use thiserror::Error;

// Variants and helpers are stubs used in later milestones (M3+).
#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum AppError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("tauri error: {0}")]
    Tauri(#[from] tauri::Error),

    #[error("{0}")]
    Other(String),
}

#[allow(dead_code)]
impl AppError {
    pub fn other<S: Into<String>>(s: S) -> Self {
        Self::Other(s.into())
    }
}

// Tauri commands must return a serializable error. Stringify everything.
impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

pub type AppResult<T> = std::result::Result<T, AppError>;
