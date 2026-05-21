//! Entity and relation extraction via gline-rs (multitask GLiNER ONNX model).
//!
//! Phase 2 evolved this from NER-only to NER + RE. The extractor holds the
//! lower-level `orp::Model` so a single loaded copy is shared between
//! `TokenPipeline` (NER) and `RelationPipeline` (RE). Pipelines themselves are
//! cheap to construct (just hold tokenizer + schema refs), so we build them on
//! each call.
//!
//! Model files are NOT bundled. The user runs the documented bootstrap once
//! (see `model_bootstrap_hint`) and the extractor lazily loads from
//! `<formation>/.chat-notes/models/`.

use crate::error::{AppError, AppResult};
use gliner::model::input::relation::schema::RelationSchema;
use gliner::model::input::text::TextInput;
use gliner::model::output::decoded::SpanOutput;
use gliner::model::params::Parameters;
use gliner::model::pipeline::relation::RelationPipeline;
use gliner::model::pipeline::token::TokenPipeline;
use orp::model::Model;
use orp::params::RuntimeParameters;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tokio::sync::OnceCell;

/// Canonical entity type labels passed to GLiNER and stored in SurrealDB's
/// `entity.entity_type` (matches the schema ASSERT in core::memory). Keeping
/// these in one place means schema and extractor never disagree.
pub const ENTITY_LABELS: &[&str] = &[
    "person",
    "organization",
    "meeting",
    "project",
    "task",
    "topic",
    "location",
    "date",
    "event",
];

/// Build the Sediment-default relation schema. The multitask GLiNER model
/// supports arbitrary predicate names; the (subject_type, object_type)
/// filters here both prune candidate triples and document the vocab for
/// downstream code.
///
/// Phase 2 starting set per docs/plans/phase-2.md decision section.
pub fn default_relation_schema() -> RelationSchema {
    let mut s = RelationSchema::new();

    // person → organization
    for pred in &[
        "works_at",
        "joined",
        "left",
        "founded",
        "invests_in",
        "advises",
        "member_of",
        "former_member_of",
        "volunteered_at",
    ] {
        s.push_with_allowed_labels(pred, &["person"], &["organization"]);
    }

    // person → person
    for pred in &[
        "knows",
        "reports_to",
        "manages",
        "parent_of",
        "child_of",
        "sibling_of",
        "partner_of",
        "mentored_by",
        "collaborates_with",
    ] {
        s.push_with_allowed_labels(pred, &["person"], &["person"]);
    }

    // person → location
    for pred in &["lives_in", "born_in", "visited"] {
        s.push_with_allowed_labels(pred, &["person"], &["location"]);
    }

    // person → topic
    for pred in &["expert_in", "interested_in", "advocates_for"] {
        s.push_with_allowed_labels(pred, &["person"], &["topic"]);
    }

    // person → meeting
    for pred in &["attended", "organized", "presented_at"] {
        s.push_with_allowed_labels(pred, &["person"], &["meeting"]);
    }

    // person → project
    for pred in &["created", "contributes_to", "leads"] {
        s.push_with_allowed_labels(pred, &["person"], &["project"]);
    }

    // person → task
    for pred in &["owns_task", "completed", "delegated_to"] {
        s.push_with_allowed_labels(pred, &["person"], &["task"]);
    }

    // org → org
    for pred in &[
        "subsidiary_of",
        "competitor_of",
        "acquired_by",
        "partner_with",
    ] {
        s.push_with_allowed_labels(pred, &["organization"], &["organization"]);
    }

    // org → location
    for pred in &["located_in", "headquartered_in"] {
        s.push_with_allowed_labels(pred, &["organization"], &["location"]);
    }

    // meeting → date
    s.push_with_allowed_labels("scheduled_for", &["meeting"], &["date"]);
    // meeting → topic
    s.push_with_allowed_labels("about", &["meeting"], &["topic"]);
    // task → date
    s.push_with_allowed_labels("due_on", &["task"], &["date"]);
    // task → task
    for pred in &["blocks", "depends_on"] {
        s.push_with_allowed_labels(pred, &["task"], &["task"]);
    }

    s
}

/// A single entity span returned by the NER model.
#[derive(Debug, Clone, Serialize)]
pub struct EntitySpan {
    pub sequence_idx: usize,
    pub text: String,
    pub class: String,
    pub probability: f32,
}

/// A single relation candidate returned by the RE model.
#[derive(Debug, Clone, Serialize)]
pub struct RelationSpan {
    pub sequence_idx: usize,
    /// Predicate label (e.g. "works_at").
    pub predicate: String,
    /// Subject entity text as it appears in the source.
    pub subject: String,
    /// Object entity text as it appears in the source.
    pub object: String,
    pub probability: f32,
}

/// Abstraction so the pipeline can be tested without ONNX models present, and
/// so the implementation can be swapped (e.g. LLM-based extraction) if needed.
pub trait EntityExtractor: Send + Sync {
    fn extract(&self, sentences: &[&str], labels: &[&str]) -> AppResult<Vec<Vec<EntitySpan>>>;
}

// --- Structured extraction (Phase 4): the Write pipeline's input contract ---

/// A fully-resolved extraction — the structured output `chat_write` consumes.
/// Unlike the raw `(EntitySpan, RelationSpan)` pair this carries note-taker
/// ("self") resolution and explicit temporal bounds, so an LLM extractor can
/// express things GLiNER's zero-shot NER+RE cannot (see ADR-0006).
#[derive(Debug, Clone, Default, Serialize, serde::Deserialize)]
pub struct Extraction {
    pub entities: Vec<ExtractedEntity>,
    pub relations: Vec<ExtractedRelation>,
}

/// One entity an extractor surfaced, already resolved to a canonical name.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ExtractedEntity {
    /// Canonical display name. For the note-taker this is always `SELF_NAME`.
    pub name: String,
    /// One of `ENTITY_LABELS`.
    pub entity_type: String,
    /// True when this entity is the note-taker — i.e. the message referred to
    /// them as "I" / "we" / "me" / "my". Facts about them route to one note.
    #[serde(default)]
    pub is_self: bool,
    pub confidence: f32,
}

/// One relation between two extracted entities, with optional validity bounds.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ExtractedRelation {
    /// Subject entity — matches an `ExtractedEntity.name`.
    pub subject: String,
    pub predicate: String,
    /// Object entity — matches an `ExtractedEntity.name`.
    pub object: String,
    /// Explicit start of validity, if the message stated one.
    #[serde(default)]
    pub valid_from: Option<chrono::DateTime<chrono::Utc>>,
    /// Explicit end of validity. `Some(_)` marks the fact historical (a closed
    /// interval) — e.g. a former employer — so it renders past-tense and is
    /// not returned by `current_facts`.
    #[serde(default)]
    pub valid_to: Option<chrono::DateTime<chrono::Utc>>,
    pub confidence: f32,
}

/// Canonical name for the note-taker entity. Facts whose subject `is_self`
/// route to `People/Me.md` regardless of the pronoun the message used.
pub const SELF_NAME: &str = "Me";

/// Produces a structured `Extraction` from a raw chat message. Keeps the Write
/// pipeline decoupled from any single extractor — GLiNER, an LLM, or a test
/// fake all sit behind this trait (ADR-0003's planned extension point,
/// realised in ADR-0006). Async because an LLM extractor is an HTTP call.
#[async_trait::async_trait]
pub trait FactExtractor: Send + Sync {
    async fn extract_facts(&self, message: &str) -> AppResult<Extraction>;
}

/// A `FactExtractor` that replays a fixed `Extraction` regardless of input.
/// Lets the Write pipeline be tested end-to-end deterministically, with no
/// ONNX or LLM dependency.
#[cfg(test)]
pub struct ScriptedExtractor {
    pub extraction: Extraction,
}

#[cfg(test)]
#[async_trait::async_trait]
impl FactExtractor for ScriptedExtractor {
    async fn extract_facts(&self, _message: &str) -> AppResult<Extraction> {
        Ok(self.extraction.clone())
    }
}

/// Extract a 4-digit year (1000–9999) from free text. The first such run wins —
/// enough for "1975", "joined in 2021", "since 2024". Returns midnight UTC on
/// Jan 1 of that year (the year-granularity `valid_from` heuristic).
pub fn parse_year_start(text: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::TimeZone;
    let chars: Vec<char> = text.chars().collect();
    for window in chars.windows(4) {
        if window.iter().all(char::is_ascii_digit) {
            if let Ok(year) = window.iter().collect::<String>().parse::<i32>() {
                if (1000..=9999).contains(&year) {
                    return chrono::Utc.with_ymd_and_hms(year, 1, 1, 0, 0, 0).single();
                }
            }
        }
    }
    None
}

/// Default model layout under `<formation>/.chat-notes/models/`. The multitask
/// model handles both NER and RE; using one model keeps disk footprint sane.
pub struct ModelPaths {
    pub root: PathBuf,
}

impl ModelPaths {
    pub fn under_app_dir(app_dir: &Path) -> Self {
        Self {
            root: app_dir.join("models").join("gliner-multitask-large-v0.5"),
        }
    }

    pub fn tokenizer(&self) -> PathBuf {
        self.root.join("tokenizer.json")
    }

    pub fn onnx(&self) -> PathBuf {
        self.root.join("onnx").join("model.onnx")
    }

    pub fn exist(&self) -> bool {
        self.tokenizer().is_file() && self.onnx().is_file()
    }
}

/// Bootstrap message printed when model files are missing. Mirrors the official
/// HuggingFace repo layout for `gliner-multitask-large-v0.5`.
pub fn model_bootstrap_hint(paths: &ModelPaths) -> String {
    format!(
        "GLiNER model files not found at {}.\n\
         Bootstrap once:\n  \
         mkdir -p {root}/onnx && cd {root} && \\\n  \
         curl -L -o tokenizer.json https://huggingface.co/onnx-community/gliner-multitask-large-v0.5/resolve/main/tokenizer.json && \\\n  \
         curl -L -o onnx/model.onnx https://huggingface.co/onnx-community/gliner-multitask-large-v0.5/resolve/main/onnx/model.onnx",
        paths.root.display(),
        root = paths.root.display()
    )
}

/// Lazy wrapper around the gline-rs ONNX `Model`. Holds it behind a mutex
/// because pipelines borrow `&Model` and the Composable wrapper is not Sync
/// across all internals.
pub struct GlinerExtractor {
    paths: ModelPaths,
    params: Parameters,
    model: OnceCell<Mutex<Model>>,
}

impl GlinerExtractor {
    pub fn new(paths: ModelPaths) -> Self {
        Self {
            paths,
            params: Parameters::default(),
            model: OnceCell::new(),
        }
    }

    fn load_model(&self) -> AppResult<&Mutex<Model>> {
        if let Some(m) = self.model.get() {
            return Ok(m);
        }
        if !self.paths.exist() {
            return Err(AppError::other(model_bootstrap_hint(&self.paths)));
        }
        let onnx = self.paths.onnx();
        let model = Model::new(&onnx, RuntimeParameters::default())
            .map_err(|e| AppError::other(format!("load ONNX model: {e}")))?;
        let _ = self.model.set(Mutex::new(model));
        Ok(self
            .model
            .get()
            .expect("OnceCell just set or already populated"))
    }

    fn tokenizer_path_str(&self) -> AppResult<String> {
        self.paths
            .tokenizer()
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| AppError::other("tokenizer path not utf-8"))
    }
}

impl EntityExtractor for GlinerExtractor {
    fn extract(&self, sentences: &[&str], labels: &[&str]) -> AppResult<Vec<Vec<EntitySpan>>> {
        let model_lock = self.load_model()?;
        let model = model_lock
            .lock()
            .map_err(|_| AppError::other("model mutex poisoned"))?;
        let tokenizer = self.tokenizer_path_str()?;

        let pipeline = TokenPipeline::new(&tokenizer)
            .map_err(|e| AppError::other(format!("build TokenPipeline: {e}")))?;
        let input = TextInput::from_str(sentences, labels)
            .map_err(|e| AppError::other(format!("build TextInput: {e}")))?;
        let span_output = model
            .inference(input, &pipeline, &self.params)
            .map_err(|e| AppError::other(format!("NER inference: {e}")))?;

        Ok(spans_to_entity_rows(span_output))
    }
}

/// Combined NER + RE in one call. The same loaded model is used for both
/// stages — NER yields entity spans, RE consumes those spans and yields
/// relation candidates filtered by the supplied schema.
pub fn extract_entities_and_relations(
    extractor: &GlinerExtractor,
    sentence: &str,
    entity_labels: &[&str],
    schema: &RelationSchema,
) -> AppResult<(Vec<EntitySpan>, Vec<RelationSpan>)> {
    let model_lock = extractor.load_model()?;
    let model = model_lock
        .lock()
        .map_err(|_| AppError::other("model mutex poisoned"))?;
    let tokenizer = extractor.tokenizer_path_str()?;

    // Step 1: NER.
    let token_pipeline = TokenPipeline::new(&tokenizer)
        .map_err(|e| AppError::other(format!("build TokenPipeline: {e}")))?;
    let input = TextInput::from_str(&[sentence], entity_labels)
        .map_err(|e| AppError::other(format!("build TextInput: {e}")))?;
    let span_output = model
        .inference(input, &token_pipeline, &extractor.params)
        .map_err(|e| AppError::other(format!("NER inference: {e}")))?;

    // Project entities out of the SpanOutput before we hand it off to RE
    // (the RelationPipeline consumes the SpanOutput by value).
    let entities = entity_spans_from(&span_output);

    // Step 2: RE. The RelationPipeline takes the SpanOutput from NER as input
    // and emits RelationOutput. We keep the same loaded Model for inference.
    let relation_pipeline = RelationPipeline::default(&tokenizer, schema)
        .map_err(|e| AppError::other(format!("build RelationPipeline: {e}")))?;
    let relation_output = model
        .inference(span_output, &relation_pipeline, &extractor.params)
        .map_err(|e| AppError::other(format!("RE inference: {e}")))?;

    let mut relations = Vec::new();
    for (seq_idx, row) in relation_output.relations.into_iter().enumerate() {
        for rel in row {
            relations.push(RelationSpan {
                sequence_idx: seq_idx,
                predicate: rel.class().to_string(),
                subject: rel.subject().to_string(),
                object: rel.object().to_string(),
                probability: rel.probability(),
            });
        }
    }

    Ok((entities, relations))
}

#[async_trait::async_trait]
impl FactExtractor for GlinerExtractor {
    /// GLiNER fallback. Runs NER+RE over the message and maps the result into
    /// an `Extraction`. GLiNER cannot resolve self-reference or tense, so
    /// `is_self` is always false and relations carry no `valid_to`; the single
    /// unambiguous-date heuristic from the original `chat_write` path is kept
    /// as the only `valid_from` source.
    async fn extract_facts(&self, message: &str) -> AppResult<Extraction> {
        let schema = default_relation_schema();
        let (entities, relations) =
            extract_entities_and_relations(self, message, ENTITY_LABELS, &schema)?;

        let date_texts: Vec<&str> = entities
            .iter()
            .filter(|e| e.class == "date")
            .map(|e| e.text.as_str())
            .collect();
        let single_year = match date_texts.as_slice() {
            [one] => parse_year_start(one),
            _ => None,
        };

        Ok(Extraction {
            entities: entities
                .into_iter()
                .map(|e| ExtractedEntity {
                    name: e.text,
                    entity_type: e.class,
                    is_self: false,
                    confidence: e.probability,
                })
                .collect(),
            relations: relations
                .into_iter()
                .map(|r| ExtractedRelation {
                    subject: r.subject,
                    predicate: r.predicate,
                    object: r.object,
                    valid_from: single_year,
                    valid_to: None,
                    confidence: r.probability,
                })
                .collect(),
        })
    }
}

fn spans_to_entity_rows(output: SpanOutput) -> Vec<Vec<EntitySpan>> {
    let mut out = Vec::with_capacity(output.spans.len());
    for spans in output.spans {
        let mut row = Vec::with_capacity(spans.len());
        for span in spans {
            row.push(EntitySpan {
                sequence_idx: span.sequence(),
                text: span.text().to_string(),
                class: span.class().to_string(),
                probability: span.probability(),
            });
        }
        out.push(row);
    }
    out
}

/// Borrowed projection of SpanOutput → flat EntitySpan list. Used when the
/// caller still needs to consume the SpanOutput for downstream RE inference.
fn entity_spans_from(output: &SpanOutput) -> Vec<EntitySpan> {
    let mut out = Vec::new();
    for spans in &output.spans {
        for span in spans {
            out.push(EntitySpan {
                sequence_idx: span.sequence(),
                text: span.text().to_string(),
                class: span.class().to_string(),
                probability: span.probability(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test that runs only when GLiNER model files are present on disk.
    /// Ignored by default so CI doesn't require a several-hundred-MB download.
    /// Run locally with: `cargo test -- --ignored extraction::tests::ner_round_trip`
    #[test]
    #[ignore]
    fn ner_round_trip() {
        let paths = ModelPaths {
            root: std::env::var("SEDIMENT_GLINER_MODEL_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("models/gliner-multitask-large-v0.5")),
        };
        if !paths.exist() {
            eprintln!("{}", model_bootstrap_hint(&paths));
            panic!("model files not found; skip with --ignored");
        }
        let extractor = GlinerExtractor::new(paths);
        let spans = extractor
            .extract(
                &["Bill Gates is an American businessman who co-founded Microsoft."],
                &["person", "company"],
            )
            .expect("extract");
        let flat: Vec<&EntitySpan> = spans.iter().flatten().collect();
        assert!(
            flat.iter()
                .any(|s| s.text == "Bill Gates" && s.class == "person"),
            "expected to recover Bill Gates as person, got: {flat:?}"
        );
        assert!(
            flat.iter()
                .any(|s| s.text == "Microsoft" && s.class == "company"),
            "expected to recover Microsoft as company, got: {flat:?}"
        );
    }

    /// Smoke test for the combined NER+RE pipeline. Requires model files;
    /// `#[ignore]` by default for the same reason as ner_round_trip.
    #[test]
    #[ignore]
    fn relation_extraction_round_trip() {
        let paths = ModelPaths {
            root: std::env::var("SEDIMENT_GLINER_MODEL_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("models/gliner-multitask-large-v0.5")),
        };
        if !paths.exist() {
            panic!("model files not found; skip with --ignored");
        }
        let extractor = GlinerExtractor::new(paths);
        let schema = default_relation_schema();
        let (entities, relations) = extract_entities_and_relations(
            &extractor,
            "Bill Gates co-founded Microsoft in 1975.",
            ENTITY_LABELS,
            &schema,
        )
        .expect("extract facts");
        assert!(!entities.is_empty(), "expected at least one entity");
        // Relations may or may not fire depending on the multitask model's
        // training — log but don't assert on count, since recall is the open
        // question we're testing in real-world use.
        eprintln!("entities: {entities:?}");
        eprintln!("relations: {relations:?}");
    }

    #[test]
    fn default_relation_schema_has_expected_predicates() {
        let s = default_relation_schema();
        let rels = s.relations();
        assert!(rels.contains_key("works_at"));
        assert!(rels.contains_key("founded"));
        assert!(rels.contains_key("located_in"));
        assert!(rels.contains_key("attended"));
        assert!(rels.contains_key("depends_on"));
        // Spot-check a constraint: works_at must allow person→organization.
        let works_at = &rels["works_at"];
        assert!(works_at.allows_subject("person"));
        assert!(works_at.allows_object("organization"));
        assert!(!works_at.allows_subject("organization"));
        // The opinion/advocacy predicate added in Phase 4.
        assert!(rels.contains_key("advocates_for"));
    }

    #[test]
    fn parse_year_start_resolves_a_four_digit_year() {
        let dt = parse_year_start("joined in 2021").expect("year parsed");
        assert_eq!(dt.to_rfc3339(), "2021-01-01T00:00:00+00:00");
        assert!(parse_year_start("last March").is_none());
        assert!(parse_year_start("42").is_none(), "two digits is not a year");
    }
}
