//! Write-vs-Ask intent classification.
//!
//! Implements the weighted-signal heuristic from spec §6.3. A pure function:
//! deterministic, instant, unit-testable, and with no LLM failure mode. Slash
//! commands (`/write`, `/ask`) are handled client-side and hard-override this.
//!
//! An LLM-backed tie-breaker for genuinely ambiguous messages is a documented
//! future enhancement; the heuristic covers the overwhelmingly common cases.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Intent {
    Write,
    Ask,
}

#[derive(Debug, Clone, Serialize)]
pub struct IntentResult {
    pub mode: Intent,
    /// 0.0–1.0. Below 0.8 the UI should surface the inferred mode for review.
    pub confidence: f32,
}

/// Interrogative openers — a message starting with one of these is almost
/// certainly a question even without a trailing `?`.
const INTERROGATIVES: &[&str] = &[
    "what", "who", "whom", "whose", "when", "where", "why", "how", "which", "is", "are", "was",
    "were", "do", "does", "did", "can", "could", "should", "will", "would", "has", "have",
];

/// Imperative verbs that signal a query/retrieval rather than a fact to file.
const QUERY_VERBS: &[&str] = &[
    "tell",
    "show",
    "find",
    "list",
    "summarize",
    "summarise",
    "explain",
    "describe",
    "remind",
    "search",
    "lookup",
];

/// Classify a chat message as Write (a fact to file) or Ask (a question).
pub fn classify(message: &str) -> IntentResult {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        // Nothing to act on — default Write, low confidence.
        return IntentResult {
            mode: Intent::Write,
            confidence: 0.5,
        };
    }

    let lower = trimmed.to_lowercase();
    let first_word: String = lower
        .split(|c: char| !c.is_alphanumeric())
        .find(|w| !w.is_empty())
        .unwrap_or("")
        .to_string();

    // Strong signal: a trailing question mark.
    if trimmed.ends_with('?') {
        return IntentResult {
            mode: Intent::Ask,
            confidence: 0.95,
        };
    }

    // Strong: opens with an interrogative word.
    if INTERROGATIVES.contains(&first_word.as_str()) {
        return IntentResult {
            mode: Intent::Ask,
            confidence: 0.85,
        };
    }

    // Moderate: opens with a retrieval imperative ("tell me ...", "list ...").
    if QUERY_VERBS.contains(&first_word.as_str()) {
        return IntentResult {
            mode: Intent::Ask,
            confidence: 0.8,
        };
    }

    // Default: a declarative statement is a fact to file.
    IntentResult {
        mode: Intent::Write,
        confidence: 0.75,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn question_mark_is_ask() {
        let r = classify("Where does Sarah work?");
        assert_eq!(r.mode, Intent::Ask);
        assert!(r.confidence >= 0.9);
    }

    #[test]
    fn interrogative_opener_is_ask() {
        assert_eq!(classify("who is the CTO of Acme").mode, Intent::Ask);
        assert_eq!(
            classify("how many projects does John lead").mode,
            Intent::Ask
        );
    }

    #[test]
    fn query_imperative_is_ask() {
        assert_eq!(classify("tell me about Sarah").mode, Intent::Ask);
        assert_eq!(classify("list all of John's tasks").mode, Intent::Ask);
    }

    #[test]
    fn declarative_is_write() {
        let r = classify("Sarah is the new CTO at Acme.");
        assert_eq!(r.mode, Intent::Write);
        assert_eq!(
            classify("John joined Beta Corp last month").mode,
            Intent::Write
        );
    }

    #[test]
    fn empty_defaults_to_write_low_confidence() {
        let r = classify("   ");
        assert_eq!(r.mode, Intent::Write);
        assert!(r.confidence < 0.8);
    }
}
