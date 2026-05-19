# Sediment — Phase 2 Plan (Memory Layer)

**Status:** Draft, awaiting approval
**Author:** Claude Opus 4.7 + Collin
**Last updated:** 2026-05-19
**Spec section:** [§15 Build Plan](../../sediment-tech-spec.md) — Phase 2 (Memory Layer, 3-4 weeks)

## Context

Phase 1 landed the desktop shell, formation I/O + watcher, embedded SurrealDB with bi-temporal schema, Ollama integration (chat + embeddings), and a `gline-rs` scaffold whose `EntityExtractor` trait is wired but never called by a real pipeline. The chat pane streams from Ollama as a generic completion — no extraction, no fact writes, no staging.

Phase 2 turns Sediment into an actual fact-extraction system. The user types a sentence; entities + relations are extracted deterministically with GLiNER; facts are written to SurrealDB with bi-temporal validity windows and contradiction handling; notes that already exist in the formation get indexed in the background so RAG can use them.

**Out of scope for Phase 2** (lands in Phase 3): the staging tray with real diff content, Keep/Discard per change, atomic markdown writes from extracted facts, snapshot/undo.

## What Phase 1 left behind that Phase 2 builds on

- `core/memory.rs` — `MemoryStore` with `replace_note_chunks`, `search_chunks`, schema applied
- `core/extraction.rs` — `EntityExtractor` trait + `GlinerExtractor` (token-mode NER only)
- `core/ollama_sidecar.rs` — `embed(model, text)` already wired to `nomic-embed-text`
- `commands/memory.rs::index_note` / `search_notes` — file → chunk → embed → store works
- `commands/extraction.rs::extract_entities` — runs NER on a single text, returns spans
- Watcher emits `formation-change` on external edits — Phase 2 wires re-indexing into this

## Milestones

Each milestone is a self-contained slice; complete only when its tests pass and the verification step demonstrates the new capability.

### P2-M1 — Entity upsert + resolution (~3 days)
- New `MemoryStore::upsert_entity(canonical_name, entity_type, aliases) -> EntityId`. Matches by canonical_name OR any alias; creates if absent; merges aliases on hit.
- Map `EntitySpan` (from `GlinerExtractor`) → typed entity_type (`Person` / `Organization` / `Topic` / etc.) via a small label-mapping table.
- New command `extract_and_upsert(text)` — runs NER, upserts every span, returns `Vec<{ span, entity_id, was_new }>`.
- Test: extract "Bill Gates founded Microsoft" twice in a row; second call returns existing IDs with `was_new = false` for both entities.

### P2-M2 — Relation extraction + bi-temporal fact writes (~4 days)
- Use `gline-rs` `RelationPipeline` with a Sediment-specific `RelationSchema` (works_at, located_in, attended, owns, etc. — kept small to start).
- New `MemoryStore::relate_fact(in_id, predicate, out_id, valid_from, source_chat_id) -> FactId` that runs **inside a single SurrealDB transaction**:
  1. Query for existing facts where `in = $in AND predicate = $pred AND out != $out AND valid_to IS NONE`.
  2. If any, set their `valid_to = $valid_from` (supersession, history preserved).
  3. RELATE the new fact.
- Conflict-detection unit test: write "John works_at Acme" with valid_from 2024-01-01; write "John works_at Beta" with valid_from 2026-03-15; assert old fact's valid_to was backfilled and both edges retained.
- Test that NOT a real contradiction (different predicate, same in/out) doesn't trigger supersession.

### P2-M3 — Auto-index on note save (~2 days)
- After `commands::formation::write_note` succeeds, spawn a background task that runs `index_note` for the path. Per-path debounce (e.g. 1500ms) so rapid Cmd+S presses coalesce into one re-embed pass.
- Also wired into the watcher: `formation-change` events with kind `created` or `modified` queue the same task for any `.md` path.
- Test: write_note then immediately query search_notes for content in the new note — top hit is the new path within ~2s on a small formation.

### P2-M4 — Background formation indexing on launch (~3 days)
- New `commands::memory::index_formation(force: bool)` — walks the open formation, queues every `.md` file for indexing. Skips files whose mtime ≤ the last indexed timestamp (new field on `note_chunk` or a sibling `note_index_state` table).
- Emits `index-progress` Tauri events with `{ done, total, current_path }`. React subscribes and shows a small progress affordance in the title bar.
- Auto-runs on first launch after `open_formation` if the formation has > 0 notes and < 50% are indexed.
- Test: drop 10 markdown files in a fresh formation, open it, watch progress emit, confirm all 10 are queryable after completion.

### P2-M5 — Write-mode chat pipeline (~4 days)
- New `commands::chat::chat_write(message, on_token: Channel<ChatEvent>) -> ()` that ties extraction to storage. Pipeline:
  1. Persist user message in SurrealDB `chat_message` → `source_chat_id`.
  2. NER + RE via gline-rs.
  3. Upsert entities (P2-M1) + relate facts (P2-M2).
  4. Stream an assistant-side summary via Ollama: "Extracted N facts about <entities>". (No staging yet — that's Phase 3.)
- React `ChatPane` learns to route Write-mode messages through `chat_write` instead of `ollama_generate`. Mode hardcoded to Write until P2-M7.
- End-to-end test: send "Sarah is the CTO at Acme" → SurrealQL `SELECT * FROM fact WHERE in.canonical_name = 'Sarah'` returns the new fact with `source_chat_id` resolving to the just-stored chat row.

### P2-M6 — Ask-mode RAG with citations (~3 days)
- New `commands::chat::chat_ask(query, on_token: Channel<ChatEvent>) -> ()`. Pipeline:
  1. Extract entities from the query (graphrag-rs-style query expansion).
  2. Hybrid SurrealQL query: graph traversal on those entities + HNSW search on `note_chunk.embedding` for the query embedding. Merge top-K.
  3. Send context + query to Ollama with the spec §11.4 prompt (cite-or-refuse).
  4. Stream answer; parse `[[Note Name]]` patterns in the stream and emit as structured citation events so the UI can render them clickable.
- React `ChatPane` renders citations as `<button>` that calls `openNote(relative_path)`.
- End-to-end test: index a sample formation with a known fact, ask the question, get an answer with at least one citation pointing at the right note.

### P2-M7 — Intent classifier (~2 days)
- New `commands::chat::classify_intent(message) -> { mode: "write"|"ask", confidence: f32 }`. Backed by a small Ollama prompt (tier-appropriate); cache the last N classifications.
- `/write` and `/ask` slash commands hard-override; falls through to LLM classification otherwise. Confidence below 0.8 surfaces a UI prompt: "Treating as Write — Switch to Ask?".
- React `ChatPane` shows mode indicator + override link above the send button.
- Test: a clearly-interrogative message ("what is Sarah's role?") classifies as Ask with confidence > 0.85; a clearly-declarative one ("Sarah is the new CTO") as Write.

### P2-M8 — ADRs + verify end-to-end (~2 days)
- `docs/adr/0003-extraction-pipeline.md` — records the gline-rs schema, label mapping, supersession algorithm.
- `docs/adr/0004-bitemporal-contradiction-detection.md` — the SQL pattern, edge cases (identical fact restated; same predicate same subject same object different times; rapid back-and-forth changes).
- Update [README.md](../../README.md) with a "Phase 2 verification" section walking through the full write + ask flow.
- Add an integration test that exercises the whole pipeline in one go on a tempdir formation.

**Total estimate:** ~23 days. Matches spec §15's "3-4 weeks for Phase 2".

## Critical files to create or update

| Path | Purpose | Milestone |
|---|---|---|
| `src-tauri/src/core/memory.rs` | `upsert_entity`, `relate_fact`, supersession SQL | P2-M1, P2-M2 |
| `src-tauri/src/core/extraction.rs` | Wire `RelationPipeline`; `RelationSchema` constants | P2-M2 |
| `src-tauri/src/core/indexer.rs` *(new)* | Background per-path indexing task + queue | P2-M3, P2-M4 |
| `src-tauri/src/commands/chat.rs` *(new)* | `chat_write`, `chat_ask`, `classify_intent` | P2-M5, P2-M6, P2-M7 |
| `src/components/ChatPane.tsx` | Route Write through `chat_write`; render citations | P2-M5, P2-M6, P2-M7 |
| `src/components/IndexProgress.tsx` *(new)* | Title-bar progress affordance for background indexing | P2-M4 |
| `docs/adr/0003-extraction-pipeline.md` *(new)* | Extraction architecture record | P2-M8 |
| `docs/adr/0004-bitemporal-contradiction-detection.md` *(new)* | Supersession SQL pattern + edge cases | P2-M8 |

## Verification (Phase 2 acceptance)

End-to-end manual run after P2-M8:

1. **Fresh formation indexing.** Open a formation with 10 prepared `.md` files. Watch the progress indicator climb to 10/10 within tier-appropriate time (Standard tier: < 1 min).
2. **Write a fact via chat.** Send "Sarah is the CTO at Acme since 2024." Verify SurrealDB has `Sarah` and `Acme` entities, a `cto_at` (or normalized) fact with `valid_from = 2024-01-01` and `source_chat_id` matching the chat row.
3. **Supersession.** Send "Sarah left Acme on 2026-04-01 and joined Beta." Verify the original fact has `valid_to = 2026-04-01` and a new fact for Beta starts at 2026-04-01.
4. **Ask with citation.** Switch to Ask (or rely on classifier): "Where does Sarah work?" Expect "Sarah is the CTO at Beta" with a citation pointing at the chat-history note or the relevant note in the formation.
5. **Point-in-time query.** SurrealQL: `SELECT * FROM fact WHERE in.canonical_name = 'Sarah' AND valid_from <= d'2025-06-01T00:00:00Z' AND (valid_to IS NONE OR valid_to > d'2025-06-01T00:00:00Z')` returns the Acme fact.
6. **External edit triggers re-index.** Edit one of the formation files outside Sediment; confirm the watcher fires and `search_notes` reflects the new content within ~2s.

Automated:
- All Phase 1 tests stay green
- New tests for: upsert idempotence (P2-M1), bi-temporal supersession (P2-M2), per-path debounce (P2-M3), indexer progress accounting (P2-M4), full pipeline integration (P2-M8)
- Intent classifier acceptance test (P2-M7) gated behind `#[ignore]` since it needs Ollama

## Out of scope for Phase 2 (deferred to later phases)

- Staging tray real content — Keep/Discard per change — atomic markdown writes from extracted facts — **Phase 3**
- Snapshot + 10s undo window — **Phase 3**
- Long-batch grouping by entity — **Phase 3 / V1.1**
- Multi-John disambiguation prompts — **V1.1**
- Confidence-based UI warnings / yellow borders on low-confidence facts — **V1.1**
- BYOK cloud fallback — **Phase 5**
- Prompt overrides directory — **V1.1**

## Decisions (resolved 2026-05-19)

1. **Predicate vocabulary — EXPANDED.** Working set of ~30 predicates organized by source-target type:

   | From → To | Predicates |
   |---|---|
   | person → org | works_at, joined, left, founded, invests_in, advises, member_of, former_member_of, volunteered_at |
   | person → person | knows, reports_to, manages, parent_of, child_of, sibling_of, partner_of, mentored_by, collaborates_with |
   | person → location | lives_in, born_in, visited |
   | person → topic | expert_in, interested_in |
   | person → meeting | attended, organized, presented_at |
   | person → project | created, contributes_to, leads |
   | person → task | owns_task, completed, delegated_to |
   | org → org | subsidiary_of, competitor_of, acquired_by, partner_with |
   | org → location | located_in, headquartered_in |
   | meeting → date | scheduled_for |
   | meeting → topic | about |
   | task → date | due_on |
   | task → task | blocks, depends_on |

   Normalization map (collapses natural variants → canonical predicate): `founded` ← founded / co-founded / started / created (org); `works_at` ← works at / employed by / employed at; `lives_in` ← lives in / resides in / based in / call X home; `met` ← met with / had a meeting with / caught up with; `joined` ← joined / started at / moved to; `left` ← left / departed / quit.

   Lives in `src-tauri/src/core/extraction.rs::PREDICATES` (typed) and `PREDICATE_ALIASES` (HashMap). Adding a new predicate requires updating one table + a unit test that the alias map round-trips.

2. **Entity ID strategy — SLUG + history.** Record ID is `entity:<slug(canonical_name)>` where slug is lowercase + non-alphanumerics → `_`. Schema gains `canonical_name_history: array<string>` field. Upsert flow: lookup by `id` first, then `canonical_name`, then membership in `aliases`. If a rename ever changes canonical_name, the old name is appended to `canonical_name_history`; the slug ID stays stable. Collision handling: if slug already exists with a different canonical_name (unrelated entity), append a `_2` / `_3` suffix.

3. **Contradiction definition — TIERED.** Default: same `(in, predicate)` with `valid_to IS NONE` and different `out` triggers supersession (set old fact's `valid_to = new fact's valid_from`). When BOTH the existing and the new fact have `confidence ≥ 0.9`, auto-supersede silently. When either is below 0.9, supersede in the data but emit a `fact-warning` Tauri event so Phase 3 staging can flag the user. Edge case ("consultant works at two places"): user can override by sending a follow-up message including both organizations explicitly — the system will then keep both as concurrent facts (handled in P2-M2 by detecting the explicit-coexist signal).

4. **Chunking strategy.** Phase 1 splits on `\n\n` with a 1500-char ceiling. Keep as-is for Phase 2; revisit only if real-formation recall is poor (deferred follow-up in Phase 2 verification).
