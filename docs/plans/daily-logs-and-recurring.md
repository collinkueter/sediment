# Sediment — Plan: Daily logs and recurring checklists

**Status:** Implemented (2026-05-22) — M0–M6 landed the same day; cargo test 99/0, biome + npm build clean. See [ADR-0010](../adr/0010-daily-logs-and-recurring.md).
**Predecessor:** the conversational-agent plan
([docs/plans/conversational-agent.md](conversational-agent.md)) reaching at
least its M3 (the Claude Code engine + behaviour prompt landing). This plan
**depends on** that substrate; it does not need to wait for ADR-0009's M7
deletion pass, but M1 here cannot land until ADR-0009 M3 has shipped.

This adds **Daily notes** (`Daily Notes/<YYYY-MM-DD>.md`), **Weekly notes**
(`Weekly Notes/<YYYY-Www>.md`), a `Templates/` folder, indexer-driven
auto-append on task check-off, and a small extension to the audit log.

---

## Context for a fresh session

Sediment's conversational agent (ADR-0009) lets the user talk to an AI that
records, questions, and connects. Today there is no convention for
*chronological organisation* — no place for "what I did today", no recurring
habit checklist, no Monday-resets-weekly. ADR-0007's `Tasks.md` handles
one-off scheduled tasks but **explicitly deferred recurrence**.

ADR-0010 establishes two new Note kinds (Daily, Weekly), three sections
(`## Checklist`, `## Did`, `## Notes`), and a template scaffold. Most of the
work is a behaviour-prompt extension and one indexer responsibility — no new
Rust entity types, no graph schema changes.

The non-trivial Rust work is in two places:
1. **`core/indexer.rs`** gains a write role: on `Tasks.md` `open → done`
   transitions, append a bullet to today's daily note `## Did`.
2. **The audit-log machinery introduced by ADR-0009 M4** is extended to record
   non-turn events (indexer appends), with a `kind` discriminator and a
   per-event revert path that re-uses the existing UI affordances.

Kept substrate: ADR-0009's `core/claude_code.rs` (the engine), the agent's
file tools (Read/Write/Edit/Grep) for note authoring, `core/memory.rs` for
the `task` table, `prompts/conversation-agent.md` as the agent's behaviour
contract. `core/tasks.rs` and the existing `Tasks.md` reconciliation stay as
ADR-0007 specified.

### Incremental landing

M0 lands ADR + plan. M1–M2 deliver daily and weekly notes end-to-end through
prompt + scaffold work only — no Rust changes. M3 adds the indexer write
role. M4 adds the undo layer for indexer writes. M5 wires checklist box
auto-flipping into the prompt. M6 surfaces non-turn events in the audit-log
panel. The app builds and runs at every milestone.

---

## Architecture

```
chat_turn(message) ──► ConversationEngine (ADR-0009)
                              │
                              ▼
                   agent reads/writes via Claude Code's native file tools
                              │
                              ├─► Templates/Daily.md, Templates/Weekly.md  (Read)
                              ├─► Daily Notes/<YYYY-MM-DD>.md             (Read+Write+Edit)
                              ├─► Weekly Notes/<YYYY-Www>.md              (Read+Write+Edit)
                              ├─► People/<Name>.md, ...                   (existing)
                              └─► record_fact / retract_fact / record_task  (MCP, unchanged)

Tasks.md edit (UI complete_task or Obsidian)
            │
            ▼
core/watcher.rs ──► core/indexer.rs reconciliation
                              │
                              ├─► task table (status=done) — existing
                              ├─► append "- <title>" to Daily Notes/<today>.md ## Did  ─── NEW
                              └─► record IndexerAppend audit-log entry              ─── NEW
                                          │
                                          ├─► fire daily-note-appended Tauri event
                                          │     └─► UndoToast.tsx (10s, existing)
                                          └─► audit-log panel renders the entry
                                                with per-event Revert
```

---

## Milestones

### M0 — ADR + plan

- Write ADR-0010 and this plan. *(done in this session)*
- Update CONTEXT.md with **Daily note**, **Weekly note**, and the
  event-vs-Fact flagged-ambiguity-resolved entry. *(done in this session)*
- **Verify:** files exist; `git diff` is clean of unrelated changes;
  `cargo check` clean on HEAD.

### M1 — Daily notes: prompt + template scaffold (no Rust)

- Add a new section to `prompts/conversation-agent.md` — **"Logging the
  user's day"** — covering:
  - The `Daily Notes/<YYYY-MM-DD>.md` convention.
  - The three sections (`## Checklist`, `## Did`, `## Notes`) and what each
    one is for.
  - Creating today's daily note on the first turn each day by reading
    `Templates/Daily.md` and writing the seeded `## Checklist`.
  - Event-shape rule (decision 6): events go to `## Did` as observation
    bullets with `[[Name]]` links; do *not* record events as graph Facts;
    do not mirror to entity notes.
  - Record-then-ask discipline (decision 10) for events missing a key
    identifier; sub-bullet enrichment on the next turn.
  - Strict-local-calendar "today" + back-dating via relative-date references
    (decision 9).
- Land a default `Templates/Daily.md` template at the formation root,
  created by the agent on first encounter if missing. (The agent prompt
  handles this — if `Templates/Daily.md` does not exist, the agent creates a
  minimal seeded version and tells the user where to edit it.)
- **Verify:** a `#[ignore]`d live test drives one real Claude Code turn that
  creates today's daily note from the template and appends an event under
  `## Did`. Manual `npm run tauri dev`: confirm the daily note appears in
  the file tree and is editable in CodeMirror without surprises.

### M2 — Weekly notes: prompt extension

- Extend the same prompt section to cover `Weekly Notes/<YYYY-Www>.md`. Same
  three sections, weekly cadence. Created on the first turn of any new ISO
  week from `Templates/Weekly.md`.
- Note that weekly `## Checklist` boxes flip in place and do *not* mirror to
  any daily-note `## Did` (symmetric with the in-note `## Checklist` rule).
- **Verify:** `#[ignore]`d live test extends M1's to cross an ISO-week
  boundary; manual `npm run tauri dev` for cross-week behaviour.

### M3 — Indexer write role: `Tasks.md` check-off → daily-note `## Did`

- Extend `core/indexer.rs` so when its `Tasks.md` reconciliation sees a task
  transition `open → done`, it:
  1. Reads today's `Daily Notes/<today>.md` (creating from
     `Templates/Daily.md` if missing — same logic the agent uses; share a
     helper).
  2. Appends `- <task.title>` to the `## Did` section. If `## Did` does not
     exist (a fresh template with only `## Checklist`), append the section
     header then the bullet.
  3. Records the append for the M4 undo layer.
- **Idempotence** is by `task_id` transition, not by file save. The
  reconciler tracks "last-known status" in the `task` table and only fires on
  the actual transition. The daily-note write itself does not feed back —
  the watcher event for the daily note is irrelevant to `Tasks.md`
  reconciliation.
- **Verify:** unit test against a temp formation — seed a task as `open`,
  flip the line to `[x]` in `Tasks.md`, run reconciliation, assert today's
  daily note exists and `## Did` contains the bullet exactly once. Re-run
  reconciliation, assert no duplicate. Edge case: today's daily note already
  has `## Did` with other bullets — append goes to the end of the section,
  not the end of the file.

### M4 — Undo for indexer-driven writes

- Extend the audit-log record format (introduced by ADR-0009 M4) with a
  `kind` field: `chat_turn` (existing) or `task_completion` (new).
- New audit-log entry shape for `task_completion`:
  ```json
  {
    "kind": "task_completion",
    "task_id": "task:call_dentist_d4e5f6",
    "daily_note_path": "Daily Notes/2026-05-22.md",
    "appended_bullet_text": "- Called the dentist",
    "appended_at": "2026-05-22T15:04:00Z"
  }
  ```
- M3's append step records one of these per `open → done` transition.
- Fire a `daily-note-appended` Tauri event after recording. `UndoToast.tsx`
  shows the 10-second quiet-undo affordance (modelled on the existing
  per-turn undo toast). Click Undo → delete the exact bullet from the daily
  note, mark the audit entry as reverted.
- **Refuse-on-edit:** if the appended bullet is no longer present verbatim
  in the daily note (the user added sub-bullet commentary or modified the
  text), revert refuses with a message: "this entry has been edited; please
  remove it manually."
- **Verify:** unit test — append a bullet, mutate the bullet text, attempt
  revert, assert refusal. Append a bullet, leave intact, revert, assert the
  bullet is gone from the daily note.

### M5 — Checklist auto-flip on natural mention (prompt only)

- Add a sub-section to the "Logging the user's day" prompt block:
  **"Matching the checklist"** (decision 7). Rules:
  - Before appending to `## Did`, read today's `## Checklist`.
  - If the user's mention is a clear, unambiguous reference to a checklist
    item, flip the box and acknowledge briefly. Do *not* also append to
    `## Did`.
  - On ambiguity, append to `## Did` and optionally ask a sharpening
    question per the discipline above.
  - The agent's scope on `## Checklist` is *only* flipping — never adding,
    removing, or reordering items. The template is the user's surface.
- **Verify:** `#[ignore]`d live test — pre-populate today's `## Checklist`
  with `- [ ] Take vitamins`; user message says "took my vitamins"; assert
  the box flips to `[x]`, no `## Did` entry was added, agent's reply
  acknowledges the flip. Then ambiguous case: pre-populate `- [ ] 30 min
  reading`; message says "did some reading"; assert no flip and `## Did`
  gains a bullet.

### M6 — Audit-log panel: render non-turn events

- The audit-log panel (introduced by ADR-0009 M5 as the replacement for the
  staging tray) renders `kind: "task_completion"` entries alongside
  `chat_turn` entries.
- Visual treatment is subtle — same panel, an icon distinguishing the kinds,
  same per-event Revert button.
- A `task_completion` entry's Revert button calls the same delete-bullet
  logic as M4's toast Undo, with the same refuse-on-edit behaviour.
- **Verify:** `npm run tauri dev` — check off a task, observe the toast,
  let it expire; open the audit-log panel and confirm the entry appears and
  Revert works.

### Open questions to resolve during the plan

These were flagged in ADR-0010 and are best decided when M1–M6 are concrete:

- **Carry-forward of unchecked items.** Default V1: *no carry-forward* —
  each day starts fresh from the template; yesterday's incomplete items sit
  in yesterday's note. Revisit if the user finds this annoying in dogfood
  use. Lands as a one-paragraph note in the M1 prompt, *not* as code.
- **Weekly note discoverability.** Assume `search_notes` is sufficient. Add
  a `find_weekly(date)` MCP tool only if the agent demonstrably struggles to
  find the right weekly note in dogfood use.

---

## Out of scope (deferred to future work)

- **Pattern recognition** that observes user behaviour and auto-suggests
  template edits ("you mention reading every Monday; add to weekly
  template?"). Belongs in the same "autonomous organise/connect pass"
  ADR-0009 deferred.
- **Recurring `task` table model** (cron-like recurrence on scheduled
  tasks). Daily/weekly habits intentionally bypass the task table — they're
  plain markdown checkboxes in daily/weekly notes.
- **End-of-day proactive nudge** from the agent.
- **Background midnight scheduler** for daily-note creation. V1 trigger is
  "first chat turn of the day."
- **Per-day-versioned templates.** Back-dated notes use the current
  template.

## Test strategy

The deterministic CI gate is M3's indexer reconciliation, M4's audit-log
record + revert logic, and the M4 refuse-on-edit path. All run as unit tests
against a temp formation, no agent in the loop.

The prompt-driven behaviours (M1, M2, M5) are verified by `#[ignore]`d live
tests driving real Claude Code turns (the M3 pattern from the
conversational-agent plan), plus manual `npm run tauri dev` for the
UX-shaped checks. This matches ADR-0006's Layer 2 convention and ADR-0009's
test strategy.
