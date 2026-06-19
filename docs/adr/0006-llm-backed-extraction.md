# ADR-0006: LLM-backed fact extraction

**Status:** Superseded by [ADR-0009](0009-conversational-agent.md) (2026-05-22) — the `FactExtractor` trait and the LLM/GLiNER extractor split were replaced by the conversational agent, which records Facts directly through graph tools. Kept for historical context.
**Relates to:** ADR-0003 (extraction pipeline), ADR-0004 (bi-temporal facts)

## Context

ADR-0003 built Write-mode extraction on GLiNER — a multitask ONNX model doing
zero-shot NER + relation extraction. Dogfooding one realistic message exposed a
structural ceiling, not a tuning problem:

> "Standup with the platform team today, we're finally ripping the legacy auth
> service off Redis before the next release. Josh mentioned he worked at
> Cloudflare back in 2019, which explains why he keeps pushing edge caching
> over a central store. Don't let me forget to finish my CBT today."

GLiNER extracted one fact, and it was wrong: it asserted Josh *currently* works
at Cloudflare. Everything else — the standup, the migration, the CBT task,
Josh's stance — was dropped. The misses trace to limits a schema-driven NER+RE
model cannot cross:

- **No first-person subject.** "I" / "we" / "my" are never entities, so every
  fact about the note-taker is unrepresentable.
- **No coreference.** "he worked at..." / "he keeps pushing..." — the subject
  is a pronoun GLiNER will not bind to "Josh".
- **Binary relations only.** Tasks, events, and opinions are not (subject,
  predicate, object) triples between two named entities.
- **No tense.** A closed interval ("worked ... back in 2019") collapses onto a
  present-tense predicate.

These are outside GLiNER's expressivity, not recall problems.

## Decision

### LLM-backed extraction behind the `FactExtractor` trait

ADR-0003 anticipated this: "an LLM-grammar-constrained extractor can drop in
behind the same trait." We add `FactExtractor`, an async trait returning a
structured `Extraction` — entities carrying an `is_self` flag, relations
carrying optional `valid_from` / `valid_to`. Two implementations:

- `LlmExtractor` — the primary. Prompts the tier's Ollama chat model for a
  JSON `Extraction`, requested in JSON mode and validated by `serde`.
- `GlinerExtractor` — the fallback, kept intact. It maps NER+RE output into the
  same `Extraction`; it cannot set `is_self` or `valid_to`.

ADR-0003's objection to LLM extraction ("JSON-shaped-output flakiness") is
answered by Ollama's JSON mode plus a deserialize-or-error boundary: a malformed
response fails the turn cleanly rather than corrupting a note.

### Tense as a closed interval, not a new predicate

A historical fact carries an explicit `valid_to`. `relate_fact` writes it as a
non-current edge and skips supersession for it; `render_fact_bullet` derives
past-tense phrasing ("Worked at") from the closed interval. No predicate
vocabulary change and no schema change — the `fact` table already has
`valid_to`.

### The note-taker as a first-class entity

An entity flagged `is_self` resolves to one canonical "Me" person; its facts
route to `People/Me.md` regardless of the pronoun the message used. The LLM
extractor does the pronoun resolution; GLiNER cannot and simply never sets the
flag.

### Test strategy — two layers

A live LLM cannot be a deterministic CI gate, so the e2e suite is split:

- **Layer 1 — deterministic pipeline test.** A `ScriptedExtractor` replays a
  hand-authored ground-truth `Extraction`; the full extract → stage → commit
  pipeline is asserted against the resulting note files. This is the CI gate
  and the TDD loop.
- **Layer 2 — live extraction test (`#[ignore]`).** Runs `LlmExtractor`
  against the real model and scores recall against the same fixture. Run
  manually; quality tracks the model tier.

## Consequences

- **Positive** — first-person facts, coreference, tense, opinions, tasks, and
  events all become representable. The trait keeps GLiNER as a zero-dependency
  fallback for BYOK / no-model environments.
- **Negative** — extraction is now an LLM round-trip: slower, and quality
  scales with the tier's model (llama3.2:3b on Lite is a floor, not a target).
- **Negative** — a 3B model will not saturate the Layer-2 recall bar; that test
  reports a number rather than gating CI.
- **Neutral** — `Extraction` is the pipeline contract now; future extractors (a
  fine-tuned model, a cloud BYOK call) implement one async method.
- **Out of scope** — causal links ("which explains why ...") remain undropped
  but uncaptured; representing them needs its own edge type and ADR.
