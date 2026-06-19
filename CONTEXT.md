# Sediment

A desktop note-taking app where the primary input is conversation. The user
talks to an AI agent; the agent records what it learns into an organized,
Obsidian-compatible body of notes, and questions the user to sharpen their
thinking.

## Language

**Formation**:
The user's folder of Markdown notes — the user-facing body of knowledge.
_Avoid_: vault, knowledge base, workspace.

**Note**:
A single Markdown file in the formation, organized as titled sections of bullets.
_Avoid_: document, page, file.

**Section**:
A titled group of bullets within a note — e.g. a person's "Work" or "Personal".
Notes are structured as sections, not prose.
_Avoid_: heading, region, block.

**Entity**:
A person, organization, project, meeting, or topic — a node in the knowledge graph.
_Avoid_: object, item, record.

**Self**:
The user themselves, modeled as a first-class **Entity** (a person) with a **Note**
(`Self.md`) whose **Sections** hold their stable attributes — preferences, working
style, goals, ongoing threads — and **Facts** for their genuine relationships. The
**Agent**'s durable, *authored* model of who the user is: the counterpart to the
*derived* **Working Set** (which knows what the user recently *touched*, not who they
*are*). Not a new store — the existing Entity/Note/Fact model aimed at the user.
_Avoid_: peer card, profile, user model, memory.

**Fact**:
A relationship between two entities, carrying a bi-temporal validity window — an
edge in the knowledge graph. Only relationship-shaped bullets are Facts; an
attribute or observation bullet is note content only.
_Avoid_: triple, edge, relation (in domain conversation).

**Agent**:
The conversational AI the user talks to. It records, questions, and connects —
it is the product's primary surface.
_Avoid_: assistant, bot, model.

**Engine** (or **Conversation engine**):
The runtime that runs the **Agent**'s loop each turn — an agentic CLI driven as the
user's *own* installed, *own*-authenticated, subscription-backed binary (Claude
Code or GitHub Copilot), chosen in Settings. The **Agent** (persona, questioning
discipline, tools) is the same whoever runs it; the **Engine** is swappable behind
one trait. Sediment never reuses a vendor's OAuth token, offers a vendor login, or
routes more than one user through a subscription — it only drives the user's own
binary.
_Avoid_: model, backend, provider.

**Working Set**:
What the **Agent** currently has in mind — the **Entities**, tasks, and threads in
play right now, and the **Notes** most related to the moment. It is *derived*, not
authored: recomputed each turn from recent activity and pushed to the **Agent**
before it acts, so the **Agent** never starts a turn cold. The **Agent** reads the
**Working Set**; it never writes it. Not a **Note** and not authoritative — a view,
the way the knowledge graph is a peer store.
_Avoid_: memory (that is the graph store), context, dashboard, state of mind.

**Open Loop**:
An unresolved question or a stated-but-unfulfilled intention the **Agent** notices
in conversation — "you said you'd pick a vendor", "you asked about the lease and
never got an answer". Captured as a lightweight record the moment it arises (so it
can be surfaced later without re-deriving it) and raised by the **Agent** in its
reply — sparingly — until the user resolves or dismisses it. Distinct from a
**Task**: a **Task** is a scheduled action with a due time and a notification; an
**Open Loop** is a soft, **Agent**-noticed thread that is nudged in conversation,
never alarmed.
_Avoid_: todo, reminder (those are **Tasks**), thread, dangling fact.

**Daily note**:
A **Note** (not an **Entity**) at `Daily Notes/<YYYY-MM-DD>.md` capturing one
calendar day's recurring checklist, events, and reflections. Three sections:
`## Checklist` (today's habits, seeded from `Templates/Daily.md`), `## Did`
(events and task completions, with `[[Name]]` links for backlinks), `## Notes`
(reflections). Created by the **Agent** on the first chat turn each day.
_Avoid_: journal entry, daybook, log.

**Weekly note**:
A **Note** (not an **Entity**) at `Weekly Notes/<YYYY-Www>.md` (ISO week id)
with the same shape as a **Daily note**, at weekly cadence. Seeded from
`Templates/Weekly.md`.
_Avoid_: weekly journal, week summary.

**Session**:
One user-initiated, bounded stretch of live capture — the **Agent** listening to a
meeting or a quick voice memo. Explicitly started and stopped by the user, visible
while it runs, and torn down on stop: the opposite of a background daemon (ADR-0011
declined the daemon; a **Session** is never unbidden and never always-on). Produces
a **Meeting note** and collapses into the single conversation when it ends —
transient capture chrome, not a second surface.
_Avoid_: recording, daemon, listener, background job.

**Meeting note**:
The **Note** of a `meeting` **Entity**, at `Meetings/<YYYY-MM-DD HHmm> — <title>.md`,
capturing one **Session**: `## Attendees`, `## Notes` (the user's typed notes and
live chat, time-anchored), `## Transcript` (speaker-labelled **Transcript segments**),
`## Action items`, `## Decisions`. Event-shaped like a **Daily note** — the transcript
itself is not **Facts**; **Facts** are what the **Agent** distils from it afterward.
_Avoid_: recording, minutes, call log.

**Transcript segment**:
One speaker-attributed, timestamped span of transcribed speech within a **Session** —
text plus a start/end audio offset from **Session** start and the speaker it is
attributed to. The unit of the **Meeting note**'s `## Transcript`, and the spine that
time-aligns the user's notes to what was being said.
_Avoid_: utterance, line, caption.

**Voiceprint**:
A speaker-embedding vector stored on a **person** **Entity** (beside the embedding it
already carries) that lets a future **Session** recognise that person's voice.
Enrolled progressively and lazily — the **Self**'s voice once, others when the user
names an unknown speaker — never via a wizard. Not a new store: the existing graph
holds it, the way it holds a **Fact**.
_Avoid_: voice signature, biometric, speaker model.

**Voice clip**:
A short WAV (a few seconds) of a **person**'s clearest speech, stored at
`People/.voices/<Name>.wav` and pointed to from their **Entity** beside the
**Voiceprint** — so a person can be recognised by *ear* after the fact, not only by
the matching vector. Captured when a speaker is named (live or in the second pass);
the full meeting audio is still transcribe-and-delete (ADR-0017 §9).
_Avoid_: recording, sample dump, audio note.

**Second pass** (also **refined transcript**):
The end-of-**Session** re-transcription: the live streaming transcript is replaced by
a high-accuracy *offline* recognizer run once over the whole buffered meeting, with
speakers re-diarized (seeding from named live speakers so they keep their names). The
`## Transcript` the **Agent** distils from is this refined one, not the rough live one.
_Avoid_: rescan, reprocess, post-processing.

## Relationships

- A **Formation** contains many **Notes**
- A **Note** is organized into **Sections**; a **Section** is a list of bullets
- Most **Entities** have a **Note**; some **Notes** are not **Entities** (e.g. `Tasks.md`, **Daily notes**, **Weekly notes**)
- A **Fact** connects two **Entities**
- The **Self** is the user's own **Entity**; its **Note** (`Self.md`) and **Facts**
  are the **Agent**'s durable, *authored* model of the user, injected into every turn
- The **Agent** reads the **Formation** and writes **Notes** and **Facts**
- The **Agent** reads the **Working Set** each turn; it is *derived* from recent activity across the **Formation**, the graph, and the conversation — never authored
- A **Daily note** captures one calendar day; a **Weekly note** captures one ISO week
- A **Session** produces a **Meeting note** and a stream of **Transcript segments**; the **Agent** distils **Facts**, **Tasks**, and **Open Loops** from the **Meeting note** when the **Session** ends
- A **Voiceprint** is attached to a **person** **Entity** and names the speaker of a **Transcript segment**; a **Voice clip** is its audible companion on the same **Entity**
- On **Session** end the **Second pass** rewrites the **Meeting note**'s `## Transcript` before the **Agent** distils it

## Example dialogue

> **Dev:** "When the user mentions a new person, does the **Agent** always create a **Note** for them?"
> **Domain expert:** "It creates an **Entity** for them in the graph. A **Note** follows once there's enough to say — an **Entity** can exist before its **Note** does."

## Flagged ambiguities

- Agent-authored prose vs. structured bullets — resolved: notes are structured
  **Sections** of bullets, not prose paragraphs. People keep notes about an
  entity as organized bullets, not sentences.
- "Fact" used loosely to mean any bullet — resolved: a **Fact** is specifically a
  graph-tracked entity→entity relationship (the subset that powers contradiction
  detection). Attribute and observation bullets are note content only.
- "Correcting a fact" conflated two operations — resolved: a **Fact** that
  *changed over time* is **superseded** (the old edge keeps its history); a
  **Fact** that was *wrong* is **retracted** (the edge is deleted — it was never
  true). The agent picks based on how the user phrases the correction.
- Event vs. **Fact** — resolved: a one-time event ("had lunch with Keaton",
  "watched a video") is an observation bullet in the **Daily note**'s `## Did`
  only — never a graph **Fact**. Relationship-Facts that surface during an event
  (e.g. "she just joined Stripe") still go to the entity's **Note** and the
  graph as before. The **Daily note** is the sole home for the event itself;
  `[[Name]]` links create the cross-reference via Obsidian backlinks.
- "Memory" overloaded — resolved: the **Formation** is long-term memory (authored,
  on disk); the knowledge graph is the structured peer store (`core/memory.rs`);
  the **Working Set** is the *short-term* working memory the **Agent** holds for the
  current moment (derived, never stored). Use the specific term; avoid bare "memory"
  in domain conversation.
- **Self** vs **Daily note** — resolved: durable facts and traits about the user
  (stable preferences, working style, standing goals, the user's own
  relationship-**Facts**) live in `Self.md` and the graph; one-time events and
  transient states about the user stay in the **Daily note** `## Did` only. The same
  **Event**-vs-**Fact** line, applied to the user. Threshold for the **Self**: "would
  this still be true, and worth knowing, next month?" The **Self** is the *authored*
  durable model; the **Working Set** is the *derived* recency view — they complement,
  never compete.
- **Open Loop** vs **Task** — resolved: a **Task** is a schedulable action the user
  wants done and alerted (ADR-0007 — `Tasks.md` + the `task` table + notification);
  an **Open Loop** is an unresolved question or decision the **Agent** noticed and
  watches, surfaced only in conversation and never via notification. The **Agent**
  picks by whether the thing is a scheduled action (**Task**) or a pending
  resolution (**Open Loop**).
- **Session** vs daemon (ADR-0017) — resolved: a **Session** is user-initiated,
  bounded, and visible while it runs; it is never the always-on background listener
  ADR-0011 declined. Capture is a deliberate act with a start and a stop, not a
  process that watches unbidden.
- **Meeting note** vs **Daily note** (ADR-0017) — resolved: both are event-shaped
  **Notes** whose transcript/events are not **Facts**. A **Meeting note** holds one
  **Session**'s transcript and the notes taken alongside it; a **Daily note** holds
  one calendar day's events and reflections. The **Agent** distils durable **Facts**
  from either into the entity **Notes** and the graph as usual.
- **Voiceprint** vs **embedding** (ADR-0017) — resolved: both are vectors on an
  **Entity**, but a **Voiceprint** identifies a **person**'s *voice* (for speaker
  recognition in a **Session**), while the existing embedding indexes a **Note**'s
  *text* (for semantic search). Same primitive, different signal; neither is a new
  store.
