# ADR-0010: Daily logs and recurring checklists

**Status:** Accepted (2026-05-22) — implemented the same day; cargo test 99/0, biome + npm build clean.
**Relates to:** ADR-0007 (tasks & reminders), ADR-0009 (conversational agent)
**Amends:** ADR-0009 §6 — extends the audit log's scope from per-chat-turn events to per-formation-modification events
**Plan:** [docs/plans/daily-logs-and-recurring.md](../plans/daily-logs-and-recurring.md)

## Context

A user wants three things Sediment does not currently model:

1. **Daily checklists** — recurring habits ("take vitamins", "30 min reading")
   that reset each day.
2. **Weekly checklists** — items ("review goals", "weekly planning") that
   refresh on a weekly cadence.
3. **A daily log** — a chronological record of what they did that day, captured
   in conversation: *"I had lunch with Keaton today and watched a youtube
   video"* should land as bullets the user can read back.

ADR-0007 modelled `Tasks.md` for one-off scheduled tasks and **explicitly
deferred recurrence**. ADR-0009 replaced the Write/Ask command bus with a
conversational agent that grounds itself, records, and questions — the right
substrate for daily journaling but with no convention for *where the
day-shaped record lives*.

The graph's bi-temporal Facts model stable entity→entity relationships
(`works_at`, `lives_in`). A one-time event ("had lunch with Keaton") is
*observation-shaped*, not relationship-shaped — the graph is the wrong tool.

## Decision

### 1. Daily notes and Weekly notes are Notes, not Entities

A **Daily note** is a Markdown file at `Daily Notes/<YYYY-MM-DD>.md`. A
**Weekly note** is at `Weekly Notes/<YYYY-Www>.md` (ISO week id). Neither has
an `entity_type` row — both are Notes that are not Entities, mirroring the
`Tasks.md` precedent already in CONTEXT.md. The agent learns the convention
from its behaviour prompt, not from a Rust scaffold.

The folder names are plural to match the existing `People/`, `Organizations/`,
`Projects/`, `Meetings/` convention in `prompts/conversation-agent.md`, and the
ISO 8601 filenames match Obsidian's daily-notes-plugin default — opening the
formation in Obsidian works without configuration.

### 2. Three sections, recommended-not-forced

| Section | Contents |
|---|---|
| `## Checklist` | Today's (or this week's) recurring items, seeded from a template. Plain Obsidian-Tasks checkboxes; **not** mirrored in the `task` table. |
| `## Did` | Events the user reports in conversation; `Tasks.md` check-offs appended by the indexer; `[[Name]]` wiki-links for backlinks. |
| `## Notes` | Reflections, observations. Short bullets — sub-bullets allowed for nested commentary. |

`## Checklist` is named differently from `Tasks.md`'s `## Tasks` region
deliberately — they are distinct concepts (recurring habits vs. scheduled
one-off tasks) and should not share a section name.

### 3. Templates are plain Markdown files in the formation

`Templates/Daily.md` and `Templates/Weekly.md` hold *only the `## Checklist`
content* — bullets like `- [ ] Take vitamins`. The other sections grow
organically. Templates are user-editable in Obsidian; the agent reads them
when creating a new daily/weekly note.

ADR-0009 §3 forbids hardcoded section templates **in Rust**. A user-owned file
in the formation is the user's artifact, not Rust scaffolding — explicitly
consistent.

**Templates are not retroactive.** Editing `Templates/Daily.md` at noon does
*not* alter today's `## Checklist`; the change takes effect from the next
daily note onward. Versioning templates per-day is more complexity than the
predictability gain is worth.

### 4. The agent creates daily and weekly notes on the first chat turn that needs them

On the first turn each day, the agent reads `Templates/Daily.md` and writes
`Daily Notes/<today>.md` with the seeded `## Checklist`. On the first turn of
each ISO week, it does the same for `Weekly Notes/<YYYY-Www>.md`. Both go
through Claude Code's native Read+Write tools — covered by ADR-0009 §6's
per-turn snapshot/audit/undo machinery for free.

The trade-off: on a pure-Obsidian day (no chat turn), no daily note is
created. The next chat turn does not retroactively create yesterday's note;
the user can say *"yesterday I…"* to back-date (see decision 9). A background
midnight scheduler is out of scope for V1.

### 5. `Tasks.md` check-offs auto-append to today's `## Did` (indexer-driven)

A box flipping `open → done` in `Tasks.md` — whether via Sediment's Reminders
popover (`complete_task`) or an Obsidian edit — triggers the indexer
(`core/indexer.rs`) to append a bullet to today's `Daily Notes/<today>.md`
`## Did` section.

Single code site. Idempotent on the *transition* (not the file save), so
writing the daily note does not re-trigger itself. No retroactive removal on
later uncheck — the call really happened; the user can delete the bullet by
hand.

This is the symmetric companion to decision 7: items inside a note's own
`## Checklist` flip in place (no `## Did` mirror), but `Tasks.md` items —
which have their own canonical home — propagate when completed.

### 6. Daily-note events are observation bullets, never graph Facts

*"Had lunch with Keaton"* is point-in-time and observation-shaped — closer to
*"said the Q3 roadmap feels overcommitted"* than to *"works at Cloudflare."*
The graph models validity intervals; lunches don't have one. So:

- The event lives in `Daily Notes/<today>.md` `## Did` only.
- The bullet uses `[[Keaton]]` — Obsidian's backlinks panel handles the
  cross-reference automatically; no mirroring to `People/Keaton.md`.
- Relationship-Facts that *surface during* the event ("she just joined
  Stripe") still go to `People/Keaton.md` and the graph via `record_fact`,
  per existing prompt rules. They may also appear as sub-bullets under the
  parent event in the daily note since that's where they were learned.

### 7. The agent flips `## Checklist` boxes on natural mention; never adds/removes/reorders

When the user says *"took my vitamins this morning"* and today's `## Checklist`
contains `- [ ] Take vitamins`, the agent flips the box and briefly
acknowledges in its reply. The flip *is* the completion record — no `## Did`
mirror.

Matching is by semantic judgment, **conservative on ambiguity.** A clear match
flips; an ambiguous mention ("did some reading" vs `- [ ] 30 min reading`)
appends to `## Did` instead and may trigger a sharpening question.

The agent's scope on `## Checklist` is *only* flipping. It does not add new
items, remove items, or reorder. The user's template is the user's surface.

### 8. Undo for indexer-driven writes: bullet-text records + two layers

The indexer's write to the daily note happens outside `chat_turn` — ADR-0009
§6's per-turn snapshot/audit/undo machinery does not cover it. To bring these
into the same model without per-event full-formation snapshots:

- **Storage shape:** record `{task_id, daily_note_path,
  appended_bullet_text, appended_at}` per append. The bullet text is the
  reverse operation. No file snapshot.
- **Layer 1 — quiet undo toast.** A `daily-note-appended` Tauri event fires
  the existing `UndoToast.tsx` for 10 seconds. Modelled on ADR-0009 §6's
  quiet-undo idiom.
- **Layer 2 — audit-log backstop.** Each indexer append becomes a synthetic
  audit-log entry alongside chat-turn entries. The audit-log panel renders
  both and offers per-event revert.

**Edge case:** if the appended bullet has been edited since logging (e.g. the
user added sub-bullet commentary), revert refuses with explanation rather
than destroying the edits.

This **extends ADR-0009 §6**: the audit log's scope grows from "per-chat-turn
events" to "per-formation-modification events" — chat-turn writes and
indexer-driven writes alike are visible and revertable.

### 9. "Today" is strict local calendar; back-dating is supported

*"Today"* means the user's current local date at turn time, flipping at
midnight. No felt-day cutoff.

When the user references a different date — *"yesterday"*, *"last Tuesday"*,
*"2026-05-19"* — the agent parses the relative reference and files to that
date's `Daily Notes/<date>.md`. If that file doesn't exist, the agent creates
it from `Templates/Daily.md` *as the template stands now* (templates are not
versioned per-day; the back-dated `## Checklist` may not reflect the actual
habits of that past day — acceptable, the user can ignore it).

### 10. Clarifying-question discipline: record-then-ask

When the user reports an event with a missing key identifier ("watched a
youtube video" — no title), the agent **records first** (the bullet lands in
`## Did` this turn) and asks one focused, light-touch question in its reply
("what video?"). On the next turn, the user's answer becomes a **sub-bullet**
under the parent — preserving the conversation flow.

Caps: at most one clarifying question per turn; if the user moves on, the
agent does not re-ask. Ask only when a *key identifier* is missing — not to
fish for richer detail when a bullet is already complete.

## Consequences

- **Positive — fits the existing agent substrate.** Most of the work is a
  behaviour-prompt extension and one indexer responsibility. No new Rust
  models for daily/weekly notes; ADR-0009's snapshot/audit/undo machinery
  covers agent writes.
- **Positive — Obsidian compatibility is free.** `Daily Notes/`, `Weekly
  Notes/`, ISO filenames, plain checkbox markdown, `[[Name]]` backlinks — the
  formation stays a first-class Obsidian vault.
- **Positive — symmetric rules.** Tasks.md → `## Did` propagates; in-note
  `## Checklist` items flip in place. Daily and weekly notes share shape.
  Events and graph Facts have a clean boundary.
- **Negative — silent-day gap.** A day with no chat turns produces no daily
  note. Tasks checked off in Obsidian that day still flip in `Tasks.md` but
  have nowhere to append, so the indexer simply skips the daily-note write
  for that day (it is not an error). The next chat turn does not retroactively
  create yesterday's note.
- **Negative — indexer gains a write role.** It was previously read-only
  (parses external edits, reconciles `task` table). Decision 5 makes it
  write to today's daily note. Care required around watcher feedback loops —
  mitigated by the transition-not-save trigger (decision 5) and idempotence
  on `task_id`.
- **Neutral — audit-log scope widening.** Decision 8 extends ADR-0009 §6's
  audit-log from per-turn to per-modification. The on-disk record format
  needs a kind field (`chat_turn` vs `task_completion`).
- **Out of scope** —
  - **Pattern recognition / auto-template-suggestion.** The user observed
    that the agent could eventually notice they always do X on Mondays and
    suggest adding it to the weekly template. This belongs in the same
    "autonomous organise/connect pass" ADR-0009 already deferred.
  - **Recurring `task` table model.** Daily/weekly habits intentionally do
    *not* live in the task table — they're plain markdown checkboxes inside
    daily/weekly notes. A future recurrence field on the Task model is still
    possible for scheduled-task recurrence ("every Monday at 9am: standup"),
    but that's a separate problem.
  - **End-of-day proactive nudge** ("looks like it's been a busy day — want
    to recap?"). The agent only acts on user turns in V1.
  - **Background midnight scheduler** for daily-note creation. Decision 4's
    "agent creates on first chat turn" is the V1 trigger.
  - **Per-day-versioned templates.** Decision 3 / 9: templates are not
    retroactive; back-dated notes use the current template.

## Open questions

1. **Carry-forward of incomplete checklist items.** Should items left
   unchecked at end-of-day roll forward to tomorrow's `## Checklist`? Or do
   they sit in yesterday's note forever (and tomorrow starts fresh from the
   template)? The conservative answer is *no carry-forward* — each day is its
   own snapshot — but a user may want streaks. Resolve in the plan.

2. **Weekly note discoverability.** Daily notes have a natural anchor
   ("today"); a Weekly note for `2026-W21` is less obvious. Does the agent
   need a `find_weekly(date)` helper, or is `search_notes` enough? Probably
   the latter; flag during M4 of the plan.
