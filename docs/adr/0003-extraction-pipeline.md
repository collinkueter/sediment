# ADR-0003: Entity and relation extraction pipeline

**Status:** Superseded by [ADR-0009](0009-conversational-agent.md) (2026-05-22) — the deterministic NER/RE extraction pipeline was replaced by the conversational agent, which decides what to record via its own understanding and graph tools. Kept for historical context.
**Relates to:** ADR-0001 (the gline-rs choice), Phase 2 milestones P2-M1, P2-M2, P2-M5, P2-M6

## Context

Sediment's Write flow turns a chat message into structured facts in SurrealDB. The pipeline needs to: detect entities (people, orgs, projects...), detect relations between them, resolve each entity to a stable record, and write the relations as bi-temporal edges with provenance.

ADR-0001 settled the *tool* — `gline-rs` running a multitask GLiNER ONNX model. This ADR records how the pipeline is *assembled* around it.

## Decision

### Stages

A Write-mode message flows through five stages, all in `commands/chat.rs::chat_write` → `commands/extraction.rs::run_extract_facts`:

1. **Persist** — the user message is written to the `chat_message` table; its record id becomes the `source_chat_id` provenance pointer.
2. **NER + RE** — `core/extraction.rs::extract_entities_and_relations` runs `TokenPipeline` (NER) then `RelationPipeline` (RE) over one shared `orp::Model`. NER yields `EntitySpan`s; RE consumes the NER `SpanOutput` and yields `RelationSpan`s filtered by `default_relation_schema()`.
3. **Entity upsert** — each entity span above the confidence floor is run through `MemoryStore::upsert_entity`, which resolves it to an existing record (by canonical name or alias) or creates a new slug-id record. A `name → entity_id` map is built for the next stage.
4. **Fact write** — each relation span is resolved (subject + object texts must both appear in the name map) and written via `MemoryStore::relate_fact` (see ADR-0004 for the bi-temporal semantics).
5. **Summarise** — counts of entities, facts, and skipped items are returned for the chat pane to render.

### Confidence floors

- Entities: `0.5` (`MIN_ENTITY_CONFIDENCE`)
- Relations: `0.6` (`MIN_RELATION_CONFIDENCE`)

RE is empirically noisier than NER, hence the higher bar. Spans below the floor are counted as `skipped_low_confidence` and surfaced to the user rather than silently dropped.

### Entity type labels and predicate vocabulary

- Entity labels (`ENTITY_LABELS`) are a fixed 9-item set that matches the `entity.entity_type` ASSERT in the SurrealDB schema, so the extractor and schema can never disagree.
- The relation schema (`default_relation_schema()`) is ~30 predicates organised by `(subject_type, object_type)` per the docs/plans/phase-2.md decision. The schema's allowed-label constraints prune nonsensical triples (e.g. an `organization` cannot be the subject of `works_at`).

### The `EntityExtractor` trait

`core/extraction.rs` keeps an `EntityExtractor` trait so the call sites are not hard-bound to `GlinerExtractor`. If real-world recall on user-style notes proves poor, an LLM-grammar-constrained extractor can drop in behind the same trait without touching the storage layer.

### Graceful degradation

Both `chat_write` and `chat_ask` work without the GLiNER model files. `chat_write` returns the bootstrap hint as an error (the chat pane shows it). `chat_ask` simply skips graph-fact retrieval and answers from vector search alone.

## Consequences

- **Positive** — one shared `Model` for NER + RE keeps memory use flat. The trait boundary keeps the LLM fallback path cheap to add. Deterministic ONNX extraction means no JSON-shaped-output flakiness from a small local LLM.
- **Negative** — RE recall depends entirely on the zero-shot multitask model; an unfamiliar predicate phrasing may simply not fire. Mitigation: the predicate normalization map (folding "co-founded" → "founded" etc.) and the option of fine-tuning later.
- **Negative** — entity resolution is currently exact-match on canonical name or alias. "John" vs "John Smith" only unify if one is recorded as the other's alias. Fuzzy / embedding-based entity resolution is a Phase 3+ refinement.
