//! BYOK (bring-your-own-key) cloud generation — tech-spec §15, Phase 5.
//!
//! When the user configures a cloud provider and an API key in settings,
//! Ask-mode answer generation runs against that provider instead of the local
//! Ollama model. It is one non-streaming chat completion per turn: slower to
//! first token than local streaming, but it lets a machine that cannot host a
//! capable local model still produce strong answers. Extraction stays local
//! (ADR-0006) — only generation is cloud-side.

use crate::error::{AppError, AppResult};
use serde::Deserialize;

/// A cloud chat provider the user can bring a key for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudProvider {
    Anthropic,
    OpenAi,
}

impl CloudProvider {
    /// Parse the provider string stored in `AppConfig.byok_provider`.
    /// Case-insensitive; an unrecognised value yields `None`.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "anthropic" => Some(Self::Anthropic),
            "openai" => Some(Self::OpenAi),
            _ => None,
        }
    }

    /// The model to call when the user did not name one explicitly — a small,
    /// fast, inexpensive default for each provider.
    pub fn default_model(&self) -> &'static str {
        match self {
            Self::Anthropic => "claude-3-5-haiku-latest",
            Self::OpenAi => "gpt-4o-mini",
        }
    }
}

/// A fully-resolved BYOK setup: the provider, the key, and the model to call.
#[derive(Debug, Clone)]
pub struct CloudConfig {
    pub provider: CloudProvider,
    pub api_key: String,
    pub model: String,
}

/// Generate an answer for `prompt` via the configured cloud provider. One
/// non-streaming request — the caller forwards the whole answer to the UI.
pub async fn generate(config: &CloudConfig, prompt: &str) -> AppResult<String> {
    let client = reqwest::Client::new();
    let request = match config.provider {
        CloudProvider::Anthropic => client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &config.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&anthropic_body(&config.model, prompt)),
        CloudProvider::OpenAi => client
            .post("https://api.openai.com/v1/chat/completions")
            .header("authorization", format!("Bearer {}", config.api_key))
            .json(&openai_body(&config.model, prompt)),
    };

    let resp = request
        .send()
        .await
        .map_err(|e| AppError::other(format!("cloud request: {e}")))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| AppError::other(format!("cloud response read: {e}")))?;
    if !status.is_success() {
        // A 401/429/400 body usually carries an actionable message.
        return Err(AppError::other(format!(
            "cloud provider returned HTTP {status}: {}",
            text.chars().take(300).collect::<String>()
        )));
    }
    match config.provider {
        CloudProvider::Anthropic => parse_anthropic_answer(&text),
        CloudProvider::OpenAi => parse_openai_answer(&text),
    }
}

/// Anthropic Messages API request body. `max_tokens` is required by the API.
fn anthropic_body(model: &str, prompt: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "max_tokens": 1024,
        "messages": [{ "role": "user", "content": prompt }],
    })
}

/// OpenAI Chat Completions API request body.
fn openai_body(model: &str, prompt: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [{ "role": "user", "content": prompt }],
    })
}

#[derive(Deserialize)]
struct AnthropicResponse {
    #[serde(default)]
    content: Vec<AnthropicBlock>,
}

#[derive(Deserialize)]
struct AnthropicBlock {
    #[serde(default)]
    text: String,
}

/// Pull the answer text out of an Anthropic Messages response, concatenating
/// any text blocks. An empty answer is an error, not a blank reply.
fn parse_anthropic_answer(body: &str) -> AppResult<String> {
    let parsed: AnthropicResponse = serde_json::from_str(body)
        .map_err(|e| AppError::other(format!("parse Anthropic response: {e}")))?;
    let text: String = parsed.content.into_iter().map(|b| b.text).collect();
    if text.trim().is_empty() {
        return Err(AppError::other("Anthropic response carried no text"));
    }
    Ok(text)
}

#[derive(Deserialize)]
struct OpenAiResponse {
    #[serde(default)]
    choices: Vec<OpenAiChoice>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
}

#[derive(Deserialize)]
struct OpenAiMessage {
    #[serde(default)]
    content: String,
}

/// Pull the answer text out of an OpenAI Chat Completions response.
fn parse_openai_answer(body: &str) -> AppResult<String> {
    let parsed: OpenAiResponse = serde_json::from_str(body)
        .map_err(|e| AppError::other(format!("parse OpenAI response: {e}")))?;
    let text = parsed
        .choices
        .into_iter()
        .next()
        .map(|c| c.message.content)
        .unwrap_or_default();
    if text.trim().is_empty() {
        return Err(AppError::other(
            "OpenAI response carried no message content",
        ));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_parse_is_case_insensitive() {
        assert_eq!(
            CloudProvider::parse("Anthropic"),
            Some(CloudProvider::Anthropic)
        );
        assert_eq!(
            CloudProvider::parse("  openai "),
            Some(CloudProvider::OpenAi)
        );
        assert_eq!(CloudProvider::parse("gemini"), None);
    }

    #[test]
    fn request_bodies_match_each_provider_schema() {
        let a = anthropic_body("claude-3-5-haiku-latest", "hello");
        assert_eq!(a["model"], "claude-3-5-haiku-latest");
        assert!(a["max_tokens"].is_number(), "Anthropic requires max_tokens");
        assert_eq!(a["messages"][0]["role"], "user");
        assert_eq!(a["messages"][0]["content"], "hello");

        let o = openai_body("gpt-4o-mini", "hello");
        assert_eq!(o["model"], "gpt-4o-mini");
        assert_eq!(o["messages"][0]["content"], "hello");
    }

    #[test]
    fn parses_a_provider_answer_and_rejects_an_empty_one() {
        let anthropic = r#"{"content":[{"type":"text","text":"Sarah works at Acme."}]}"#;
        assert_eq!(
            parse_anthropic_answer(anthropic).unwrap(),
            "Sarah works at Acme."
        );
        let openai = r#"{"choices":[{"message":{"role":"assistant","content":"Hi there."}}]}"#;
        assert_eq!(parse_openai_answer(openai).unwrap(), "Hi there.");

        // An empty content array is a failed turn, not a blank answer.
        assert!(parse_anthropic_answer(r#"{"content":[]}"#).is_err());
        assert!(parse_openai_answer(r#"{"choices":[]}"#).is_err());
    }
}
