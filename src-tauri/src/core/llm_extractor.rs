//! LLM-backed fact extraction (ADR-0006).
//!
//! `LlmExtractor` prompts the tier's Ollama chat model for a structured
//! `Extraction` — JSON requested in Ollama's JSON mode and validated by serde.
//! Unlike GLiNER it resolves first-person reference, coreference, and tense.
//!
//! It is fail-soft: any failure (daemon down, model missing, unparseable JSON)
//! falls back to the wrapped `GlinerExtractor`, so the Write pipeline always
//! has an extractor. A malformed response never corrupts a note — it just
//! drops to the deterministic fallback.

use crate::core::extraction::{
    parse_year_start, ExtractedEntity, ExtractedRelation, Extraction, FactExtractor,
    GlinerExtractor, ENTITY_LABELS,
};
use crate::core::memory::slugify;
use crate::error::{AppError, AppResult};
use ollama_rs::generation::completion::request::GenerationRequest;
use ollama_rs::generation::options::GenerationOptions;
use ollama_rs::generation::parameters::FormatType;
use ollama_rs::Ollama;

/// Confidence stamped on every LLM-extracted entity and relation. The LLM does
/// not emit calibrated probabilities; this sits above both `MIN_*` floors so
/// LLM output is never dropped as "low confidence" (those floors exist to tame
/// GLiNER's noisy RE).
const LLM_CONFIDENCE: f32 = 0.9;

/// An Ollama-backed `FactExtractor` with a GLiNER fallback.
pub struct LlmExtractor {
    client: Ollama,
    model: String,
    fallback: GlinerExtractor,
}

impl LlmExtractor {
    pub fn new(client: Ollama, model: String, fallback: GlinerExtractor) -> Self {
        Self {
            client,
            model,
            fallback,
        }
    }

    /// Prompt the model and parse its JSON into an `Extraction`. Errors (rather
    /// than degrading) so the trait impl can decide to fall back.
    async fn extract_via_llm(&self, message: &str) -> AppResult<Extraction> {
        // Temperature 0 + a fixed seed: extraction is a parsing task, not a
        // creative one — determinism beats variety and curbs the model
        // inventing or borrowing entities.
        let request = GenerationRequest::new(self.model.clone(), build_prompt(message))
            .format(FormatType::Json)
            .options(GenerationOptions::default().temperature(0.0).seed(42));
        let response = self
            .client
            .generate(request)
            .await
            .map_err(|e| AppError::other(format!("LLM extraction request: {e}")))?;
        let dto: LlmExtraction = serde_json::from_str(extract_json_object(&response.response))
            .map_err(|e| AppError::other(format!("LLM extraction returned bad JSON: {e}")))?;
        Ok(dto.into_extraction())
    }
}

#[async_trait::async_trait]
impl FactExtractor for LlmExtractor {
    async fn extract_facts(&self, message: &str) -> AppResult<Extraction> {
        match self.extract_via_llm(message).await {
            Ok(extraction) => Ok(extraction),
            Err(e) => {
                tracing::warn!("LLM extraction failed ({e}); falling back to GLiNER");
                self.fallback.extract_facts(message).await
            }
        }
    }
}

/// Build the few-shot extraction prompt. The worked example demonstrates the
/// three things GLiNER cannot do — self-resolution, coreference, tense — and
/// is fenced with an explicit "do not copy" warning, because a small model
/// will otherwise lift entities straight out of the example.
fn build_prompt(message: &str) -> String {
    format!(
        "You extract structured facts from a personal note. Output ONLY a JSON \
object, no prose, of exactly this shape:\n\
{{\"entities\":[{{\"name\":string,\"type\":string,\"is_self\":boolean}}],\
\"relations\":[{{\"subject\":string,\"predicate\":string,\"object\":string,\
\"valid_from\":string|null,\"valid_to\":string|null,\"current\":boolean}}]}}\n\n\
Rules:\n\
- \"type\" is one of: person, organization, project, task, meeting, event, topic, location, date.\n\
- is_self is true ONLY for the note-taker — entities the text calls \"I\", \"me\", \"we\", \"my\", \"our\". Always name that entity \"Me\".\n\
- Resolve pronouns: when \"he\"/\"she\"/\"they\" refers to a named person, use that person's name as the subject or object.\n\
- predicate is a short snake_case verb phrase, e.g. works_at, attended, owns_task, leads, advocates_for, due_on, member_of, manages, knows.\n\
- Every relation's subject and object MUST each match an entity \"name\".\n\
- valid_from / valid_to: a year (\"2019\") or a date the text states, otherwise null.\n\
- current: false when the fact is past or no longer true (\"worked at\", \"used to\", \"former\"), true otherwise.\n\n\
The block below shows ONLY the output format. Its names and facts are \
unrelated to the real note — never copy them into your answer.\n\
[FORMAT EXAMPLE — DO NOT COPY ITS CONTENT]\n\
input: \"Talked to Dana today. She used to work at Stripe until 2022. Remind me to renew my passport.\"\n\
output: {{\"entities\":[{{\"name\":\"Me\",\"type\":\"person\",\"is_self\":true}},{{\"name\":\"Dana\",\"type\":\"person\",\"is_self\":false}},{{\"name\":\"Stripe\",\"type\":\"organization\",\"is_self\":false}},{{\"name\":\"renew passport\",\"type\":\"task\",\"is_self\":false}}],\"relations\":[{{\"subject\":\"Dana\",\"predicate\":\"works_at\",\"object\":\"Stripe\",\"valid_from\":null,\"valid_to\":\"2022\",\"current\":false}},{{\"subject\":\"Me\",\"predicate\":\"owns_task\",\"object\":\"renew passport\",\"valid_from\":null,\"valid_to\":null,\"current\":true}}]}}\n\
[END EXAMPLE]\n\n\
Now extract ONLY from this real note, ignoring the example's content entirely:\n\
\"{message}\"\n\
JSON:"
    )
}

/// Pull the JSON object out of a raw model response. Even in JSON mode a small
/// local model sometimes wraps its answer in a ```` ```json ```` fence or adds
/// a sentence of preamble; slicing from the first `{` to the last `}` recovers
/// the object in every such case (fence backticks and preamble sit outside the
/// braces). Returns the trimmed input unchanged when it holds no object, so a
/// genuinely empty or garbage response still fails the downstream parse loudly.
fn extract_json_object(raw: &str) -> &str {
    let trimmed = raw.trim();
    match (trimmed.find('{'), trimmed.rfind('}')) {
        (Some(start), Some(end)) if end > start => &trimmed[start..=end],
        _ => trimmed,
    }
}

// --- Lenient JSON DTOs -------------------------------------------------------
// A small local model is loose with the schema, so every field is optional /
// defaulted and dates are free-form strings parsed downstream.

#[derive(serde::Deserialize)]
struct LlmExtraction {
    #[serde(default)]
    entities: Vec<LlmEntity>,
    #[serde(default)]
    relations: Vec<LlmRelation>,
}

#[derive(serde::Deserialize)]
struct LlmEntity {
    #[serde(default)]
    name: String,
    #[serde(default, rename = "type")]
    entity_type: String,
    #[serde(default)]
    is_self: bool,
}

#[derive(serde::Deserialize)]
struct LlmRelation {
    #[serde(default)]
    subject: String,
    #[serde(default)]
    predicate: String,
    #[serde(default)]
    object: String,
    #[serde(default)]
    valid_from: Option<String>,
    #[serde(default)]
    valid_to: Option<String>,
    #[serde(default = "default_true")]
    current: bool,
}

fn default_true() -> bool {
    true
}

impl LlmExtraction {
    fn into_extraction(self) -> Extraction {
        let entities = self
            .entities
            .into_iter()
            .filter(|e| !e.name.trim().is_empty())
            .map(|e| ExtractedEntity {
                name: e.name.trim().to_string(),
                entity_type: normalize_entity_type(&e.entity_type),
                is_self: e.is_self,
                confidence: LLM_CONFIDENCE,
            })
            .collect();

        let relations = self
            .relations
            .into_iter()
            .filter(|r| {
                !r.subject.trim().is_empty()
                    && !r.predicate.trim().is_empty()
                    && !r.object.trim().is_empty()
            })
            .filter_map(|r| {
                // A predicate of pure punctuation slugifies to "" — drop the
                // relation rather than emit a verb-less "- " bullet.
                let predicate = slugify(&r.predicate);
                if predicate.is_empty() {
                    return None;
                }
                let valid_from = r.valid_from.as_deref().and_then(parse_when);
                let mut valid_to = r.valid_to.as_deref().and_then(parse_when);
                // A fact flagged not-current must be a closed interval so it
                // renders past-tense and is not a "current" graph edge. If the
                // model gave no end, close it at the start (or now).
                if !r.current && valid_to.is_none() {
                    valid_to = valid_from.or_else(|| Some(chrono::Utc::now()));
                }
                Some(ExtractedRelation {
                    subject: r.subject.trim().to_string(),
                    predicate,
                    object: r.object.trim().to_string(),
                    valid_from,
                    valid_to,
                    confidence: LLM_CONFIDENCE,
                })
            })
            .collect();

        Extraction {
            entities,
            relations,
        }
    }
}

/// Map a model-supplied type onto the canonical `ENTITY_LABELS` set; anything
/// unrecognised falls back to `topic` (a safe catch-all that satisfies the
/// SurrealDB `entity_type` ASSERT).
fn normalize_entity_type(raw: &str) -> String {
    let t = raw.trim().to_lowercase();
    if ENTITY_LABELS.contains(&t.as_str()) {
        t
    } else {
        "topic".to_string()
    }
}

/// Parse a free-form date string the model emitted: "today"/"now", an RFC3339
/// timestamp, a `YYYY-MM-DD` date, or a bare 4-digit year. Unrecognised → None.
fn parse_when(raw: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::TimeZone;
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    if matches!(t.to_lowercase().as_str(), "today" | "now" | "tonight") {
        return Some(chrono::Utc::now());
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(t) {
        return Some(dt.with_timezone(&chrono::Utc));
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(t, "%Y-%m-%d") {
        return d
            .and_hms_opt(0, 0, 0)
            .map(|ndt| chrono::Utc.from_utc_datetime(&ndt));
    }
    parse_year_start(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The DTO mapping is the deterministic half of the LLM extractor — it is
    /// unit-tested without a model. A not-current relation must come out as a
    /// closed interval even when the model gives no explicit `valid_to`.
    #[test]
    fn maps_json_into_extraction_with_tense_and_self() {
        let json = r#"{
          "entities": [
            {"name": "Me", "type": "person", "is_self": true},
            {"name": "Josh", "type": "person", "is_self": false},
            {"name": "Cloudflare", "type": "organization", "is_self": false},
            {"name": "weird", "type": "not-a-real-type", "is_self": false}
          ],
          "relations": [
            {"subject": "Josh", "predicate": "works at", "object": "Cloudflare",
             "valid_from": "2019", "valid_to": null, "current": false},
            {"subject": "Me", "predicate": "attended", "object": "Standup",
             "valid_from": null, "valid_to": null, "current": true}
          ]
        }"#;
        let dto: LlmExtraction = serde_json::from_str(json).expect("parse");
        let ex = dto.into_extraction();

        assert_eq!(ex.entities.len(), 4);
        assert!(ex.entities.iter().any(|e| e.name == "Me" && e.is_self));
        // Unknown type is coerced to the catch-all so commit's ASSERT passes.
        assert_eq!(
            ex.entities
                .iter()
                .find(|e| e.name == "weird")
                .unwrap()
                .entity_type,
            "topic"
        );

        let worked = &ex.relations[0];
        assert_eq!(worked.predicate, "works_at", "predicate is slugified");
        assert!(worked.valid_from.is_some(), "year 2019 parsed");
        assert!(
            worked.valid_to.is_some(),
            "a not-current fact must become a closed interval"
        );

        let attended = &ex.relations[1];
        assert!(
            attended.valid_to.is_none(),
            "a current fact stays open-ended"
        );
    }

    #[test]
    fn parse_when_handles_years_dates_and_today() {
        assert!(parse_when("2019").is_some());
        assert!(parse_when("2026-05-20").is_some());
        assert!(parse_when("today").is_some());
        assert!(parse_when("sometime soon").is_none());
        assert!(parse_when("").is_none());
    }

    /// A small model wraps JSON in a fence or adds preamble even in JSON mode;
    /// `extract_json_object` must recover the object so a good extraction is
    /// not needlessly dropped to the GLiNER fallback (gap G1).
    #[test]
    fn extract_json_object_unwraps_fences_and_preamble() {
        let obj = r#"{"entities":[],"relations":[]}"#;
        assert_eq!(extract_json_object(obj), obj, "a bare object is unchanged");
        assert_eq!(
            extract_json_object("```json\n{\"entities\":[],\"relations\":[]}\n```"),
            obj,
            "a fenced object is unwrapped",
        );
        assert_eq!(
            extract_json_object("Here is the extraction:\n{\"entities\":[],\"relations\":[]}"),
            obj,
            "leading preamble is stripped",
        );
        assert_eq!(
            extract_json_object("  \n{\"entities\":[],\"relations\":[]}\nThanks!  "),
            obj,
            "trailing prose is stripped",
        );
        // No object at all — returned trimmed so the downstream parse fails loudly.
        assert_eq!(extract_json_object("  no json here  "), "no json here");
    }

    /// A fenced response must deserialize end-to-end, not just survive slicing.
    #[test]
    fn fenced_response_deserializes() {
        let raw = "```json\n{\"entities\":[{\"name\":\"Acme\",\"type\":\"organization\",\
                   \"is_self\":false}],\"relations\":[]}\n```";
        let dto: LlmExtraction =
            serde_json::from_str(extract_json_object(raw)).expect("fenced JSON parses");
        let ex = dto.into_extraction();
        assert_eq!(ex.entities.len(), 1);
        assert_eq!(ex.entities[0].name, "Acme");
    }

    /// A predicate that slugifies to "" (pure punctuation) must drop the whole
    /// relation rather than yield a verb-less "- " bullet (gap G3).
    #[test]
    fn relation_with_empty_predicate_is_dropped() {
        let json = r#"{
          "entities": [
            {"name": "A", "type": "person", "is_self": false},
            {"name": "B", "type": "person", "is_self": false}
          ],
          "relations": [
            {"subject": "A", "predicate": "-->", "object": "B", "current": true},
            {"subject": "A", "predicate": "knows", "object": "B", "current": true}
          ]
        }"#;
        let ex: LlmExtraction = serde_json::from_str(json).expect("parse");
        let ex = ex.into_extraction();
        assert_eq!(ex.relations.len(), 1, "only the real predicate survives");
        assert_eq!(ex.relations[0].predicate, "knows");
    }
}
