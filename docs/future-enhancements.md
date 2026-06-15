# Sediment — Future enhancements

A registry of deferred ideas that are not yet scheduled into an ADR or a plan.
Lightweight by design: an entry here is a *maybe*, captured so it is not lost.
Promote an item to an ADR (if it is a real, hard-to-reverse decision) or a plan
(when it is picked up for build) at that point.

---

## Configurable assistant tone ("banter level")

**What.** Let the user dial the **Agent**'s conversational personality along a
spectrum — from factual / stoic, through warm (today's default), to sassy /
zingy — as a *setting*, not a code change.

**Why.** Tone is taste. Some users want a terse, just-the-facts note-keeper;
others want a thinking partner with a bit of edge that makes the daily
back-and-forth enjoyable. A single fixed *"warm, concise, direct"* persona (the
current `prompts/conversation-agent.md` §Tone) serves the middle but fully
pleases neither end of that range.

**Sketch.**
- A **tone setting** in Settings — presets (e.g. *Stoic · Warm · Sassy*) or a
  slider — persisted in `AppConfig` and rendered into the behaviour prompt's
  persona section at turn time. The behaviour prompt stays the single versioned
  artifact (ADR-0009 §8); tone becomes a *parameter* of it, not a fork of it.
- At the **sassy** end, the Agent may add a light, occasional zinger. The
  motivating example: when you tell it something it already knows, a knowing
  callback — *"You mentioned this last Tuesday — good thing you've got me."* The
  system already *detects* the repeat (entity resolution + `find_contradiction` +
  the Working Set, ADR-0011 / ADR-0012); sassy mode just surfaces that detection
  with personality instead of silently de-duplicating.

**Guardrails.**
- Tone affects **reply wording only** — never *what* gets recorded, grounded, or
  filed. A zinger must never trade away accuracy or the recording discipline.
- Banter rides the **in-reply rider discipline** (ADR-0011 §4): occasional, never
  nagging, one beat at most, and it always yields to the real answer or question.
  Snark is the seasoning, not the meal.
- The default stays **neutral / warm**; sass is opt-in. Humour lands unevenly;
  never impose it.
- Per-user, and ideally tunable mid-conversation (*"dial it down"*).

**Where it would live.** `prompts/conversation-agent.md` (a templated persona
block keyed by the tone setting) + a Settings control + an `AppConfig` field. No
new architecture — a personalization layer over the existing Agent.
