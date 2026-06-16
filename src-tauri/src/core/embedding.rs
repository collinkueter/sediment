//! How note search is powered.
//!
//! Sediment defaults to semantic search via a local Ollama embedding model
//! (`nomic-embed-text`), but supports a fully local, no-model alternative:
//! keyword/BM25 search over the same `note_chunk.text` (SurrealDB's built-in
//! full-text index). The provider selects which path the three embedding
//! call sites take — indexing, the deterministic pre-pass, and the agent's
//! `search_notes` tool.

/// The note-search backend the user has chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingProvider {
    /// Local Ollama embedding model — semantic (vector) search.
    Ollama,
    /// No embedding model — keyword/BM25 search only.
    None,
}

impl EmbeddingProvider {
    /// Parse the persisted `AppConfig.embedding_provider` string. `None`/unknown
    /// resolves to the default (`Ollama`); `"none"`/`"keyword"` selects keyword.
    pub fn from_config(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some("none") | Some("keyword") => Self::None,
            _ => Self::Ollama,
        }
    }

    /// The canonical config/env string for this provider.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ollama => "ollama",
            Self::None => "none",
        }
    }

    /// True when this provider uses the local embedding model (vector search).
    pub fn is_semantic(self) -> bool {
        matches!(self, Self::Ollama)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_config_strings_with_default() {
        assert_eq!(
            EmbeddingProvider::from_config(None),
            EmbeddingProvider::Ollama
        );
        assert_eq!(
            EmbeddingProvider::from_config(Some("ollama")),
            EmbeddingProvider::Ollama
        );
        assert_eq!(
            EmbeddingProvider::from_config(Some("none")),
            EmbeddingProvider::None
        );
        assert_eq!(
            EmbeddingProvider::from_config(Some("keyword")),
            EmbeddingProvider::None
        );
        // Unknown values fall back to the semantic default.
        assert_eq!(
            EmbeddingProvider::from_config(Some("banana")),
            EmbeddingProvider::Ollama
        );
    }

    #[test]
    fn round_trips_as_str() {
        assert_eq!(EmbeddingProvider::Ollama.as_str(), "ollama");
        assert_eq!(EmbeddingProvider::None.as_str(), "none");
        assert!(EmbeddingProvider::Ollama.is_semantic());
        assert!(!EmbeddingProvider::None.is_semantic());
    }
}
