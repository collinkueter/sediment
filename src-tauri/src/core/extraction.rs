//! Entity and relation extraction via gline-rs (GLiNER + multitask models).
//!
//! Scope (this turn): lazy-loaded NER from a multitask GLiNER ONNX model. The
//! `EntityExtractor` trait keeps the wider pipeline (M-pivot follow-ups) loosely
//! coupled to the concrete model — swap or stub it in tests.
//!
//! Model files are NOT bundled. The user runs the documented bootstrap once
//! (see `model_bootstrap_hint`) and the extractor lazily loads from
//! `<formation>/.chat-notes/models/`.

use crate::error::{AppError, AppResult};
use gliner::model::input::text::TextInput;
use gliner::model::params::Parameters;
use gliner::model::pipeline::token::TokenMode;
use gliner::model::GLiNER;
use orp::params::RuntimeParameters;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tokio::sync::OnceCell;

/// A single entity span returned by the NER model.
#[derive(Debug, Clone, Serialize)]
pub struct EntitySpan {
    pub sequence_idx: usize,
    pub text: String,
    pub class: String,
    pub probability: f32,
}

/// Abstraction so the pipeline can be tested without ONNX models present, and
/// so the implementation can be swapped (e.g. LLM-based extraction) if needed.
pub trait EntityExtractor: Send + Sync {
    fn extract(&self, sentences: &[&str], labels: &[&str]) -> AppResult<Vec<Vec<EntitySpan>>>;
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
         curl -L -o tokenizer.json https://huggingface.co/knowledgator/gliner-multitask-large-v0.5/resolve/main/tokenizer.json && \\\n  \
         curl -L -o onnx/model.onnx https://huggingface.co/knowledgator/gliner-multitask-large-v0.5/resolve/main/onnx/model.onnx",
        paths.root.display(),
        root = paths.root.display()
    )
}

/// Lazy GLiNER wrapper. Holds the loaded model behind a mutex (the underlying
/// type isn't `Sync`-friendly across all gline-rs internals — taking a lock
/// per inference is cheap relative to the inference itself).
pub struct GlinerExtractor {
    paths: ModelPaths,
    model: OnceCell<Mutex<GLiNER<TokenMode>>>,
}

impl GlinerExtractor {
    pub fn new(paths: ModelPaths) -> Self {
        Self {
            paths,
            model: OnceCell::new(),
        }
    }

    fn load(&self) -> AppResult<&Mutex<GLiNER<TokenMode>>> {
        if let Some(m) = self.model.get() {
            return Ok(m);
        }
        if !self.paths.exist() {
            return Err(AppError::other(model_bootstrap_hint(&self.paths)));
        }
        let tokenizer = self.paths.tokenizer();
        let onnx = self.paths.onnx();
        let model = GLiNER::<TokenMode>::new(
            Parameters::default(),
            RuntimeParameters::default(),
            tokenizer
                .to_str()
                .ok_or_else(|| AppError::other("tokenizer path not utf-8"))?,
            onnx.to_str()
                .ok_or_else(|| AppError::other("onnx path not utf-8"))?,
        )
        .map_err(|e| AppError::other(format!("load GLiNER: {e}")))?;
        // OnceCell::set fails only if already initialised; map to a no-op.
        let _ = self.model.set(Mutex::new(model));
        Ok(self
            .model
            .get()
            .expect("OnceCell just set or already populated"))
    }
}

impl EntityExtractor for GlinerExtractor {
    fn extract(&self, sentences: &[&str], labels: &[&str]) -> AppResult<Vec<Vec<EntitySpan>>> {
        let lock = self.load()?;
        let guard = lock
            .lock()
            .map_err(|_| AppError::other("gliner mutex poisoned"))?;
        let input = TextInput::from_str(sentences, labels)
            .map_err(|e| AppError::other(format!("build TextInput: {e}")))?;
        let output = guard
            .inference(input)
            .map_err(|e| AppError::other(format!("GLiNER inference: {e}")))?;
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
        Ok(out)
    }
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
}
