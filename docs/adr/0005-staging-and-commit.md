# ADR-0005: Staging tray and review-before-commit

**Status:** Accepted (2026-05-20)
**Relates to:** ADR-0003 (extraction pipeline), ADR-0004 (bi-temporal facts), Phase 3 milestones P3-M1–M8

## Context

Phase 2's `chat_write` violated spec principle #3 — "AI proposes, human disposes." Extracted facts hit SurrealDB *immediately*, with no review. Phase 3 inserts a staging layer between extraction and the formation: nothing reaches a note or the graph without an explicit Keep.

The crucial reframing (spec §3, "Documents are the output"): **the markdown note is the artifact.** A fact does not just become a graph edge — it becomes a line in a note. The graph mirrors the notes. So a SurrealDB write is a *side effect* of committing a note change, not the primary action.

## Decision

### The pipeline

```
chat_write   extract → route → diff → write staging entry JSON   (nothing committed)
                                              │
                                     user reviews in tray
                                              │
                                   Keep ──────┴────── Discard
                                     │                   │
                       snapshot → write note →      delete entry
                       upsert+relate → re-index     (no other effect)
                       → undo record (10s undo)
```

### Staging entries are JSON files

One `StagingEntry` per `chat_write` batch, persisted as `.chat-notes/staging/stage_<utc>_<rand>.json` (`core/staging.rs`). Staging is pre-commit, ephemeral, and must survive a graph rebuild — so it is *not* a SurrealDB table. An entry holds `NoteChange`s; each `NoteChange` holds the full post-change note text (`new_content`), a summary `diff`, the `StagedFact`s, and any `Conflict`s.

### Fact routing — to the subject's note

`core/router.rs::route_fact` files a fact `(subject, predicate, object)` in the **subject** entity's note. If the subject already has a `note_path`, it is an `update`; otherwise a `create` at `<Folder>/<Name>.md`, where the folder is derived from `entity_type` (`person`→`People`, `organization`→`Organizations`, …, else `Notes`). On commit, `set_entity_note_path` links the entity so later facts route as updates.

### Template-based diff generation

`core/diff_gen.rs` renders each fact to a deterministic markdown bullet (`- Founded Microsoft`) under a managed `## Facts` section. A predicate→phrasing table maps `works_at`→"Works at" etc. Per-fact provenance (`fact-id → source-chat`) lives in a `chat-notes` YAML frontmatter block. **User prose outside the managed section, and user frontmatter keys outside the `chat-notes` block, are never touched.** Re-applying the same fact is idempotent — the `fact-id` (`<predicate>:<object-slug>`) is matched against the block.

This is V1's deliberate choice over an LLM-polished natural-prose merge: a 3B local model's free-prose merge is unreliable and hard to diff cleanly. A structured section is deterministic and testable. The LLM merge is the documented upgrade path.

### Diff review

The note viewer renders a staged note as a `@codemirror/merge` `unifiedMergeView` — on-disk content vs. the staged proposal, with per-chunk accept/reject controls. Reviewer edits (including chunk rejections) are persisted back into the staging entry, so a partially-accepted change commits exactly what the reviewer sees.

### Commit + undo

`commit_changes` (the testable core of the `keep_staging` command): snapshots each affected note into `.chat-notes/snapshots/<commit-id>/`, writes the new markdown, upserts entities, writes the bi-temporal facts, re-indexes, and removes (or trims) the staging entry. An `UndoRecord` captures the snapshots, the exact fact ids written, and the pre-commit staging entry. `undo_commit` — offered as a 10-second toast — restores the notes, deletes exactly those facts, re-indexes, and puts the staging entry back.

### Conflict detection

`MemoryStore::find_conflicts` runs *before* commit: it finds current facts with the same subject + predicate and a different object — the facts `relate_fact` would silently supersede. They are attached to the `NoteChange` as `Conflict`s and surfaced as a side-by-side banner. `resolve_conflict` handles the three choices: **Update** (supersede — the default), **Keep both** (sets `explicit_coexist`, so the commit calls `relate_fact_with(.., supersede = false)` — closes the ADR-0004 consultant gap), **Discard new** (drops the fact and re-renders the diff).

### Watcher self-write suppression

A commit writes note files, which the file watcher would re-index redundantly. `FormationWatcher::mark_self_write` marks just-written paths; the watcher drops the next event for each (5-second TTL).

## Consequences

- **Positive** — no fact reaches the formation unreviewed. The note is the artifact; the graph mirrors it. Snapshots make every commit reversible. Deterministic diffs are unit-testable with no model.
- **Negative — undo does not reopen superseded facts.** If a committed fact superseded an older one, undo deletes the new fact but leaves the old one closed (`valid_to` stays set). Scoped out of V1.
- **Negative — temporal `valid_from` is year-granularity and single-date only.** NER `date` spans carry no character offsets, so a fact cannot be positionally matched to a date. A message with exactly one date stamps all its facts; multiple dates fall back to message time (R2).
- **Negative — entity resolution is still exact-match.** Fuzzy / "Did you mean [[John Smith]]?" candidate suggestion (refinement R4) proved larger than a Phase 3 milestone and is **deferred to Phase 3.5**.
- **Negative — note-file last-write-wins.** If a note is edited on disk between staging and Keep, the commit overwrites that edit with `new_content`. Acceptable for V1; the diff viewer shows the current on-disk state as the base.
