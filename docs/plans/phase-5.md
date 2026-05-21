# Sediment — Phase 5 Plan: Polish & Extraction Robustness

**Status:** In progress (2026-05-20)
**Predecessor:** Phase 4 complete at commit `c67ffd7` — see [ADR-0006](../adr/0006-llm-backed-extraction.md)

---

## Context for a fresh session

Phases 1–4 are committed. The desktop shell, embedded SurrealDB, background
indexing, the Write → stage → review → commit pipeline, Ask-mode retrieval,
conflict detection, and LLM-backed extraction (ADR-0006) all work and are
covered by 55 unit tests.

Phase 5 is the spec's final stage — "Polish" (tech-spec §15): hardening, edge
cases, and test coverage rather than new subsystems. This plan records a gap
analysis of the Phase 4 code and the polish it drives.

---

## Gap analysis (2026-05-20)

A read-through of the committed Phase 4 pipeline surfaced these gaps. They are
robustness issues, not design flaws — the architecture holds.

### G1 — The LLM may not return a bare JSON object

`LlmExtractor::extract_via_llm` does `serde_json::from_str(response.trim())`.
Ollama's JSON mode usually yields a bare object, but small local models still
sometimes wrap it in a ```` ```json ```` fence or prefix a sentence of
preamble ("Here is the extraction:"). Either makes `from_str` fail, dropping a
good extraction to the GLiNER fallback for no real reason.

**Fix:** an `extract_json_object` step — strip code fences, then slice from the
first `{` to the last `}` — before deserializing.

### G2 — Relation endpoints must string-match an entity name exactly

`run_chat_write` resolves a relation by `entity_by_name.get(&rel.subject)`, an
exact-case `HashMap` lookup. A 3B model routinely emits the entity as `"Josh"`
but the relation subject as `"josh"` / `" Josh"`, and the prompt's "Always name
that entity Me" is not reliably obeyed. Every such drift counts as
`skipped_unresolved` and silently loses a real fact.

**Fix:** resolve relation endpoints against entity names case-insensitively and
whitespace-trimmed. This is *intra-extraction* matching only — cross-message
entity resolution (R4, fuzzy `lookup_entity`) stays out of scope.

### G3 — A punctuation-only predicate slugifies to the empty string

`slugify("->")` is `""`. `into_extraction` filters empty *raw* predicates but
not ones that vanish after slugifying, yielding a `fact_id` of `":object"` and
a blank `- ` bullet.

**Fix:** drop a relation whose slugified predicate is empty.

### G4 — Thin end-to-end coverage of non-create routing

The `e2e_extraction` fixtures all extract person facts that *create* a fresh
note. No fixture exercises: an organization as the fact subject (the
`Organizations/` folder), updating a note that already holds user prose, or
re-sending an identical message (whole-batch idempotence).

**Fix:** add fixtures for each.

## Deferred — explicitly out of scope for Phase 5

- **BYOK cloud fallback** (tech-spec §15). A genuinely new feature — a cloud
  LLM client, API-key storage, and settings UI — not polish. It cannot be
  verified without live keys, and a half-wired version would violate the
  "no half-finished implementations" rule. `Tier::Byok` already degrades
  cleanly to the local model; a full BYOK path is its own ADR + phase.
- **Fuzzy / embedding entity resolution** (Phase 3 R4). `lookup_entity` stays
  exact-match; a "Did you mean [[John Smith]]?" disambiguation step is a
  sizeable feature, not a hardening pass.
- **Note-name `_2` collision suffix** (Phase 3 open question). Moot under
  exact-match resolution — same type + same name resolves to one entity, and
  different types route to different folders. Revisit alongside R4.

---

## Milestones

### P5-M1 — Extraction robustness

- `core/llm_extractor.rs`: add `extract_json_object`; apply it in
  `extract_via_llm`. Drop relations with an empty slugified predicate (G3).
- `commands/chat.rs::run_chat_write`: case-insensitive, trimmed relation
  endpoint resolution (G2).
- Unit tests: fenced JSON, preamble JSON, plain JSON, and an empty-predicate
  relation through `into_extraction`.
- **Verify:** `cargo test`.

### P5-M2 — Real-world end-to-end test scenarios

New `e2e_extraction` cases:
- organization as fact subject → `Organizations/<name>.md`;
- a commit, an out-of-band user edit adding prose, then a second fact —
  prose survives;
- re-sending an identical message stages nothing;
- a relation whose endpoint case differs from the entity name still resolves
  (covers G2).
- **Verify:** `cargo test`, `cargo test -- --ignored` with a model present.

### P5-M3 — Phase 5 verification

Full gates green (`cargo fmt`/`clippy`/`test`, `tsc`, `biome`); README phase
status updated; commit.

---

## Verification (Phase 5 acceptance)

1. A fenced or preamble-wrapped LLM response is parsed, not dropped to GLiNER.
2. A relation whose endpoint case/spacing drifts from the entity still files.
3. Organization-subject facts land under `Organizations/`.
4. An update never disturbs user prose; a re-sent message stages nothing.
5. All gates green; commit.
