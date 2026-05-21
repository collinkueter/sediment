//! Tier → model manifest (spec §5). Maps each hardware tier to the local
//! models the app needs: an Ollama chat model, an Ollama embedding model, and
//! the GLiNER extraction model. The launch-time readiness check
//! (`commands::models`) compares this against what is installed and downloads
//! whatever is missing.

use crate::core::hardware::Tier;
use crate::core::ollama_sidecar::DEFAULT_EMBED_MODEL;

/// The local models a tier requires.
pub struct TierModels {
    /// Ollama chat model tag. `None` for BYOK — generation is cloud-side.
    pub chat: Option<&'static str>,
    /// Ollama embedding model tag — `nomic-embed-text` for every tier.
    pub embed: &'static str,
    /// Whether the GLiNER extraction model is needed. True for every tier —
    /// it is the deterministic CPU extractor, independent of the generative
    /// tier (BYOK still extracts locally).
    pub needs_gliner: bool,
}

/// Models for `tier`, per spec §5's tier table. Qwen 2.5 above Lite keeps the
/// per-tier prompt library in one family.
pub fn models_for_tier(tier: Tier) -> TierModels {
    let chat = match tier {
        Tier::Lite => Some("llama3.2:3b"),
        Tier::Standard => Some("qwen2.5:14b"),
        Tier::Pro => Some("qwen2.5:32b"),
        Tier::Byok => None,
    };
    TierModels {
        chat,
        embed: DEFAULT_EMBED_MODEL,
        needs_gliner: true,
    }
}

/// The chat model to generate with for `tier`. BYOK (and an unset tier) fall
/// back to the Lite model locally, since cloud BYOK generation is not wired
/// up yet.
pub fn chat_model_for_tier(tier: Tier) -> &'static str {
    models_for_tier(tier).chat.unwrap_or("llama3.2:3b")
}

/// Approximate on-disk download size, for the setup screen. Display only.
pub fn size_hint(model: &str) -> &'static str {
    match model {
        "llama3.2:3b" => "~2 GB",
        "qwen2.5:14b" => "~9 GB",
        "qwen2.5:32b" => "~20 GB",
        "nomic-embed-text" => "~0.3 GB",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_tier_has_a_model_set() {
        assert_eq!(models_for_tier(Tier::Lite).chat, Some("llama3.2:3b"));
        assert_eq!(models_for_tier(Tier::Standard).chat, Some("qwen2.5:14b"));
        assert_eq!(models_for_tier(Tier::Pro).chat, Some("qwen2.5:32b"));
        assert_eq!(models_for_tier(Tier::Byok).chat, None);
        // Every tier needs local embeddings + GLiNER.
        for tier in [Tier::Lite, Tier::Standard, Tier::Pro, Tier::Byok] {
            let m = models_for_tier(tier);
            assert_eq!(m.embed, "nomic-embed-text");
            assert!(m.needs_gliner);
        }
        // BYOK falls back to the Lite model for local generation.
        assert_eq!(chat_model_for_tier(Tier::Byok), "llama3.2:3b");
        assert_eq!(chat_model_for_tier(Tier::Pro), "qwen2.5:32b");
    }
}
