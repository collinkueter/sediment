# ADR-0015: The Self note — a durable, authored model of the user

**Status:** Proposed (2026-06-17) — designed through a structured grilling session;
domain term captured in [CONTEXT.md](../../CONTEXT.md) (**Self**), along with the
**Self**-vs-**Daily note** boundary.
**Amends:** [ADR-0011](0011-working-set-and-push-grounding.md) — answers the gap it
named ("No durable model of the user", §Context) with an *authored* model, picking
up where the **Working Set**'s honest limit (§3) leaves off; extends §2's
push-grounding with an always-injected, highest-priority **Self** slot above the
derived Working Set.
**Relates to:** [ADR-0010](0010-daily-logs-and-recurring.md) (the **Self** holds
durable user facts; the **Daily note** holds one-time events — the **Event**-vs-**Fact**
line applied to the user), [ADR-0004](0004-bitemporal-contradiction-detection.md)
(supersede/retract on the user's own Facts), [ADR-0009](0009-conversational-agent.md)
(single conversation, no new surface).
**Keeps:** ADR-0011's no-daemon / no-second-surface stance; the Working Set unchanged;
the snapshot/audit/undo machinery; the formation and graph stores.

## Context

ADR-0011 named four gaps; one was **"No durable model of the user — each turn
re-derives the user from a cold start."** It answered this with the **Working Set**,
calling it *"the durable model of the user that cures the amnesia."* But the Working
Set is **deterministically derived from recency** (file `mtime`, recent mentions,
open tasks/loops) and admits its own **honest limit** (ADR-0011 §3): it knows *"what
the user touched, not what matters."* The things that make an assistant feel like it
knows you — your preferences, your working style, your standing goals — are *semantic
and stable*, invisible to recency.

The product goal is an assistant that gets to know you. Today Sediment models
**everyone except the user**: it builds rich notes and Facts about the people,
orgs, and projects you mention, but holds no first-class model of *you*.

The trigger was research into **Hermes Agent** (Nous Research) and its **Honcho**
memory provider: a synthesized **"peer card"** / user representation injected into
every turn, plus a background **"dialectic"** reflection that derives patterns about
the user across sessions. Two parts of Hermes's strong version conflict with Sediment
commitments, and this ADR keeps only what fits:

- Its eight memory providers are mostly **cloud services** — declined; Sediment is
  local-only.
- Its dialectic runs as a **background / session-end synthesis** — declined; ADR-0011
  placed *"the background daemon, the new-day/app-open catch-up turn"* explicitly out
  of scope, *"in favour of the in-reply rider."*

## Decision

### 1. Model the user as the **Self** — existing concepts, aimed inward

The user is the **Self**: a reserved `entity:self` **Entity** (a person) with a
**Note** `Self.md` whose **Sections** hold stable attributes, and **Facts** for the
user's genuine relationships (`Self works_at …`). This is **not a new store or
concept** — it is the Entity/Note/Fact model pointed at the user. "Peer card" is
Hermes's word; ours is **Self** (CONTEXT.md).

### 2. Author it incrementally, in-turn — no reflection pass

The agent records durable truths about the user **during normal turns**, exactly as
it records a Fact about any entity. We **decline Hermes's dialectic/synthesis pass**:
deriving patterns the user never stated would require a background or session-end
job, which ADR-0011 ruled out in favour of the in-reply rider. The Self captures what
the user *states*, not patterns inferred behind their back. (Whether a bounded
*in-reply* form of synthesis is worth adding later is Open question 1 — it must arrive
as an in-reply move, never a daemon.)

### 3. Always inject the Self, at top priority

`Self.md` carries a `## Summary` region — a handful of lines (core preferences,
current goals, working style) the agent keeps current as part of normal authoring.
The deterministic pre-pass (ADR-0011 §2) **injects that section verbatim** as the
**highest-priority** grounding slot, **above** the Working Set, ranked so it is
**never** the section truncated under `INJECTED_CONTEXT_BUDGET` (ADR-0011 open Q3).
The full `Self.md` is **not** pushed every turn; the agent pulls detail on demand
with its own file tools (Hermes's L0→L2 tiering, done with existing tools — no new
retrieval). Self = *who you are* (authored, stable, always in); the Working Set
stays *what you're touching* (derived, churny). They complement, never compete.

### 4. Durable subset only — the recording discipline

The **Self** holds the *durable* subset: stable preferences ("hates morning
meetings"), working style ("thinks by writing"), standing goals ("ship V1 by
August"), and the user's own relationship-**Facts**. One-time events and transient
states ("had lunch with Keaton", "tired today") stay in the **Daily note `## Did`**
(ADR-0010) — never the Self. The threshold is **"would this still be true, and worth
knowing, next month?"** Changes to a Self fact use the **supersede / retract**
discipline (ADR-0004 and the `record_fact` contradiction interlock): "used to hate
mornings, now doesn't" → supersede.

### 5. Lazy creation, no new surface

The Self is created the first time there is something durable to record — the same
rule as every other entity (CONTEXT.md: *"an Entity can exist before its Note
does"*). **No onboarding wizard.** One gentle first-session conversational nudge
("anything I should know about how you like to work?") is permitted but is *not* a
gate. `Self.md` is an ordinary **Note**: viewable in the note viewer, editable in
Obsidian, revertable through the audit log, and optionally surfaced in the existing
Working Set panel. No second surface (ADR-0009/0011).

### 6. No memory-provider trait (YAGNI)

One local provider: the existing `MemoryStore` (SurrealDB graph + on-device
embeddings) — Sediment's own local equivalent of Hermes's local "Holographic"
provider. We **decline a pluggable `MemoryProvider` trait**: there is exactly one
implementation and the alternatives are the cloud providers we ruled out. Keep the
Self-grounding logic as a tidy module (e.g. `self_model::summary_for_grounding`);
introduce a trait only when a real *second local* provider exists to shape it — the
way `ConversationEngine` earned its seam from two real engines (ADR-0008/0012).

## Consequences

- **Positive** — the assistant gains a durable, *authored* model of the user (the
  "gets to know you" capability) inside the single conversation, with **no daemon and
  no new surface**. Reuses entities, Facts, notes, and the audit/undo machinery
  wholesale.
- **Positive** — identity is never crowded out by recency; the always-on Self slot is
  small and fixed-cost, and ranked first under the §2 budget.
- **Positive** — fully **local and transparent**: the Self is plain Markdown,
  user-editable and revertable (Hermes's editable peer card, but on-device). The
  landing page's "never leaves your machine" promise stays literally true.
- **Negative** — per-turn prompt grows by a small fixed Self block (bounded; competes
  within ADR-0011's budget, but ranked above everything).
- **Negative** — recording quality is **prompt-driven**: what lands in the Self vs the
  Daily note depends on the agent honouring the durable/ephemeral threshold (tunable,
  not guaranteed). The `## Summary` may go mildly stale between self-relevant turns
  (accepted; it self-corrects on the next one).
- **Negative** — declining reflection means the Self captures what the user *states*,
  not patterns it was never told. The deepest "it really gets me" synthesis is out —
  **by choice**, to stay within ADR-0011's no-daemon grain.
- **Out of scope** — the reflection / dialectic synthesis pass in any background or
  session-end form; the pluggable provider trait and all cloud memory providers
  (local-only); a dedicated Self UI surface.

## Open questions

1. **Reflection-as-in-reply-rider.** Is there a bounded *in-reply* form of synthesis —
   at most one derived observation about the user per turn, disciplined exactly like
   the one-Open-Loop-per-turn rider (ADR-0011 §4) — that adds the "patterns you didn't
   state" value without a daemon? Deferred; first measure whether in-turn authoring
   alone already feels like it knows you.
2. **Summary maintenance.** Should `## Summary` be agent-authored prose, or
   deterministically rendered from `Self.md`'s sections? Start agent-authored; revisit
   if it drifts or bloats.
3. **Budget interaction.** Exact ranking and cap of the Self slot against ADR-0011
   open Q3 (Working Set + related notes under `INJECTED_CONTEXT_BUDGET`). Pick a small
   fixed cap; tune empirically.
