# Sediment — Phase 3 Plan: Staging Tray & Review-Before-Commit

**Status:** Planned (2026-05-20)
**Predecessor:** Phase 2 complete at commit `a70c949` — see [phase-2.md](phase-2.md)

---

## Context for a fresh session

This document is written to be implemented after a context clear. Everything
needed to start is here.

**What Sediment is.** A macOS desktop app (Tauri 2 + Rust core + React 19/TS).
The user chats; the app extracts entities and relations and files them into a
"formation" — a folder of Obsidian-compatible markdown notes. See
[sediment-tech-spec.md](../../sediment-tech-spec.md) (v0.2) and
[docs/adr/](../adr/) for the architecture.

**Stack.** Tauri 2, React 19 + TypeScript + Vite + Tailwind v4 + Zustand,
CodeMirror 6. Rust core: SurrealDB embedded (`kv-surrealkv`) for graph +
vectors + docs; `gline-rs` (multitask GLiNER ONNX) for NER + RE; Ollama for
chat + embeddings. No Python.

**What's built (Phase 1 + 2, all committed, 20 unit tests + 2 ignored pass):**
- Formation open / file I/O / debounced watcher / background indexing
- Embedded SurrealDB: `entity`, `fact` (bi-temporal RELATION edges),
  `note_chunk` (HNSW vectors), `chat_message`, `note_index_state`
- Extraction: `gline-rs` NER + RE, verified against the real model
- `chat_write`: extracts facts and **writes them straight to SurrealDB**
- `chat_ask`: hybrid retrieval + streamed cited answer
- Intent classifier (heuristic), onboarding, hardware tiering

**Key files:**
- `src-tauri/src/commands/chat.rs` — `chat_write`, `chat_ask`, `classify_intent`
- `src-tauri/src/commands/extraction.rs` — `run_extract_facts` (plain fn),
  `extract_facts` command
- `src-tauri/src/core/extraction.rs` — `extract_entities_and_relations`,
  `GlinerExtractor`, `default_relation_schema`, `ENTITY_LABELS`
- `src-tauri/src/core/memory.rs` — `MemoryStore`: `upsert_entity`,
  `relate_fact`, `current_facts`, `insert_chat_message`, `record_id_to_string`,
  `slugify`, schema in `SCHEMA_SQL`
- `src-tauri/src/core/formation_state.rs` — `atomic_write`, `AppConfig`
- `src-tauri/src/commands/formation.rs` — `APP_DIR` (`.chat-notes`),
  `init_chat_notes_skeleton` (already creates `snapshots/ staging/
  chat-history/`), `walk_notes`
- `src/components/StagingTray.tsx` — placeholder, no real content
- `src/components/NoteViewer.tsx` — CodeMirror 6 host
- `src/lib/store.ts` — `useFormationStore`, `useChatStore`, `useUiStore`
  (`stagingTrayOpen`)
- `src/lib/tauri.ts` — typed `invoke` wrappers

**Verification convention.** Every milestone ends with `cargo clippy
--all-targets -- -D warnings`, `cargo fmt --check`, `cargo test --lib`,
`npx biome check src`, `npx tsc --noEmit` all clean. Commit per milestone.
Gotcha: SurrealKV tests create a stray `src-tauri/surrealkv:/` dir — it's
gitignored; `rm -rf` it before `git add`.

---

## The architectural shift

Phase 2's `chat_write` violates spec principle #3 ("AI proposes, human
disposes"): extracted facts hit SurrealDB **immediately**, with no review.
Phase 3 inserts a staging layer.

```
                  Phase 2 (now)                Phase 3 (target)
chat_write   →  extract → upsert+relate    extract → route → diff
                (LIVE in SurrealDB)        → write staging entry JSON
                                           (nothing committed yet)
                                                      │
                                            user reviews in tray
                                                      │
                                          Keep ───────┴─────── Discard
                                            │                    │
                                  snapshot notes           delete entry
                                  write markdown diffs      (no-op else)
                                  upsert+relate in SurrealDB
                                  re-embed changed notes
                                  10s undo window
```

The crucial reframing: **the markdown note is the artifact.** A fact does not
just become a graph edge — it becomes a line in a note (spec §3: "Documents
are the output"). The graph mirrors the notes. So Phase 3 adds a *fact router*
(which note does this fact belong in?) and a *diff generator* (what markdown
change does it produce?), and SurrealDB writes become a side effect of
committing a note change.

---

## Decisions (made here so implementation isn't blocked)

1. **Diff generation is template-based, not LLM-based (for V1).**
   Each fact renders to a deterministic markdown bullet under a managed
   `## Facts` section of the target note. Rationale: a 3B local model's
   free-prose merge is unreliable and hard to diff cleanly; a structured
   section is deterministic, testable, and still Obsidian-readable. The
   LLM-polished natural-prose merge (spec §8 `diff_generator`) is a documented
   post-V1 enhancement. A predicate→phrasing table renders `works_at` →
   "Works at", `founded` → "Founded", etc.

2. **Fact routing: route to the subject entity's note.**
   Each fact `(subject, predicate, object)` is filed in the *subject* entity's
   note. Resolution: if the entity has `note_path` set, it's an `update`;
   otherwise it's a `create` at `<Folder>/<Canonical Name>.md` where Folder is
   derived from `entity_type` (`person`→`People`, `organization`→
   `Organizations`, `project`→`Projects`, `meeting`→`Meetings`, `task`→`Tasks`,
   else `Notes`). On commit, the entity's `note_path` is set.

3. **Staging entries are JSON files in `.chat-notes/staging/`.**
   Per spec §6 "Staging State". One file per `chat_write` batch,
   `stage_<rfc3339>.json`. Not a SurrealDB table — staging is pre-commit,
   ephemeral, and must survive a graph rebuild. The directory already exists
   (`init_chat_notes_skeleton`).

4. **Diff visualization uses `@codemirror/merge`'s `unifiedMergeView`.**
   Resolves spec open question #4. The official CodeMirror package renders a
   single-editor unified diff with per-chunk accept/reject gutter controls —
   exactly the staging-review UX. Add `@codemirror/merge` to `package.json`.

5. **Facts in a note live under a managed `## Facts` section** as bullets,
   with the `chat-notes` YAML frontmatter block tracking `fact-id` →
   `source-chat` provenance (spec §6 frontmatter convention). The diff
   generator creates the section + frontmatter block if absent. User prose
   elsewhere in the note is never touched.

6. **Snapshots are per-note file copies** into
   `.chat-notes/snapshots/<staging-id>/<relative-path>` taken immediately
   before a commit writes. Undo restores them. Whole-formation snapshots are
   overkill.

7. **`run_extract_facts` splits.** The current function does extract + commit.
   Split into: `extract_facts_only` (NER + RE, returns spans, **no DB writes**)
   and the commit path (`upsert_entity` + `relate_fact`) which moves into the
   Keep handler. `chat_write` calls extract-only → route → diff → stage.

---

## Milestones

Each is self-contained: implementable in a fresh session from this doc + the
codebase. Commit at each boundary.

### P3-M1 — Staging data model + persistence

- New `src-tauri/src/core/staging.rs`:
  - `StagingEntry { id, created, chat_message_id, chat_excerpt, status,
    changes: Vec<NoteChange> }`
  - `NoteChange { kind: "create"|"update", note_path, diff (unified-ish
    markdown), new_content (full note text after change), facts:
    Vec<StagedFact>, confidence }`
  - `StagedFact { subject_id, subject_name, predicate, object_id,
    object_name, valid_from, confidence }` — enough to run `relate_fact` on
    commit without re-extracting.
  - `read_all(staging_dir)`, `read_one(id)`, `write(entry)`, `remove(id)` —
    JSON via `serde_json`, atomic writes via `formation_state::atomic_write`.
- Commands in `src-tauri/src/commands/staging.rs`: `list_staging`,
  `get_staging(id)`, `discard_staging(id)` (deletes the JSON; no other effect).
- Register module + commands in `lib.rs` and `commands/mod.rs`.
- Tests: round-trip a `StagingEntry` through write/read/remove in a tempdir.
- **Verify:** `cargo test`; `discard_staging` removes only the file.

### P3-M2 — Fact router + diff generator

- New `src-tauri/src/core/router.rs`:
  - `route_fact(entity_type, canonical_name, existing_note_path) ->
    (note_path, ChangeKind)` — decision #2.
  - `entity_type_folder(entity_type) -> &str`.
- New `src-tauri/src/core/diff_gen.rs`:
  - `predicate_phrasing(predicate) -> String` — the lookup table (decision #1).
  - `render_fact_bullet(fact) -> String` — `"- Founded Microsoft"`.
  - `apply_facts_to_note(existing_content: Option<&str>, facts: &[StagedFact])
    -> NoteChange` — inserts/updates the `## Facts` section and the
    `chat-notes` frontmatter block; returns the new full content + a unified
    diff string. Never edits user prose outside the managed section.
- Tests: new note creation has frontmatter + `## Facts`; updating an existing
  note appends under the section without disturbing other content; idempotent
  re-apply of the same fact does not duplicate the bullet.
- **Verify:** `cargo test` — pure functions, no models needed.

### P3-M3 — Rewire `chat_write` to stage instead of commit

- Split `commands/extraction.rs::run_extract_facts`: add
  `extract_facts_only(text, formation_root) -> AppResult<(Vec<EntitySpan>,
  Vec<RelationSpan>)>` with **no SurrealDB writes**. Keep the entity-resolution
  helper but call it without persisting (or resolve names→slugs locally).
- `chat_write` new flow: `insert_chat_message` → `extract_facts_only` → for
  each relation, `route_fact` + group by note → `diff_gen::apply_facts_to_note`
  per affected note → assemble a `StagingEntry` → `staging::write`.
  Return the entry (or its id) instead of `ExtractFactsResult`.
- Emit a `staging-created` Tauri event with the entry so the UI updates live.
- The Phase 2 direct-commit path is gone from `chat_write`; the actual
  `upsert_entity`/`relate_fact` calls move to P3-M6's commit handler.
- **Verify:** `cargo test`; `cargo clippy`. (E2E needs the GLiNER model — the
  staging entry should appear as a JSON file under `.chat-notes/staging/`.)

### P3-M4 — Staging tray UI

- Rebuild `src/components/StagingTray.tsx` per spec §9:
  - Collapsed bar shows "Staged changes — N notes affected" / "none yet".
  - Expanded: the chat excerpt, then a row per `NoteChange` —
    `✎`/`➕` icon, `[[note path]]`, `+N facts`, `[view]` `[✗]`.
  - `[Discard All]` and `[Keep All]` buttons.
  - `[view]` selects the note in the left pane with diff mode on (P3-M5).
- New `useStagingStore` in `src/lib/store.ts`: holds the active
  `StagingEntry[]`, `refresh()`, subscribes to the `staging-created` event in
  `App.tsx`.
- `src/lib/tauri.ts`: typed wrappers + interfaces for the staging commands.
- **Verify:** Claude Preview — the tray renders rows from a mocked entry; the
  collapse/expand works.

### P3-M5 — Diff visualization in the note viewer

- `npm install @codemirror/merge`.
- `NoteViewer.tsx` gains a diff mode: when the formation store's
  `currentNotePath` matches a staged `NoteChange`, render the note with
  `unifiedMergeView` (original = on-disk content, modified = `new_content`),
  showing per-chunk accept/reject gutter controls.
- Clicking `[view]` in the tray sets `currentNotePath` + a `diffFor` staging
  reference; `NoteViewer` switches to merge view.
- Accept/reject on individual chunks updates the staged `NoteChange` (a
  partially-accepted change is allowed).
- **Verify:** Claude Preview — open a note with a staged change, see green
  insertion gutter, accept/reject a chunk.

### P3-M6 — Commit pipeline: Keep / Discard + snapshot + undo

- New `src-tauri/src/commands/staging.rs::keep_staging(id, note_paths?)`:
  1. Snapshot every affected note into
     `.chat-notes/snapshots/<staging-id>/`.
  2. Write each `NoteChange.new_content` to disk via `atomic_write`.
  3. For each `StagedFact`: `upsert_entity` (set `note_path`), `relate_fact`.
  4. Re-index changed notes (`core/indexer::index_note_path`).
  5. Remove the staging entry. Emit `staging-committed`.
- `keep_staging` with a `note_paths` subset commits only those notes
  (individual Keep); the rest stay staged.
- `undo_commit(staging-id)`: within a 10s window, restore snapshots, delete the
  facts written (track the new fact ids from step 3), re-index. A toast in the
  UI offers "Undo" for 10s after a commit.
- Suppress the file-watcher's re-index for paths we just wrote (a short-lived
  "self-write" path set on `FormationWatcher`) so commit doesn't double-index.
- **Verify:** `cargo test` for snapshot round-trip; manual E2E: stage → Keep →
  note file on disk has the `## Facts` bullet, SurrealDB has the fact, Undo
  reverts both.

### P3-M7 — Conflict detection + resolution UI

- When `relate_fact` would supersede an existing current fact (same subject +
  predicate, different object), the staging entry flags that `NoteChange` as
  `conflicts: Vec<Conflict>` with the existing fact's details.
- This needs a *pre-commit* conflict check: a `MemoryStore::find_conflicts(
  subject_id, predicate, new_object_id) -> Vec<FactRow>` query, called during
  P3-M3 staging assembly.
- Staging tray renders a conflict banner (spec §10): side-by-side existing vs
  new, with `[Update]` / `[Keep both]` / `[Discard new]`.
- `[Keep both]` sets an `explicit_coexist` flag on the `StagedFact` so the
  commit's `relate_fact` skips supersession (addresses refinement R3, the
  consultant case — see ADR-0004).
- **Verify:** stage a contradicting fact, see the banner; each resolution path
  produces the right SurrealDB state.

### P3-M8 — Refinements + Phase 3 verification

The deferred refinements (see next section), then full verification:
- README Phase 3 section + walkthrough.
- `docs/adr/0005-staging-and-commit.md`.
- An integration test chaining stage → keep → verify note + graph, and
  stage → discard → verify no-op (no models needed — drive it with a
  hand-built `StagingEntry`).
- Full gates green; commit.

---

## Refinements (explicitly requested)

These were flagged at the end of Phase 2. Each is folded into a milestone or
P3-M8.

| # | Refinement | Where | Notes |
|---|---|---|---|
| R1 | **Staging tray / review-before-commit** | P3-M1–M6 | This *is* the body of Phase 3, not a side item. |
| R2 | **`valid_from` from temporal phrasing** | P3-M2 + P3-M8 | NER already extracts `date` entities (verified: "1975" came back at 0.995). The router/diff-gen should associate a `date` entity in the same sentence with the fact and use it as `valid_from` instead of `now()`. Heuristic first: nearest `date` span in the sequence. ADR-0004 limitation. |
| R3 | **Consultant / concurrent-employment case** | P3-M7 | The `[Keep both]` conflict resolution sets `explicit_coexist`, and the commit's `relate_fact` skips supersession for that fact. Closes the ADR-0004 "consultant" gap. May need a `relate_fact` variant or a `supersede: bool` param. |
| R4 | **Fuzzy / embedding-based entity resolution** | P3-M8 (scoped) | Today `upsert_entity` is exact-match on canonical name or alias. Full fuzzy resolution is large; **scope for Phase 3**: a candidate-suggestion step — on a `create`, run a vector/trigram similarity check against existing entity names and, if a near-match exists, surface a "Did you mean [[John Smith]]?" prompt in the staging tray (spec §10 disambiguation). Auto-merge is *not* in scope; the user decides. If this proves too big, split to a Phase 3.5 item. |
| R5 | **`surrealkv://` cwd pollution fix** | P3-M8 (or the already-spawned task) | `MemoryStore::open` builds `surrealkv://<path>`; the backend creates a literal `surrealkv:` dir in cwd. Try the raw path or `surrealkv:///<absolute>`. A spawn-task chip already exists for this; fold it in here if not done. |

---

## Open questions / risks

- **Diff-gen template vs LLM.** Decision #1 picks template-based for V1. If the
  structured `## Facts` section feels too rigid in practice, the LLM-polished
  merge is the upgrade path — but it needs a reliable diff and a bigger model.
  Revisit after dogfooding.
- **`unifiedMergeView` partial-accept semantics.** Per-chunk accept/reject
  updating a `NoteChange` mid-review needs care: the staged `new_content` and
  the on-disk `original` both shift as chunks resolve. Prototype P3-M5 early.
- **Watcher vs commit double-index.** P3-M6's self-write suppression must be
  watertight or every Keep triggers a redundant re-index. Unit-test the
  suppression set.
- **Fact provenance on Undo.** `undo_commit` must delete exactly the facts it
  created. P3-M6 step 3 must collect the returned fact ids; do not re-derive.
- **Entity-type folder collisions.** Two entities named "Acme" of different
  types would both want `Organizations/Acme.md` vs `Projects/Acme.md` — fine,
  different folders. Same type + same name is the `slugify` collision case
  already handled by `pick_available_slug` on the graph side; the note side
  needs the same `_2` suffix logic.

---

## Verification (Phase 3 acceptance)

1. **Stage:** Write-mode chat message → a JSON entry appears in
   `.chat-notes/staging/`; the tray shows the affected notes; nothing in
   SurrealDB yet.
2. **Review:** `[view]` opens the note with a green-gutter unified diff.
3. **Keep:** the note file on disk gains the `## Facts` bullet + frontmatter;
   SurrealDB now has the entity + fact; staging entry gone; "Undo" toast for 10s.
4. **Undo:** within 10s, note reverts to its snapshot and the fact is removed.
5. **Discard:** `[✗]` / Discard All removes the entry with zero disk/graph effect.
6. **Conflict:** a contradicting fact shows the side-by-side banner; `[Keep
   both]` produces two concurrent current facts.
7. **Temporal:** "John joined Acme in 2021" stamps the fact `valid_from` 2021,
   not today.
8. `cargo test` green (storage/router/diff-gen/staging unit tests, no models);
   `cargo test -- --ignored` green with the GLiNER model present; clippy, fmt,
   biome, tsc clean.
