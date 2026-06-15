# Sediment — Plan: Tasks & Reminders

**Status:** Implemented (2026-05-20) — milestones M0–M8 complete; see ADR-0007.
**Predecessor:** Phase 5 (polish) — current HEAD `6e9af73`. Builds on the
Write → stage → review → commit pipeline (Phase 3, ADR-0005) and LLM-backed
extraction (Phase 4, ADR-0006).

This is a *new subsystem*, beyond the spec's Phase 1–5 arc. It adds a
first-class task list, due-date reminders, OS + in-app notification, and
chat-message reminder capture.

---

## Context for a fresh session

Today a "task" only exists as a knowledge-graph relation. The LLM extractor
prompt (`core/llm_extractor.rs`) already recognises `task` entities and emits
`owns_task` / `due_on` relations, which `core/router.rs` files as `## Facts`
bullets on a person's note. There is **no task state** (open/done), **no
structured due time**, **no aggregate task list**, and **no notification**.
`due_on` points at a `date` *entity* — not a timestamp anything can schedule.

This plan adds a `Task` as its own model, distinct from the `task` graph
entity. A `Task` is something the user wants to *do* and be *reminded* about.
The `task` entity type and `owns_task` relation stay as-is for facts like
"Josh owns the migration" — they are not touched here.

### Decisions locked with the user (2026-05-20)

1. **Notification: OS + in-app.** Native desktop notification via
   `tauri-plugin-notification` (fires while the app runs, even backgrounded)
   plus an in-app reminder surface when the window is focused.
2. **Storage: `Tasks.md` + graph.** A managed `## Tasks` checklist region in a
   single `Tasks.md` note is the canonical, Obsidian-compatible store. It is
   mirrored into a SurrealDB `task` table so the scheduler can query due times.
3. **Capture: via the staging tray.** A reminder detected in a Write-mode
   message stages like a fact — the user reviews and Keeps it before it becomes
   a task. Consistent with spec principle #3, "AI proposes, human disposes."
   (The spec's staging mockup, §577, already shows `✎ [[Tasks]] +1 item`.)

---

## Architecture

```
chat message ──► extract_facts() ──► Extraction { entities, relations, tasks* }
                                              │ *new
                          run_chat_write ─────┤
                                              ▼
                       NoteChange { note_path: "Tasks.md", staged_tasks* }
                                              │
                                   staging tray (review)
                                              │ Keep
                                              ▼
              ┌─────────────────────────────────────────────┐
              │  Tasks.md  ## Tasks region  (canonical text) │
              │  SurrealDB `task` table     (queryable)      │
              └─────────────────────────────────────────────┘
                                              ▲
                    watcher ──► indexer reconciles external edits
                                              │
                  core/reminders.rs scheduler (tokio task)
                       │ remind_at <= now, status=open, !notified
                       ├──► OS notification (tauri-plugin-notification)
                       └──► `reminder-due` event ──► in-app surface
```

### The `Task` model

| field            | type                  | notes                                            |
|------------------|-----------------------|---------------------------------------------------|
| `id`             | `task:<slug>_<rand>`  | stable; also the `## Tasks` provenance key        |
| `title`          | `String`              | the action — "Renew passport"                     |
| `status`         | `open` \| `done`      |                                                   |
| `due`            | `Option<DateTime>`    | when it is due                                    |
| `remind_at`      | `Option<DateTime>`    | when to notify; defaults to `due` if unset        |
| `notified`       | `bool`                | scheduler sets true after firing                  |
| `created`        | `DateTime`            |                                                   |
| `completed_at`   | `Option<DateTime>`    |                                                   |
| `source_chat_id` | `String`              | provenance — the message it came from             |
| `linked_entity`  | `Option<String>`      | optional entity id (e.g. a person the task is for)|

### `Tasks.md` managed region

Mirrors the existing `## Facts` convention in `core/diff_gen.rs`: a single
managed `## Tasks` heading whose checklist lines are AI-managed, with a
`chat-notes` frontmatter `tasks:` block carrying `task-id → source-chat`
provenance (parallel to the existing `facts:` block). Prose elsewhere is never
touched.

```markdown
## Tasks

- [ ] Renew passport 📅 2026-06-01
- [x] Call the dentist 📅 2026-05-21
```

The `📅 YYYY-MM-DD` due-date syntax is Obsidian-Tasks-plugin compatible, so the
file stays useful outside Sediment. Checking a box in Obsidian is a valid edit
that the indexer reconciles back (M7).

### Why a separate `task` table, not graph facts

Reminders need point-in-time scheduling queries (`remind_at <= now`). The
bi-temporal fact graph models *validity intervals*, not *alarms*, and rebuilds
from notes. A dedicated `task` table is the right shape and is cheap in the
embedded SurrealDB already in use (`core/memory.rs`).

---

## Milestones

### M0 — ADR-0007 + dependency setup
- Draft `docs/adr/0007-tasks-and-reminders.md` recording the three locked
  decisions and the `Task`-vs-`task`-entity distinction.
- Add `tauri-plugin-notification` to `src-tauri/Cargo.toml`, register it in
  `src-tauri/src/lib.rs`, and add `@tauri-apps/plugin-notification` to
  `package.json`.
- Add the `notification:default` permission to
  `src-tauri/capabilities/default.json`.
- **Verify:** `cargo check` + `npm run build` clean; app launches.

### M1 — `Task` model + SurrealDB schema
- New `core/tasks.rs`: the `Task` struct, a `DEFINE TABLE task` migration in
  `core/memory.rs`'s schema bootstrap, and CRUD: `upsert_task`, `list_tasks`,
  `set_task_status`, `due_tasks(now)`, `mark_notified`.
- **Verify:** unit tests against the embedded DB (no models needed) — round-trip,
  `due_tasks` time filter, status transition.

### M2 — `Tasks.md` managed region
- Extend `core/diff_gen.rs` (or a sibling `core/task_diff_gen.rs`) with
  `apply_tasks_to_note` — a `## Tasks` checklist region + `tasks:` provenance
  block, idempotent re-apply (mirrors `apply_facts_to_note`).
- A checklist renderer: `Task` → `- [ ] Title 📅 date` and a parser for the
  reverse direction (reused by M7).
- **Verify:** unit tests — create/update/idempotence/box-state round-trip,
  prose untouched.

### M3 — Extraction: reminders
- Add `tasks: Vec<ExtractedTask>` to `Extraction` (`core/extraction.rs`).
- Extend the `LlmExtractor` JSON schema + few-shot example in
  `build_prompt` (`core/llm_extractor.rs`) to emit
  `{"tasks":[{"title","due","remind_at"}]}`; extend the lenient DTO.
- GLiNER fallback: synthesise an `ExtractedTask` from an `owns_task` + optional
  `due_on` relation pair so the deterministic path still produces a task.
- Relative-date parsing: extend `parse_when` for "tomorrow", "next Friday",
  "in 3 days", "9am" against a `now` reference.
- **Verify:** DTO-mapping unit tests (deterministic, no model); the live
  `LlmExtractor` recall test stays `#[ignore]`d per ADR-0006.

### M4 — Staging integration
- Add `staged_tasks: Vec<StagedTask>` to `NoteChange` (`core/staging.rs`),
  `#[serde(default)]` for back-compat with existing staging JSON.
- In `run_chat_write` (`commands/chat.rs:147`), when `extraction.tasks` is
  non-empty, build a `NoteChange` targeting `Tasks.md` carrying the staged
  tasks; the diff shows `+- [ ] …` lines.
- Extend `keep_staging` (`commands/staging.rs:480`): on commit, write the
  checklist line(s) to `Tasks.md` (snapshotted for undo, already supported) and
  `upsert_task` the records; record new task ids in the `UndoRecord` so undo
  deletes them (parallel to `new_fact_ids`).
- `StagingTray.tsx` renders task rows (`➕ Tasks.md · +1 task`); extend the
  `NoteChange` / `StagedFact` types in `src/lib/tauri.ts`.
- **Verify:** the `commands/staging.rs` integration test gains a stage → keep →
  task-in-`Tasks.md`-and-table case, and stage → discard → no-op.

### M5 — Scheduler + notifications
- New `core/reminders.rs`: a background tokio task. On a tick (sleep until the
  next `remind_at`, with a 60s ceiling) it runs `due_tasks(now)` for
  `status=open, notified=false`, fires an OS notification and a `reminder-due`
  Tauri event per task, then `mark_notified`.
- Spawn it from `lib.rs` setup on the Tauri async runtime — same pattern as the
  indexer (commit `a5883c3`, "Spawn indexer on Tauri async runtime").
- Reload on formation open/switch. Tauri commands: `snooze_task(id, until)`,
  `complete_task(id)`.
- Request macOS notification permission on first reminder commit (or fold into
  onboarding); handle a denied permission gracefully (in-app surface only).
- **Verify:** scheduler unit test with an injectable clock — a task due in the
  past fires exactly once and flips `notified`.

### M6 — In-app reminder surface
- A bell button + unread count in `TitleBar` (`src/App.tsx`) opening a
  Reminders popover: due now + upcoming, with Complete / Snooze actions.
- A `reminder-due` toast, modelled on `UndoToast.tsx`.
- New `useRemindersStore` slice in `src/lib/store.ts`; `tauri.ts` wrappers for
  `list_tasks`, `snooze_task`, `complete_task`; subscribe to `reminder-due`.
- **Verify:** `npm run tauri dev` — stage and Keep a reminder, confirm the
  toast + OS notification fire and the popover lists the task.

### M7 — External-edit reconciliation
- The indexer (`core/indexer.rs`) parses the `## Tasks` region when `Tasks.md`
  changes (watcher event) and reconciles the `task` table: a `- [x]` line →
  `status=done` + cancel its reminder; a removed line → delete the task; a
  hand-added line → a new task record.
- Keeps the plain-text file authoritative, consistent with the formation
  philosophy and the existing `## Facts` re-index behaviour.
- **Verify:** edit `Tasks.md` outside Sediment, confirm the table reconciles
  within the watcher debounce window (~2s).

### M8 — End-to-end + polish
- e2e test: "Remind me to call the dentist tomorrow at 9am" → staged on
  `Tasks.md` → Keep → checklist line + `task` record → scheduler fires at the
  due time (injected clock).
- Notification click deep-links to the task in the Reminders popover.
- Catch-up: on launch the scheduler surfaces reminders that came due while the
  app was closed (see Open Questions).
- Update `README.md` (project layout + a verification section).

---

## Open questions / out of scope

- **App-closed reminders.** `tauri-plugin-notification` only fires while the
  process runs. True background alarms (app quit) need an OS launch agent —
  **out of scope for V1.** Mitigation: on next launch the scheduler fires any
  overdue, un-notified reminders late and the in-app inbox lists them.
- **Recurring reminders** ("every Monday") — out of scope for V1; the `Task`
  model leaves room for a future `recurrence` field.
- **Reminder vs. plain task.** A message can mention a to-do with no time
  ("I should email Sam"). V1: extract it as a `Task` with `due = None` — it
  lands in `Tasks.md` but never schedules a notification.
- **Timezone.** All times stored UTC; render in the OS local zone.
- **Ask-mode** could answer "what's due this week" by querying the `task`
  table — a natural follow-up, not in this plan.

---

## Test strategy

Follows the repo convention: the deterministic half is the CI gate. M1, M2, M5
(injected clock), and the M4 staging integration test all run under
`cargo test` with **no models**. The model-dependent M3 `LlmExtractor` recall
test is `#[ignore]`d, matching ADR-0006. The notification + popover paths (M5,
M6) are verified manually via `npm run tauri dev`.
