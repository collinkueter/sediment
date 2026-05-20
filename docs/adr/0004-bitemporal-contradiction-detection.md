# ADR-0004: Bi-temporal facts and contradiction detection

**Status:** Accepted (2026-05-20)
**Relates to:** ADR-0001 (SurrealDB), Phase 2 milestone P2-M2

## Context

A fact in Sediment is not static. "John works at Acme" can later become "John works at Beta." The spec's defining promise (§1, §6) is that old facts don't vanish — they get a validity window and stay queryable as history. This ADR records how that is implemented.

## Decision

### The fact edge

Facts are `RELATION` edges in SurrealDB's `fact` table, directed `entity → entity`. Each carries:

- `predicate` — e.g. `works_at`
- `valid_from` — datetime the fact became true
- `valid_to` — datetime it stopped being true, or `NONE` for a currently-valid fact
- `source_chat_id` — provenance pointer to the originating `chat_message`
- `confidence` — 0.0–1.0 from the extractor

A fact is **current** iff `valid_to IS NONE`.

### The supersession algorithm

`MemoryStore::relate_fact` writes a new fact in a two-statement batch:

```surql
-- 1. Close out any current fact with the same (subject, predicate) and a
--    DIFFERENT object — that is the contradiction.
UPDATE fact SET valid_to = $valid_from
  WHERE in = entity:<subject>
    AND predicate = $predicate
    AND out != entity:<object>
    AND valid_to IS NONE;

-- 2. Write the new fact, open-ended.
RELATE entity:<subject> -> fact -> entity:<object>
  SET predicate = $predicate, valid_from = $valid_from,
      valid_to = NONE, source_chat_id = $chat, confidence = $conf;
```

Both statements run in one `.query()` call, so the close-out and the new write are atomic from any reader's perspective. History is preserved: the superseded edge keeps all its fields, only `valid_to` is filled in.

### What counts as a contradiction

Supersession fires only when **all** hold:

- same subject
- same predicate
- **different** object
- the existing fact is currently valid (`valid_to IS NONE`)

Restating the *same* `(subject, predicate, object)` does NOT supersede — it is the same fact, not a contradiction. (Verified by the `relate_fact_does_not_supersede_same_object` test.)

### Tiered confidence handling

Per the docs/plans/phase-2.md decision: when both the existing and incoming facts have `confidence >= 0.9`, supersession is silent. When either is below 0.9, the write still happens but the UI should flag it (a `fact-warning` surface) so the user can review in the Phase 3 staging tray. *(The warning event is a Phase 3 wiring task; the data-side supersession is unconditional today.)*

### Temporal queries

- **Current state:** `SELECT * FROM fact WHERE in = $e AND valid_to IS NONE`
- **Point-in-time** (as of `$ts`): `... AND valid_from <= $ts AND (valid_to IS NONE OR valid_to > $ts)`

Both are exercised by the `temporal_fact_round_trip` test.

## Consequences

- **Positive** — history is never destroyed; point-in-time queries "what did we know about X on date D" fall straight out of the model. The atomic two-statement batch means no reader ever sees two current facts mid-write.
- **Negative** — the "consultant works at two places at once" case is misread as a contradiction: the second `works_at` closes the first. The decision record's mitigation (user re-states both explicitly to mark them concurrent) is **not yet implemented** — it needs an "explicit coexist" signal in the extraction output. Tracked as a Phase 3 follow-up.
- **Negative** — `valid_from` currently defaults to "now" (the time of the chat message). Real temporal phrasing in the message ("...since 2024", "...left in March") is not yet parsed into `valid_from` / `valid_to`. A date-extraction pass (LLM or rule-based) is a Phase 3 enhancement; until then all facts are stamped at message time.
- **Negative** — `relate_fact` interpolates the subject/object slugs into the SQL string (SurrealDB rejects bound params in `RELATE` id position). Slugs are `[a-z0-9_]` only, so injection risk is nil, but the pattern is worth remembering.
