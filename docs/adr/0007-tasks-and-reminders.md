# ADR-0007: Tasks and reminders

**Status:** Accepted (2026-05-20)
**Relates to:** ADR-0005 (staging and commit), ADR-0006 (LLM-backed extraction)
**Plan:** [docs/plans/tasks-and-reminders.md](../plans/tasks-and-reminders.md)

## Context

Sediment models a "task" only as a knowledge-graph relation — `owns_task`,
`due_on` — filed as `## Facts` bullets on a person's note. ADR-0006 noted the
structural gap: tasks are not `(subject, predicate, object)` triples. There is
no task *state* (open / done), no schedulable due time (`due_on` points at a
`date` *entity*, not a timestamp), no aggregate list, and no notification.

A reminder — something the user wants to *do* and be *alerted* about — needs a
different shape than a bi-temporal fact. The fact graph models validity
intervals and rebuilds from notes; it is the wrong tool for a point-in-time
alarm.

## Decision

### A `Task` is its own model, distinct from the `task` graph entity

The `task` entity type and `owns_task` / `due_on` relations stay as-is, for
facts like "Josh owns the migration." A reminder is a new `Task` record:
`title`, `status` (open / done), `due`, `remind_at`, provenance. The two never
share storage.

### Storage: `Tasks.md` is canonical, the `task` table mirrors it

A managed `## Tasks` checklist region in a single root-level `Tasks.md` note is
the source of truth — plain markdown, Obsidian-Tasks-plugin compatible:

```markdown
- [ ] Renew passport 📅 2026-06-01 🆔 renew_passport_a1b2c3
- [x] Call the dentist 📅 2026-05-21 ✅ 2026-05-20 🆔 call_dentist_d4e5f6
```

Each line carries a `🆔` so a checklist line maps unambiguously to its record —
the line is self-describing, so (unlike `## Facts`) tasks need no `chat-notes`
frontmatter provenance block. A SurrealDB `task` table mirrors the line and adds
the scheduling-only fields the markdown does not express (`remind_at`,
`notified`, `source_chat_id`). The markdown owns `{title, status, due, id}`; the
table owns the rest. An external edit re-parses `Tasks.md` and reconciles the
table by id.

### Reminders are captured through the staging tray

A reminder detected in a Write-mode message stages like a fact — a `NoteChange`
targeting `Tasks.md` carrying `staged_tasks`. The user reviews and Keeps it
before it becomes a task. Consistent with spec principle #3, "AI proposes,
human disposes." A Keep writes the checklist line, the `task` row, and an undo
record; the existing snapshot/undo machinery covers `Tasks.md` for free.

### Notification: OS + in-app, fired by a background scheduler

`tauri-plugin-notification` raises a native desktop notification; a
`reminder-due` event drives an in-app surface (a bell + popover, a toast). A
background tokio task — `core/reminders.rs`, spawned in `lib.rs` like the
indexer — ticks, queries the `task` table for `remind_at <= now`,
`status = open`, `notified = false`, fires both, and sets `notified`.

### App-closed alarms are out of scope for V1

`tauri-plugin-notification` only fires while the process runs. True background
alarms (app quit) need an OS launch agent — deferred. The scheduler instead
fires overdue, un-notified reminders on the next launch, so nothing is lost; it
just arrives late.

## Consequences

- **Positive** — tasks gain real state, scheduling, an aggregate list, and
  notification. `Tasks.md` stays a plain Obsidian-readable file.
- **Positive** — staging reuse means tasks inherit review, snapshot, and undo
  with no new commit machinery.
- **Negative** — a second source of truth (`Tasks.md` ↔ `task` table) needs a
  reconciler; bugs there can drift the two. Mitigated by making the markdown
  authoritative for everything it can express.
- **Neutral** — `Extraction` grows a `tasks` field; the `FactExtractor`
  contract is unchanged otherwise. GLiNER synthesises tasks from
  `owns_task` / `due_on` as a weak fallback.
- **Out of scope** — recurring reminders, per-task remind-vs-due times in
  markdown, and OS-level background scheduling.
