# ADR-0009: Chat as a conversational agent

**Status:** Accepted (2026-05-22; decisions refined through a structured design
review the same day, then accepted for implementation — see the
[plan](../plans/conversational-agent.md) and [CONTEXT.md](../../CONTEXT.md)).
**Supersedes:** ADR-0003 (extraction pipeline), ADR-0006 (LLM-backed extraction),
ADR-0008 (Claude Code answer engine)
**Amended by:** ADR-0011 (push-grounded turns, the Working Set, in-reply
proactivity); ADR-0012 (GitHub Copilot engine — retires the Gemini CLI engine on
ToS grounds)
**Amends:** ADR-0005 (staging and commit); `sediment-tech-spec.md` v0.2 §2–§8,
§13 — including design principle #1 (local-first) and principle #2 (formation
authoritative)
**Keeps:** ADR-0004 (bi-temporal contradiction detection), ADR-0007 (tasks &
reminders), ADR-0001 (Tauri/Rust stack), ADR-0002 (CodeMirror editor)
**Plan:** [docs/plans/conversational-agent.md](../plans/conversational-agent.md)

## Context

Sediment's chat today is a **command bus**, not a conversation. Every message is
classified Write or Ask (`core/intent.rs`) and run through a fixed pipeline:

- **Write** (`chat_write`) → extract a structured `Extraction` → route facts to
  notes → render diffs → park a `StagingEntry`. The assistant's only reply is a
  receipt: *"Staged 3 facts across 2 notes."* It never asks a question.
- **Ask** (`chat_ask`) → retrieve → cited answer. The one path where it talks.

What the product is meant to be — stated by the user:

> A user has a conversation with the AI agent, and it constantly stores away the
> information being collected in an organized fashion — *and* questions the user
> when it needs more detail, when there are contradictions, or when there are
> related notes. The chat has access to all previous notes, and helps the user
> build more well-formed foundations from new incoming thoughts.

A fixed DAG cannot decide, turn by turn, whether to record, ask, retrieve, or
challenge. That needs an **agent loop** with tools, where every turn produces a
real conversational reply.

### Decisions taken with the user (2026-05-22)

Four framing decisions, then twelve refinements from a design review:

1. **Engine** — support both BYOK model APIs and local agentic CLIs. The agent
   is always a capable model; a small local chat model is not a goal.
2. **Modes** — collapse Write/Ask into one conversation.
3. **Recording** — the conversation *is* the review; staging shrinks to an audit
   log; changes apply with a quiet undo.
4. **Rely on AI** — lean on a capable agent rather than keep deterministic/local
   components first-class; this retires GLiNER and the hardware-tier strategy.

The design review then resolved the consequences (§ numbers below):
notes stay structured (§3), the graph becomes a peer store (§4), the engine is
the Claude Code CLI for V1 (§5), and recording/review mechanics (§6).

## Decision

### 1. An agent loop is the spine; the pipeline becomes tools

The conversational **agent** runs on every turn. Each turn:

1. The user message is persisted to `chat_message`.
2. The agent runs with a recent window of conversation and a tool surface over
   the formation. It grounds itself (search, read), and records what it learns.
3. The agent produces a streamed conversational reply — acknowledging, asking
   clarifying questions, flagging contradictions, pointing at related notes.

Entity extraction, fact routing, diff generation, and contradiction detection
stop being pipeline stages and become **tools the agent calls** (§5) or
behaviour the agent simply performs.

### 2. One conversation, no modes

The Write/Ask split, the mode toggle, and `core/intent.rs` are removed. The
agent records and answers in the same turn. There is no `classify_intent`.

### 3. The note model: structured sections of bullets

A **note** is a Markdown file organised as titled **sections** of bullets — not
prose. People keep notes about a person as a contact-card of organised bullets,
not as sentences. The agent authors and maintains this structure.

Section structure is **agent-driven**, steered by a recommended section
vocabulary in the agent's behaviour prompt (§8) — no section templates are
hardcoded in Rust. This keeps notes consistent without rigid scaffolds.

A note's bullets are **two tiers**, and the distinction is invisible to the user:

- Every bullet is **note content** — written by the agent into a section.
- A bullet that is a genuine entity→entity relationship (`works_at`,
  `reports_to`, `lives_in`, `member_of`, …) is *also* recorded as a graph
  **Fact** (§4). Attribute bullets ("has a dry sense of humour") and observation
  bullets ("said the Q3 roadmap feels overcommitted") are note content only.

Contradiction detection therefore operates on relationship-Facts — exactly where
contradictions are detectable and worth catching — without graph noise from
every attribute bullet.

### 4. The knowledge graph is a peer store, not derived state

Tech-spec principle #2 said all derived state is reproducible from the
formation. With free-form sectioned bullets (§3) and Facts carrying data a
bullet does not encode (`valid_from`/`valid_to`, `source_chat_id`, confidence),
the graph can no longer be mechanically reconstructed from the notes.

**Principle #2 is amended.** The formation is authoritative for *note content*;
the bi-temporal knowledge graph is a **peer authoritative store** for entity,
relationship, and temporal data. Both are durable on disk (`.md` files;
SurrealKV). The graph is not "derived."

**External edits** (Obsidian, cloud-sync) are handled by what is mechanical vs.
what needs the agent:

- **Embeddings — eager.** Re-chunking and re-embedding a changed note is
  mechanical; the file watcher keeps doing it immediately, so retrieval never
  goes stale.
- **Graph Facts — lazy.** No eager graph re-sync per file save. The watcher
  marks a note "externally changed"; the agent reconciles the graph the next
  time that entity comes up in conversation (it already reads notes to ground
  itself). The graph may be transiently stale between an external edit and the
  next mention — acceptable, since contradiction handling happens in
  conversation, which is what triggers reconciliation.

Consequence: corruption recovery (tech-spec §10) loses its "rebuild the graph
from the formation" escape hatch. The graph relies on SurrealKV durability;
`chat_message` history is retained as provenance.

### 5. The engine: the Claude Code CLI (V1)

A `ConversationEngine` trait abstracts who runs the agent loop. It admits two
shapes: an **API engine** (Sediment owns the loop over HTTP) and a **CLI engine**
(an agentic CLI owns its own loop). **V1 implements one engine — the Claude Code
CLI.** The Gemini CLI is the second engine; API engines come later. *(ADR-0012 retires
the Gemini CLI engine on ToS grounds — Google prohibits third-party use of
Gemini-CLI OAuth — and makes GitHub Copilot the second engine.)* Shipping one
engine first proves the loop itself; breadth is mechanical afterward.

This reverses ADR-0008, which hardened Claude Code into a tool-less plain
answerer. The conversational engine instead spawns `claude` *as an agent*:

- **Working directory = the formation root.** Claude Code's **native file tools**
  (Read, Edit, Write, Grep, Glob) are enabled and scoped to the formation — they
  are how the agent reads and writes notes. Bash is not enabled.
- **A graph-only MCP server.** Sediment exposes a small stdio MCP server (~7
  tools) for the part of the tool surface that is *not* files — the bi-temporal
  graph and the embedding index. `--mcp-config` points at it;
  `--strict-mcp-config` keeps the user's other MCP servers out. One stdio server
  per turn for V1 (persistent-per-session is a later optimisation).
- **`--system-prompt`** = the behaviour prompt (§8); `--no-session-persistence`
  (Sediment owns the transcript — below); `stream-json` output, parsed as in
  `core/claude_code.rs` today, extended to surface `tool_use` events as
  conversational activity.

The **graph-only MCP tool surface** (`core/formation_tools.rs`, served by
`core/formation_mcp.rs`):

| Tool | Backed by |
|---|---|
| `search_notes(query, k)` | semantic search — `MemoryStore::search_chunks` |
| `find_entity(name)` | `lookup_entity` + `current_facts` |
| `related_facts(entity)` | graph traversal on `fact` edges |
| `find_contradiction(subject, predicate, object)` | `MemoryStore::find_conflicts` |
| `record_fact(subject, predicate, object, validity)` | a bi-temporal `fact` edge |
| `retract_fact(fact_id)` | `MemoryStore::delete_fact` (see §6) |
| `record_task(title, due?)` | a `Tasks.md` line + `task` row (ADR-0007) |

Note read/write is *not* in this list — Claude Code's native file tools do that
(Option B). The MCP server stays small, which keeps the V1 build tractable.

**Sediment owns the conversation transcript.** It lives in `chat_message`, not in
Claude Code session files — `source_chat_id` provenance must point at Sediment's
own data, and keeping the transcript engine-side keeps the trait clean for the
Gemini CLI later. Each turn spawns `claude` fresh and feeds a **recent window**
of conversation (≈ the last 10–20 turns); older context is pulled by the agent's
own tools (`search_notes`, `read_note`). The formation is the long-term memory;
the window is just conversational continuity.

### 6. Recording is conversational; review is an audit log

**Recording is per-fact, conditional on confidence** — not all-or-nothing per
turn. A fact nothing contradicts is recorded in the same turn the agent learns
it (the agent may still ask a sharpening question alongside). A fact that is
contradictory or ambiguous is **not** recorded — the agent asks, and records it
on the turn the user resolves it. No state machine is needed: the conversation
history *is* the deferred state.

**Correcting a Fact is two distinct operations.** A Fact that *changed over time*
is **superseded** (the old edge keeps its history — the bi-temporal model). A
Fact that was *wrong* is **retracted** — the edge is deleted, because it was
never true. The agent picks based on how the user phrases the correction.

**Review happens two ways, consistent with each other.** Correcting by
conversation is primary — *"you've got Josh and Devon backwards"* is a normal
recording turn. The **audit log** is a browsable backstop: every turn's applied
change, revertable at **per-Fact granularity** (a turn that recorded eight Facts
can have one reverted without losing the other seven — `UndoRecord` already
tracks `new_fact_ids` per commit).

Because Claude Code edits note files directly (§5, Option B), the app does not
see note writes as discrete tool calls. To support undo and the audit log it
**snapshots the whole formation before each turn** into
`.chat-notes/snapshots/<turn>/` (Markdown is tiny; the copy is cheap), then
diffs snapshot-vs-after to learn which notes changed. Graph writes *are*
discrete — they pass through the in-app MCP server — so the audit-log entry for a
turn is `{changed note files (before/after), recorded fact ids, retracted fact
ids}`. Full snapshots are retained for a bounded recent window (the audit log's
byte-revertable range); older turns are corrected conversationally.

`StagingEntry` and its review UI (the staging tray, conflict banners,
disambiguation suggestions) are removed — those interactions now happen in
conversation. Principle #3 is honoured by visibility + reversibility, not a
blocking review queue.

### 7. Extraction, intent classification, and tiers are removed

- **GLiNER / LLM extraction** — `core/extraction.rs`, `core/llm_extractor.rs`,
  the `gline-rs` + ONNX dependencies, and the GLiNER model download are removed.
- **Intent classification** — `core/intent.rs` removed (§2).
- **Hardware tiers** — the tier strategy (tech-spec §5) is removed; onboarding
  becomes "set up your engine," not "detect your tier."

Embeddings stay local: `nomic-embed-text` via the Ollama sidecar still backs the
`search_notes` index. The Ollama *chat* path is removed.

### 8. The agent's behaviour is a versioned prompt

The interrogating, cross-referencing, contradiction-catching behaviour is
emergent from the tool surface plus the agent's instructions. The behaviour
prompt — the agent persona, the questioning discipline, and the recommended
section vocabulary (§3) — is a first-class, versioned artifact checked into the
repo (`prompts/conversation-agent.md`), not a string literal.

## Consequences

- **Positive** — the product becomes a thinking partner that records, questions,
  and connects, in one conversation. A large net deletion: GLiNER, ONNX,
  `gline-rs`, the intent classifier, tier detection, the multi-prompt extraction
  chain, and the staging-tray UI all go.
- **Positive** — V1 has one engine and a small (~7-tool) graph-only MCP server;
  Claude Code's own file-editing does the heavy lifting for notes.
- **Negative — local-first is downgraded.** Principle #1 no longer holds for the
  primary interaction: the conversation and note content go to the user's
  installed `claude` CLI under their own subscription. Embeddings and the
  formation stay on-device. This is deliberate (framing decision #4) and must be
  stated plainly in onboarding and settings.
- **Negative — principle #2 is amended** (§4): the graph is a peer store, no
  longer reconstructable from the formation; corruption recovery loses an escape
  hatch.
- **Negative — latency and quota.** An agentic loop is several model round-trips
  per turn; the CLI adds ~6s cold start (ADR-0008). Every turn draws on the
  user's Claude subscription quota.
- **Negative — coarser note traceability.** Note bullets are traceable per-note
  per-turn, not per-bullet (§6), because note writes are not discrete tool calls.
  Graph-Fact traceability is unaffected — `record_fact` carries `source_chat_id`.
- **Neutral — prompt injection surface.** The agent reads user-authored notes and
  has file + graph tools. Blast radius stays bounded: no network/exfiltration
  tools, Bash disabled, every turn snapshotted and reversible.
- **Out of scope** — API engines; an autonomous background organise/connect pass;
  multi-formation context; a cloud embedder. The tech spec needs a v0.3 rewrite
  to match this ADR — tracked separately, not blocking the plan.

## Open questions

1. **Gemini CLI parity.** The second engine's session model and tool-config
   surface differ from Claude Code's; confirm the `ConversationEngine` trait
   absorbs the difference cleanly when M6 lands.

## Resolved during planning

- **Per-turn budget** — V1 caps the `claude` subprocess with a 300s wall-clock
  timeout (a safety net against a hung turn); no tool-call count cap. Revisit if
  turns run long in practice.
- **Snapshot retention** — full pre-turn formation snapshots are kept for the
  last 20 turns; older turns are corrected conversationally (§6).
