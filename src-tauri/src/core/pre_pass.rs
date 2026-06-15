//! The deterministic pre-pass — ADR-0011 §2.
//!
//! Before the agent runs, the orchestrator grounds the turn *itself* instead of
//! hoping the agent chooses to search. It resolves the entities named in the
//! message to their existing notes (so the agent reuses them and never creates a
//! duplicate `Josh.md`), surfaces those entities' current Facts (so a
//! contradiction is visible), and pulls the most related note excerpts. The
//! result is rendered to a Markdown block and pushed into the prompt as
//! `TurnRequest::injected_context`.
//!
//! **Best-effort, never fatal.** Every step swallows its own errors (a logged
//! warning) and degrades to less grounding — the embedder being offline yields
//! no related-notes section, not a failed turn. The agent's own tools remain the
//! backstop; this only guarantees a reliable floor.
//!
//! **Honest scope.** Entity resolution, current Facts, and related notes are
//! fully deterministic. *Full* contradiction detection still needs the
//! relationship parsed from the message — that stays the agent's job (GLiNER is
//! retired); the pre-pass only hands it the current Facts to judge against.

use crate::core::memory::{record_id_to_string, FactRow, MemoryStore};
use crate::core::ollama_sidecar::{OllamaSidecar, DEFAULT_EMBED_MODEL};
use std::collections::HashSet;

/// How many related note excerpts to pull.
const RELATED_K: usize = 5;
/// Cap on resolved entities, so a name-dense message can't fan out unboundedly.
const MAX_ENTITIES: usize = 10;
/// Max characters of a related-note excerpt.
const EXCERPT_CHARS: usize = 200;

/// One entity the message names that already exists in the formation.
#[derive(Debug, Clone)]
pub struct ResolvedEntity {
    pub name: String,
    pub entity_type: String,
    pub note_path: Option<String>,
    /// Current relationship Facts, pre-rendered as `predicate object` lines.
    pub facts: Vec<String>,
}

/// One note the message is semantically near.
#[derive(Debug, Clone)]
pub struct RelatedNote {
    pub note_path: String,
    pub excerpt: String,
}

/// The grounding the pre-pass pushes into a turn.
#[derive(Debug, Clone, Default)]
pub struct PrePassContext {
    pub resolved_entities: Vec<ResolvedEntity>,
    pub related_notes: Vec<RelatedNote>,
}

impl PrePassContext {
    /// The resolved-entities section (identity + current Facts) — the highest-
    /// priority grounding: it's what stops duplicate notes and surfaces
    /// contradictions. `None` when nothing resolved.
    pub fn render_entities_markdown(&self) -> Option<String> {
        if self.resolved_entities.is_empty() {
            return None;
        }
        let mut s = String::new();
        s.push_str(
            "Retrieved from your notes before you replied. Reuse these existing notes and \
             entities — do not create duplicates.\n\n",
        );
        s.push_str("## Entities already in your notes\n");
        for e in &self.resolved_entities {
            match &e.note_path {
                Some(p) => s.push_str(&format!("- {} ({}) — {}\n", e.name, e.entity_type, p)),
                None => s.push_str(&format!("- {} ({})\n", e.name, e.entity_type)),
            }
            for f in &e.facts {
                s.push_str(&format!("  - {f}\n"));
            }
        }
        Some(s.trim_end().to_string())
    }

    /// The related-notes section — useful, but the first thing dropped under
    /// budget pressure. `None` when nothing related was found.
    pub fn render_related_markdown(&self) -> Option<String> {
        if self.related_notes.is_empty() {
            return None;
        }
        let mut s = String::new();
        s.push_str("## Related notes\n");
        for n in &self.related_notes {
            s.push_str(&format!("- {}: {}\n", n.note_path, n.excerpt));
        }
        Some(s.trim_end().to_string())
    }
}

/// Ground a turn: resolve named entities + their current Facts, and pull related
/// notes. Best-effort — see the module doc.
pub async fn build_pre_pass(
    store: &MemoryStore,
    sidecar: &OllamaSidecar,
    message: &str,
) -> PrePassContext {
    PrePassContext {
        resolved_entities: resolve_entities(store, message).await,
        related_notes: related_notes(store, sidecar, message).await,
    }
}

/// Resolve the proper-noun candidates in `message` against the graph, attaching
/// each resolved entity's current Facts.
async fn resolve_entities(store: &MemoryStore, message: &str) -> Vec<ResolvedEntity> {
    let mut out = Vec::new();
    let mut seen_ids = HashSet::new();
    // Words already covered by a resolved multi-word name, so a resolved
    // "Sarah Chen" doesn't also fire a stray single-token "Sarah" lookup that
    // could pull in a different Sarah.
    let mut consumed: HashSet<String> = HashSet::new();
    for name in candidate_names(message) {
        if out.len() >= MAX_ENTITIES {
            break;
        }
        let is_multi = name.contains(' ');
        if !is_multi && consumed.contains(&name.to_lowercase()) {
            continue;
        }
        match store.lookup_entity(&name).await {
            Ok(Some(ent)) => {
                if !seen_ids.insert(ent.id.clone()) {
                    continue;
                }
                if is_multi {
                    for w in name.split_whitespace() {
                        consumed.insert(w.to_lowercase());
                    }
                }
                let facts = match store.current_facts(&ent.id).await {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!("pre_pass: current_facts({}) failed: {e}", ent.id);
                        Vec::new()
                    }
                };
                out.push(ResolvedEntity {
                    name: ent.canonical_name,
                    entity_type: ent.entity_type,
                    note_path: ent.note_path,
                    facts: facts.iter().map(render_fact).collect(),
                });
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("pre_pass: lookup_entity({name}) failed: {e}"),
        }
    }
    out
}

/// Embed the message and pull the nearest note chunks, de-duplicated by note.
async fn related_notes(
    store: &MemoryStore,
    sidecar: &OllamaSidecar,
    message: &str,
) -> Vec<RelatedNote> {
    if message.trim().is_empty() {
        return Vec::new();
    }
    let embedding = match sidecar.embed(DEFAULT_EMBED_MODEL, message).await {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("pre_pass: embed failed, skipping related notes: {e}");
            return Vec::new();
        }
    };
    let hits = match store.search_chunks(embedding, RELATED_K).await {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("pre_pass: search_chunks failed: {e}");
            return Vec::new();
        }
    };
    let mut seen = HashSet::new();
    hits.into_iter()
        .filter(|h| seen.insert(h.note_path.clone()))
        .map(|h| RelatedNote {
            note_path: h.note_path,
            excerpt: truncate(&h.text, EXCERPT_CHARS),
        })
        .collect()
}

/// Render a current/historical Fact as a short `predicate object` line.
fn render_fact(f: &FactRow) -> String {
    let obj = record_id_to_string(&f.object);
    let obj = obj.strip_prefix("entity:").unwrap_or(&obj).replace('_', " ");
    let mut s = format!("{} {}", f.predicate, obj);
    if let Some(to) = f.valid_to {
        s.push_str(&format!(" (until {})", to.format("%Y-%m-%d")));
    }
    s
}

/// Candidate entity names: capitalized alphabetic tokens, plus adjacent
/// capitalized pairs (for two-word names), minus a small set of sentence-initial
/// / pronoun words. A deterministic floor — crude, but it never invents and the
/// agent resolves the rest itself.
fn candidate_names(message: &str) -> Vec<String> {
    const SKIP: &[&str] = &[
        "The", "A", "An", "I", "He", "She", "It", "They", "We", "You", "My", "His", "Her", "Their",
        "Our", "Your", "This", "That", "These", "Those", "When", "What", "Who", "Where", "Why",
        "How", "Is", "Are", "Was", "Were", "Do", "Does", "Did", "If", "And", "But", "Or", "So",
        "Then", "Now", "Today", "Yesterday", "Tomorrow", "Maybe", "Also", "Just",
    ];
    let words: Vec<&str> = message
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
        .collect();
    let is_name = |w: &str| -> bool {
        w.chars().next().map(char::is_uppercase).unwrap_or(false)
            && w.chars().count() >= 2
            && w.chars().all(|c| c.is_alphabetic() || c == '\'')
            && !SKIP.contains(&w)
    };

    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for i in 0..words.len() {
        if !is_name(words[i]) {
            continue;
        }
        if i + 1 < words.len() && is_name(words[i + 1]) {
            let bigram = format!("{} {}", words[i], words[i + 1]);
            if seen.insert(bigram.clone()) {
                out.push(bigram);
            }
        }
        let w = words[i].to_string();
        if seen.insert(w.clone()) {
            out.push(w);
        }
    }
    out
}

/// Collapse whitespace and clip to `max` characters with an ellipsis.
fn truncate(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        s
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", cut.trim_end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::memory::{FactWriteInput, MemoryStore};
    use chrono::Utc;
    use std::path::PathBuf;

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir()
            .join("sediment-test-pre-pass")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&p).expect("tempdir");
        p
    }

    #[test]
    fn candidate_names_picks_proper_nouns_and_skips_pronouns() {
        let c = candidate_names("Did Josh move to Cloudflare? He told Sarah Chen.");
        assert!(c.contains(&"Josh".to_string()));
        assert!(c.contains(&"Cloudflare".to_string()));
        assert!(c.contains(&"Sarah Chen".to_string()), "adjacent pair captured");
        assert!(!c.iter().any(|w| w == "Did" || w == "He"), "pronouns/openers skipped");
    }

    #[test]
    fn empty_pre_pass_renders_no_sections() {
        let empty = PrePassContext::default();
        assert!(empty.render_entities_markdown().is_none());
        assert!(empty.render_related_markdown().is_none());
    }

    /// The high-value path: a name in the message resolves to its existing entity
    /// and its current Facts are surfaced in the grounding block.
    #[tokio::test]
    async fn pre_pass_resolves_a_named_entity_with_its_facts() {
        let root = tempdir();
        let store = MemoryStore::open(&root.join(".chat-notes").join("memory"))
            .await
            .expect("open store");
        let josh = store.upsert_entity("Josh", "person", vec![]).await.unwrap();
        let cf = store
            .upsert_entity("Cloudflare", "organization", vec![])
            .await
            .unwrap();
        store
            .relate_fact(FactWriteInput {
                subject_id: josh.id.clone(),
                predicate: "works_at".to_string(),
                object_id: cf.id.clone(),
                valid_from: Utc::now(),
                valid_to: None,
                source_chat_id: "chat_message:seed".to_string(),
                confidence: 1.0,
            })
            .await
            .unwrap();

        // Embedding is offline in unit tests; related-notes degrades to empty,
        // and entity resolution still works.
        let ctx = build_pre_pass(&store, &OllamaSidecar::default(), "Did Josh change jobs?").await;

        let josh_e = ctx
            .resolved_entities
            .iter()
            .find(|e| e.name == "Josh")
            .expect("Josh resolved from the message");
        assert!(
            josh_e.facts.iter().any(|f| f.contains("works_at")),
            "current fact surfaced for the agent to judge against"
        );

        let md = ctx.render_entities_markdown().expect("entities grounding rendered");
        assert!(md.contains("Josh"));
        assert!(md.contains("do not create duplicates"));
    }
}
