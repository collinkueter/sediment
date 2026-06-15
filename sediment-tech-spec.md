# Sediment — Technical Specification

**Status:** Draft v0.2 — **§2–§8 superseded by [ADR-0009](docs/adr/0009-conversational-agent.md); a v0.3 rewrite is pending.**
**Author:** Collin
**Last updated:** 2026-05-19

> ⚠️ **Out of date.** ADR-0009 reworked chat from a Write/Ask extraction
> pipeline into a single conversational agent loop. It supersedes §2–§8 of this
> spec (and amends design principles #1 and #2 — local-first and
> formation-authoritative). Specifically: GLiNER extraction, intent
> classification, the hardware-tier strategy, the staging tray, and the
> multi-prompt extraction chain are all removed. Treat §2–§8 as historical until
> the v0.3 rewrite lands; ADR-0009 is the current source of truth.

> **v0.2 changes:** Architecture simplified — Graphiti, FalkorDB, and LanceDB
> replaced by a single embedded SurrealDB instance (graph + vectors + documents).
> Entity and relation extraction handled by `gline-rs` (GLiNER ONNX inference,
> CPU). Python sidecar removed entirely. User-facing "vault" renamed to
> "formation."
>
> **Note on graphrag-rs.** Earlier drafts referenced `graphrag-rs` (automataIA)
> as the extraction crate. On inspection it's a 50KLOC opinionated full RAG
> framework whose `persistent-storage` feature pulls LanceDB back as a
> transitive dependency. We swapped to `gline-rs` (fbilhaut) — a focused
> ~5-dep GLiNER inference engine. A single multitask GLiNER model file
> (`gliner-multitask-large-v0.5`) covers both NER and relation extraction.

---

## 1. Name

**Sediment** (`github.com/[you]/sediment`)

The name comes from geology: sediment is material carried by a current — water, wind, time — that settles and accumulates into lasting, readable layers. Each layer is traceable to the conditions that formed it.

The metaphor maps precisely onto what the app does:

- **The stream is conversation.** Thoughts, facts, and observations flow in through chat — fragmented, raw, out of order.
- **Settling is extraction.** The AI distills that stream into structured knowledge, routing each fact to where it belongs.
- **Layers are temporal.** Like geological strata, every fact in the formation carries a timestamp and a source. Older facts don't disappear when new ones arrive — they get a validity window, readable as history. SurrealDB's bi-temporal edges make this tangible.
- **The formation accumulates.** Over time, sediment builds into something solid, navigable, and rich — a knowledge base that reflects exactly how understanding accumulated.

The word is available across the software namespace (no conflicting GitHub repos, npm packages, or major products), easy to spell, and works naturally as both a product name and a repository identifier.

---

## 2. Vision

A desktop note-taking app where the primary input is conversation. The user chats; the app silently does the work of a diligent assistant — extracting facts, routing them to the right notes, building a living knowledge graph, and surfacing what's already known. The user reviews staged changes (git-style) before they commit to the formation.

The app inverts the Obsidian model: instead of *being* the organizer, the user is the *thinker*. Documents are the output, not the input.

### Design Principles

1. **Local-first.** No data leaves the machine by default. Cloud LLM use is opt-in via BYOK.
2. **Formation is authoritative.** Markdown files on disk are the source of truth. All derived state (graph, embeddings, indexes) is reproducible from the formation.
3. **AI proposes, human disposes.** Every AI-generated change is staged for review before committing.
4. **Trust through traceability.** Every fact in the formation can be traced back to the chat message that produced it.
5. **Obsidian-compatible.** The formation should be openable and editable in Obsidian without breaking.

---

## 3. Core Concept

```
┌─────────────────────────────┬──────────────────────┐
│  NOTE VIEWER (left)         │  CHAT (right)        │
│                             │                      │
│  Auto-navigates to affected │  User brain-dumps    │
│  notes as AI stages changes │  thoughts, facts,    │
│                             │  questions           │
│  Inline diffs show proposed │                      │
│  edits with green/red       │  AI infers intent:   │
│                             │  Write or Ask        │
│  ┌─────────────────────┐    │                      │
│  │ STAGING TRAY        │    │  [Send] (Cmd+Enter)  │
│  │ 3 notes affected    │    │                      │
│  │ [Keep All][Discard] │    │                      │
│  └─────────────────────┘    │                      │
└─────────────────────────────┴──────────────────────┘
```

### The Two Modes

**Write Mode** (default for statements)
- User dumps thoughts; AI extracts entities, facts, tasks
- Routes each fact to the appropriate note (existing or new)
- Stages all proposed changes for review
- On Keep: formation writes, graph updates, embeddings refresh

**Ask Mode** (for questions)
- User asks about the formation contents
- AI queries the graph + vector index
- Returns answer with citations to source notes
- No formation changes are made

Mode is inferred from the message but displayed before the AI acts, with an inline override.

---

## 4. Architecture

### High-Level System Diagram

```
┌────────────────────────────────────────────────────────────────┐
│                    Tauri Desktop App                            │
│  ┌────────────────────────────────────────────────────────┐    │
│  │              React + TypeScript UI                      │    │
│  │  - Note viewer (CodeMirror 6)                          │    │
│  │  - Chat pane                                            │    │
│  │  - Staging tray                                         │    │
│  │  - Settings & onboarding                                │    │
│  └──────────────────────┬─────────────────────────────────┘    │
│                         │ IPC                                   │
│  ┌──────────────────────▼─────────────────────────────────┐    │
│  │              Rust Core (Tauri backend)                  │    │
│  │  - Formation file watcher (notify crate)                │    │
│  │  - Staging state machine                                │    │
│  │  - Extraction pipeline (gline-rs: multitask GLiNER)    │    │
│  │  - Prompt orchestration                                 │    │
│  │  - Snapshot/undo manager                                │    │
│  │  - Hardware detection & tier selection                  │    │
│  │  - SurrealDB (embedded, in-process)                     │    │
│  └──┬────────────────────────────┬────────────────────────┘    │
│     │                            │                              │
│  ┌──▼─────┐                ┌─────▼────────────┐                 │
│  │ Ollama │                │ Markdown         │                 │
│  │ (LLM   │                │ Formation        │                 │
│  │  side- │                │ (user's          │                 │
│  │  car)  │                │  Obsidian-       │                 │
│  │        │                │  compatible      │                 │
│  │        │                │  folder)         │                 │
│  └────────┘                └──────────────────┘                 │
│                                                                 │
│  Only external runtime is Ollama. Everything else lives        │
│  in-process. Network access only for opt-in BYOK.              │
└────────────────────────────────────────────────────────────────┘
```

### Component Responsibilities

| Component | Responsibility |
|---|---|
| **React UI** | Rendering, user interaction, diff visualization |
| **Rust Core** | Orchestration, file I/O, process management, state, extraction pipeline |
| **Ollama Sidecar** | Local LLM inference for generative steps (fact routing, diff drafting, Q&A) |
| **gline-rs (in-process)** | Deterministic entity extraction and relation extraction via a multitask GLiNER ONNX model, CPU-friendly |
| **SurrealDB (embedded, in-process)** | Single store for the bi-temporal knowledge graph, HNSW vector embeddings, chat history, and staging state. Queried via SurrealQL. |
| **Markdown Formation** | Source of truth; standard `.md` files in user's chosen folder |

### Why This Stack

- **Tauri** over Electron: smaller binary, lower memory baseline, native performance — critical when running local LLMs alongside.
- **SurrealDB embedded** over a graph DB + separate vector DB: one in-process store handles graph traversal, vector similarity, and document queries in a single SurrealQL round trip. No server, no IPC, no second binary to ship. Uses the `kv-surrealkv` backend for durable on-disk storage.
- **gline-rs** (multitask GLiNER ONNX model) for extraction: deterministic, schema-constrained, ONNX runtime that runs on CPU. Avoids the JSON-shaped-output reliability problems that small local LLMs have, while keeping LLMs for the genuinely generative steps (fact routing, diff drafting, Q&A). A single multitask model file handles both NER and RE.
- **Ollama**: standard local LLM runtime; handles model swapping; broad model support.
- **No Python runtime**: extraction, storage, and orchestration are all Rust. The Python sidecar from earlier drafts is gone — eliminating cross-language IPC, packaging complexity, and the second runtime's memory baseline.

---

## 5. Hardware Tiers & Model Strategy

The app detects hardware on first launch and recommends a tier. User can override.

### Tier Definitions

| Tier | RAM | GPU/Compute | Default Models | Capabilities |
|---|---|---|---|---|
| **Lite** | 16 GB | Apple Silicon M1+ or 8GB VRAM | Llama 3.2 3B + Nomic Embed v2 | Basic extraction, simple routing, slower Q&A |
| **Standard** | 32 GB | M3 Pro / 16GB VRAM | Qwen 2.5 14B or Llama 3.3 8B + Nomic Embed | Full extraction, good Q&A, decent disambiguation |
| **Pro** | 64 GB+ | M3 Max+ / 24GB+ VRAM | Llama 3.3 70B Q4 or Qwen 2.5 32B | Frontier-class reasoning, strong contradiction detection |
| **BYOK Cloud** | Any | N/A | User-provided API key (Anthropic, OpenAI, etc.) | Frontier-quality regardless of hardware |

### Hardware Detection

On first launch:
1. Detect platform (macOS / Windows / Linux)
2. Detect total RAM (`sys-info` crate in Rust)
3. Detect GPU/VRAM (platform-specific: Metal on macOS, CUDA/ROCm probing on Linux/Windows)
4. Score against tier thresholds
5. Recommend tier, allow override
6. Pull recommended models via Ollama in background

### Prompt Versioning

Each task has a prompt library indexed by `(task, tier, model_family)`:

```
prompts/
  entity_extraction/
    lite-llama3.md
    lite-qwen.md
    standard.md
    pro.md
    cloud.md
  intent_classifier/
    lite.md
    standard.md
    ...
```

This is critical: a prompt that produces clean JSON from Llama 3.3 70B may produce malformed garbage from Llama 3.2 3B. Each tier needs prompts tuned for its capability ceiling.

### Multi-Model Orchestration

Even within a tier, multiple models may run:
- **Small/fast model** (3B): intent classification, simple routing
- **Medium model** (14B-32B): entity extraction, diff generation
- **Embedding model** (Nomic Embed): vector embeddings

On smaller machines this means **model swapping** — Ollama handles loading/unloading but it costs latency. Strategy:
- Pin the "intent classifier" model in memory if RAM allows
- Lazy-load larger models on first use, keep warm for the session
- Embedding model is always loaded (small, frequent use)

---

## 6. Data Model

### Formation Structure

```
formation/
├── .chat-notes/                  # App metadata (gitignore-friendly)
│   ├── snapshots/                # Pre-commit snapshots for undo
│   ├── staging/                  # Pending changes not yet committed
│   ├── chat-history/             # Conversation transcripts by date
│   ├── memory/                   # SurrealDB embedded store (SurrealKV files)
│   ├── config.json               # Formation-specific settings
│   └── prompt-overrides/         # User customizations
├── People/
│   ├── John Smith.md
│   └── Sarah Chen.md
├── Meetings/
│   └── 2026-05-25 Standup.md
├── Projects/
│   └── Q2 Planning.md
├── Tasks.md
└── [user's existing notes...]
```

The `.chat-notes/` directory is app-specific state. The rest is standard Obsidian-style organization. Users can rearrange folders freely; the app respects existing structure.

### Note Frontmatter Convention

The app uses YAML frontmatter (Obsidian-compatible) to store metadata:

```yaml
---
type: person          # person | meeting | project | task | note
aliases: [John, JS]   # for entity resolution
created: 2026-05-19
updated: 2026-05-19
chat-notes:
  entity-id: "person_john_smith_001"
  last-extracted: 2026-05-19T14:32:00Z
  facts:
    - id: "fact_001"
      source-chat: "2026-05-19T14:30:00Z"
      confidence: 0.92
---

# John Smith

VP of Engineering at Acme Corp.

## Personal
- Son plays baseball (Little League, Monday mornings)
```

The `chat-notes` frontmatter block is app metadata. Users can edit the body freely; the app re-extracts from edits and updates its graph accordingly.

### Graph Schema (SurrealDB)

Entities are nodes; facts are graph edges with bi-temporal validity windows.
The full schema lives in [src-tauri/src/core/memory.rs](src-tauri/src/core/memory.rs)
and is applied at first launch.

```surql
-- Entities: people, orgs, meetings, projects, etc.
DEFINE TABLE entity SCHEMAFULL;
DEFINE FIELD entity_type    ON entity TYPE string
    ASSERT $value IN ['person','organization','meeting','project',
                      'task','topic','location','date','event'];
DEFINE FIELD canonical_name ON entity TYPE string;
DEFINE FIELD aliases        ON entity TYPE array<string> DEFAULT [];
DEFINE FIELD note_path      ON entity TYPE option<string>;   -- formation-relative
DEFINE FIELD embedding      ON entity TYPE option<array<float>>;
DEFINE FIELD created_at     ON entity TYPE datetime VALUE time::now();
DEFINE FIELD updated_at     ON entity TYPE datetime VALUE time::now();

DEFINE INDEX entity_name      ON entity FIELDS canonical_name;
DEFINE INDEX entity_embedding ON entity FIELDS embedding
    HNSW DIMENSION 768 DIST COSINE;

-- Facts: directed edges with a validity window and a source chat pointer.
DEFINE TABLE fact SCHEMAFULL TYPE RELATION FROM entity TO entity;
DEFINE FIELD predicate      ON fact TYPE string;             -- e.g. "works_at"
DEFINE FIELD valid_from     ON fact TYPE datetime;
DEFINE FIELD valid_to       ON fact TYPE option<datetime>;   -- NULL = current
DEFINE FIELD source_chat_id ON fact TYPE string;
DEFINE FIELD confidence     ON fact TYPE float DEFAULT 1.0;
DEFINE FIELD created_at     ON fact TYPE datetime VALUE time::now();
DEFINE INDEX fact_validity  ON fact FIELDS valid_from, valid_to;

-- Note chunks for semantic retrieval (vector search).
DEFINE TABLE note_chunk SCHEMAFULL;
DEFINE FIELD note_path  ON note_chunk TYPE string;
DEFINE FIELD chunk_idx  ON note_chunk TYPE int;
DEFINE FIELD text       ON note_chunk TYPE string;
DEFINE FIELD embedding  ON note_chunk TYPE array<float>;
DEFINE INDEX chunk_embedding ON note_chunk FIELDS embedding
    HNSW DIMENSION 768 DIST COSINE;

-- Chat history (also the audit trail for fact sources).
DEFINE TABLE chat_message SCHEMAFULL;
DEFINE FIELD role       ON chat_message TYPE string
    ASSERT $value IN ['user','assistant','system'];
DEFINE FIELD content    ON chat_message TYPE string;
DEFINE FIELD session_id ON chat_message TYPE string;
DEFINE FIELD timestamp  ON chat_message TYPE datetime VALUE time::now();
```

A change of employment becomes two edges in the same `fact` table — the old
edge gets its `valid_to` filled in; a new edge starts where the old one ended:

```surql
-- John worked at Acme from 2024 to 2026-03-15:
RELATE entity:john -> fact -> entity:acme
    SET predicate = 'works_at',
        valid_from = d'2024-01-01T00:00:00Z',
        valid_to   = d'2026-03-15T00:00:00Z',
        source_chat_id = 'msg_001';

-- Then moved to Beta Corp:
RELATE entity:john -> fact -> entity:beta_corp
    SET predicate = 'works_at',
        valid_from = d'2026-03-15T00:00:00Z',
        valid_to   = NONE,
        source_chat_id = 'msg_017';
```

Temporal queries:

```surql
-- All current facts about John
SELECT * FROM fact WHERE in = entity:john AND valid_to IS NONE;

-- What did we know about John on 2026-02-01?
SELECT * FROM fact
WHERE in = entity:john
  AND valid_from <= d'2026-02-01T00:00:00Z'
  AND (valid_to IS NONE OR valid_to > d'2026-02-01T00:00:00Z');
```

When a new fact contradicts an existing one, the old edge's `valid_to` is set
to the new edge's `valid_from`. Both rows are retained for historical queries.

### Staging State

Staged changes live in `.chat-notes/staging/` as JSON:

```json
{
  "id": "stage_2026-05-19_14-32",
  "created": "2026-05-19T14:32:00Z",
  "chat_message_id": "msg_abc123",
  "chat_excerpt": "John's kid has baseball Monday morning...",
  "status": "pending",
  "changes": [
    {
      "type": "update",
      "note": "People/John Smith.md",
      "diff": "...",
      "extracted_facts": [...],
      "confidence": 0.92
    },
    {
      "type": "create",
      "note": "Meetings/2026-05-25 Standup.md",
      "content": "...",
      "extracted_facts": [...],
      "confidence": 0.88
    }
  ]
}
```

A staging entry is one atomic "batch" — created by one Send action. Keep All commits everything; individual changes can be kept or discarded.

---

## 7. Core Flows

### 6.1 Write Flow

```
1. User types in chat pane
2. User hits Send (or Cmd+Enter)
3. Intent classifier runs → "Write"
4. UI shows "🔍 Treating as: Write · [Switch to Ask]"
5. Pipeline kicks off:
   a. gline-rs (multitask GLiNER): typed entities + candidate relation facts
   b. Formation retrieval (single SurrealQL query joining graph traversal +
      HNSW vector search on existing entity/note_chunk embeddings)
      → "John" resolves to People/John Smith.md (0.95 confidence)
      → New entity: Monday standup meeting
   c. Fact router (LLM) → which note for each fact
   d. Conflict detector (SurrealQL against the bi-temporal fact table)
   e. Diff generator (LLM) → produce markdown changes per note
6. Staging entry written to .chat-notes/staging/
7. UI updates:
   - Left pane auto-navigates to first affected note
   - Inline diffs render (CodeMirror 6 with diff extensions)
   - Staging tray shows list of affected notes
8. User reviews:
   - Click any row in tray → left pane jumps to that note
   - Keep individual / Keep All / Discard
9. On Keep:
   - Snapshot current formation state
   - Write markdown changes to disk
   - Apply graph + embedding updates in one SurrealDB transaction (new fact
     edges with validity windows; existing facts get `valid_to` filled in if
     superseded; affected note chunks re-embedded)
   - Remove staging entry
   - Show toast: "3 notes updated · Undo"
```

### 6.2 Ask Flow

```
1. User types question in chat pane
2. User hits Send
3. Intent classifier runs → "Ask"
4. UI shows "🔍 Treating as: Ask · [Switch to Write]"
5. RAG pipeline kicks off:
   a. Query expansion → gline-rs extracts entities + topics from question
   b. Hybrid retrieval — one SurrealQL query:
      - Graph traversal on the fact edges (entity + temporal constraints)
      - HNSW vector search on note_chunk embeddings
      - SurrealDB's query engine merges the two result sets
   c. Context assembly → top results + their source note paths
   d. Answer generation → LLM with strict "cite or refuse" prompt
6. Response renders in chat:
   - Answer text
   - Inline citations: [[Note Name]] (clickable, opens in left pane)
   - "Show sources" expandable shows retrieved passages
7. No formation changes. Chat history retained for follow-ups.
```

### 6.3 Intent Inference

Run on every message before pipeline dispatch:

```
Inputs:
  - message text
  - recent chat history (last 5 turns)
  - timestamp

Output:
  { mode: "write" | "ask", confidence: 0.0-1.0 }

Signals (weighted):
  - Question marks, interrogative words (what/who/when/why/how/does/is/are)
  - Imperative mood ("remind me", "tell me")
  - Statement structure (subject-verb-object factual)
  - Recent context (was the last turn an answer? continuation likely Ask)
  - Explicit prefix (slash commands as override)

Slash commands (always win):
  /ask <question>     → force Ask
  /write <statement>  → force Write
  /split              → split mixed-intent message

Threshold:
  - confidence >= 0.8 → proceed silently with inferred mode
  - confidence < 0.8  → show explicit prompt before processing
```

### 6.4 Formation Change Detection (External Edits)

The Rust core runs a file watcher on the formation directory:

```
1. File change event from notify crate
2. Debounce (500ms) to coalesce rapid saves
3. Diff against last-known content
4. For each changed section:
   a. Re-extract entities and facts (gline-rs)
   b. Diff extracted facts against current SurrealDB facts for that note
   c. Apply graph + embedding updates in one SurrealDB transaction
      (new fact edges, expire removed facts by setting valid_to, re-embed
      changed chunks)
5. Toast notification: "External changes detected in X notes · synced"
```

This is critical for Obsidian compatibility — users *will* edit notes directly, and the app must stay in sync.

### 6.5 Initial Formation Indexing

On first launch with an existing formation:

```
1. Walk the formation directory, list all .md files
2. App is usable immediately with limited capability:
   - Chat works
   - Ask mode returns "formation still indexing, X% complete"
   - Write mode works but new entities aren't yet de-duplicated against formation
3. Background indexing job:
   a. Priority queue: recently-modified files first
   b. For each file:
      - Parse frontmatter and content
      - Extract entities and facts via gline-rs
      - Insert entity nodes + fact edges into SurrealDB
      - Chunk content, embed via Ollama, write embeddings to note_chunk table
   c. Progress UI in settings/status bar
4. On completion: full Ask capability enabled
```

Estimated time on Standard tier: ~30 minutes for 500 notes. Lite tier: 2-3 hours. Pro: ~10 minutes.

---

## 8. Prompt Strategy

### Multi-Prompt Pipeline (per Write batch)

Rather than one mega-prompt, use specialized prompts chained together:

| Prompt | Purpose | Model Tier Used |
|---|---|---|
| `intent_classifier` | Write vs Ask | Smallest available |
| `entity_extractor` | Extract people, orgs, dates, topics from message | Small/medium |
| `entity_resolver` | Match extracted entities to existing formation entities | Small + graph query |
| `fact_extractor` | Extract facts with subject/predicate/object | Medium |
| `fact_router` | Determine which note(s) each fact belongs in | Medium |
| `conflict_detector` | Check fact against existing graph; flag contradictions | Medium |
| `diff_generator` | Produce final markdown changes per note | Medium/large |
| `qa_responder` | Answer question with citations (Ask mode) | Largest available |

### Structured Output Enforcement

All non-trivial prompts return JSON. Pipeline includes:

1. **JSON schema in prompt** — provide explicit schema example
2. **Strict parsing** — `serde_json` in Rust with full validation
3. **Retry-on-failure** — up to 3 attempts with corrective re-prompt
4. **Grammar-constrained decoding** — for Ollama, use GBNF grammars where supported
5. **Fallback heuristics** — regex/parser fallbacks for catastrophic failures

### Prompt Customization

Users can override system prompts in `.chat-notes/prompt-overrides/`. This is a power-user feature for tuning extraction style, but ships with sensible defaults.

---

## 9. UX Specification

### Layout

- **Left pane (60%)**: Note viewer with CodeMirror 6
  - Markdown rendering with live preview toggle
  - Diff highlighting (green/red gutters) when staged changes exist
  - Tabs for multiple open notes
  - Backlinks panel at bottom
- **Right pane (40%)**: Chat pane
  - Message history (current session)
  - Input box with multiline support
  - Send button + Cmd+Enter shortcut
  - Mode indicator above input
- **Bottom strip**: Staging tray (collapsible, expands on staged changes)

### Staging Tray Detail

```
┌──────────────────────────────────────────────────────────────┐
│ STAGED CHANGES  (from chat · 2 min ago)              [×]    │
│                                                              │
│ From: "John's kid has baseball Monday morning..."           │
│                                                              │
│ ✎  [[People/John Smith]]     +2 facts        [view] [✗]    │
│ ➕  [[Meetings/2026-05-25]]   new note        [view] [✗]    │
│ ✎  [[Tasks]]                 +1 item          [view] [✗]    │
│                                                              │
│              [Discard All]        [Keep All]                │
└──────────────────────────────────────────────────────────────┘
```

- Clicking `[view]` jumps left pane to that note with diff view active
- Each row keepable/discardable individually
- "Keep All" commits the batch atomically
- After commit: toast with "Undo" for ~10 seconds

### Diff Visualization

For an updated note, render inline:

```markdown
## Personal
- Lives in Des Moines, IA
+ Son plays baseball (Little League, Monday mornings)
```

Green = additions, red = deletions, no marker = unchanged. CodeMirror 6 has good diff support out of the box.

### Trust-Building UI Affordances

- **Source link on every staged fact**: hover shows the chat excerpt that produced it
- **Confidence indicator**: low-confidence extractions (< 0.8) get a yellow border
- **Conflict warning**: contradicting an existing fact shows a red banner with a side-by-side comparison
- **Recently committed**: formation notes show a subtle indicator if they were modified by the app in the last hour, with a "view edit history" affordance

### Onboarding (First Launch)

1. Welcome screen — explain the concept in 3 short slides
2. Choose formation location — new folder, existing folder, or empty
3. Hardware detection — show recommended tier, allow override
4. Model download — Ollama pulls recommended models, progress bar
5. Optional: BYOK cloud setup — for Lite users or as upgrade path
6. If existing formation: kick off background indexing, show progress
7. Sample chat: walk through one Write and one Ask example

---

## 10. Edge Case Handling

### Entity Disambiguation

When "John" could match multiple entities:

1. Check recent chat context (last 10 turns) — is one John more recent?
2. Check formation frequency — which John appears more often?
3. Check semantic similarity — does surrounding context match one John's known traits?
4. If still ambiguous, prompt the user inline:
   > 🔍 Which John? `[[John Smith]]` (coworker, mentioned 2h ago) or `[[John Doe]]` (brother-in-law)

User selection is recorded for future disambiguation in this session.

### Contradiction Detection

When a new fact contradicts an existing fact:

1. The bi-temporal `fact` edge in SurrealDB handles many cases automatically:
   the existing edge's `valid_to` is set to the new edge's `valid_from`, both
   rows remain queryable, and history is preserved.
2. For substantive contradictions, the staging UI shows:
   > ⚠️ This conflicts with an existing fact in `[[John Smith]]` from 2026-03-15:
   > - **Existing:** Works at Acme Corp
   > - **New:** Works at Beta Corp
   > [Update (Acme → Beta)] [Keep both as different periods] [Discard new]

### Hallucination Defense

Every fact in the formation has a source pointer back to its original chat message. The chat history is retained in `.chat-notes/chat-history/`.

Periodic audit feature (Pro tier and BYOK): scan low-confidence facts and flag them for user review.

For Ask mode: enforce citation-or-refusal. The prompt explicitly instructs the model to say "I don't have that information in your formation" rather than invent.

### Concurrent Edits

Formation edited externally while staging is pending:

1. File watcher detects external change
2. Check if affected file overlaps with any pending staged change
3. If yes:
   - Mark staged change as "stale"
   - Show warning in staging tray
   - Offer to re-extract from new content or discard
4. If no: proceed normally

### Cloud Sync (iCloud/Dropbox)

The formation may live in a synced folder. Implications:

- File events may arrive in bursts
- Brief file lock conflicts during sync — retry with exponential backoff
- App's `.chat-notes/` directory should *not* sync across machines (set per-platform sync exclusion: `.nosync` on macOS, etc.)
- Document this clearly in onboarding: graph and index are local-only

### Rename Detection

Obsidian users rename notes often. The notify crate exposes rename events on most platforms. Handle:

1. On rename event: update graph node's note pointer
2. Update vector store metadata (no re-embedding needed)
3. Update backlinks in other notes (preserving Obsidian's auto-rename behavior)

### Deleted Notes

User deletes `[[John Smith]]` in Obsidian:

1. File watcher detects deletion
2. Graph nodes corresponding to that note are *not* deleted (facts persist)
3. Note pointer marked as "orphaned"
4. Backlinks throughout formation are flagged
5. Toast: "John Smith was deleted. 12 backlinks orphaned. [Review]"
6. User can choose to: restore note (recreate from graph facts), accept orphaned, or purge facts

### Long Brain-Dump Batches

A 10-minute meeting transcript pasted in one Send:

1. Chunk by paragraph/topic boundary if message exceeds context window
2. Process chunks in sequence, accumulate staging entries
3. Present combined staging tray with grouping by entity:
   ```
   STAGED CHANGES (from long message)
   Group by: [Entity ▾] [Note] [Chronological]

   ▼ John Smith (4 changes)
   ▼ Sarah Chen (2 changes)
   ▼ Q2 Planning (3 changes)
   ```
4. Bulk actions: "Keep all John-related" / "Keep all meeting decisions"

### Mixed-Intent Messages

"What's Sarah's deadline? Also, John mentioned the budget is approved."

Intent classifier detects mixed intent and either:
- Auto-splits into two batches (one Ask, one Write)
- Or surfaces the split for user confirmation: `[Process as 2 separate: Ask + Write]`

### Empty/Unclear Input

Messages like "hmm" or "thinking..." → intent classifier returns low confidence on both. Pipeline short-circuits with a gentle "I'm not sure what to do with that — could you clarify?"

### Model Cold Start

First inference after launch:
1. Show "Warming up model..." indicator
2. Run a tiny dummy inference at app launch to preload
3. Keep model warm for the session

### Memory Store Corruption Recovery

If the embedded SurrealDB store corrupts (disk failure, interrupted write):
1. Detect on startup (a schema-version probe query)
2. Offer to rebuild from the formation
3. Rebuild process: drop and recreate `.chat-notes/memory/`, walk the
   formation, re-extract facts via gline-rs, re-embed chunks, repopulate
4. Time estimate based on tier and formation size

This is why **the formation is authoritative**: SurrealDB is always reproducible.

---

## 11. Security & Privacy

### Threat Model

The app is local-first; threat surface is limited to:
- Local file system access (user-controlled)
- Network access for Ollama model downloads (one-time, from ollama.ai)
- Optional BYOK API calls (user explicitly enables)

### Data Boundaries

| Data | Where it lives | Leaves machine? |
|---|---|---|
| Formation notes | User's filesystem | Only if user-synced folder |
| Chat history | `.chat-notes/chat-history/` | Never (unless BYOK enabled) |
| Graph data | `.chat-notes/graph.falkor` | Never |
| Vector embeddings | `.chat-notes/vectors.lance/` | Never |
| LLM inference | Ollama (local) | Never (unless BYOK enabled) |

### BYOK Mode

When enabled:
- User provides API key, stored in OS keychain (macOS Keychain, Windows Credential Manager, Linux libsecret)
- Each cloud call is logged in `.chat-notes/cloud-usage.log` with timestamp + token estimate
- UI shows clear indicator when a request is going to cloud vs local
- User can set per-task cloud preference: "Use cloud for Q&A only" / "Cloud for everything" / etc.

### Prompt Injection Defense

The formation contains user-authored content. Adversarial content in a note could attempt to manipulate the AI when notes are retrieved during Ask mode.

Mitigations:
- Retrieved note content is wrapped in clear delimiters in prompts
- Prompts include explicit instructions to treat retrieved content as data, not instructions
- Q&A responses never execute actions; they only produce text answers
- No automatic external API calls based on note content

---

## 12. Performance Targets

| Operation | Tier | Target |
|---|---|---|
| App launch to ready | All | < 5s (excluding model preload) |
| Intent classification | Lite | < 500ms |
| Intent classification | Standard | < 200ms |
| Intent classification | Pro | < 100ms |
| Full Write pipeline (single fact) | Lite | < 10s |
| Full Write pipeline (single fact) | Standard | < 5s |
| Full Write pipeline (single fact) | Pro | < 3s |
| Ask response (with citations) | Lite | < 15s |
| Ask response (with citations) | Standard | < 8s |
| Ask response (with citations) | Pro | < 4s |
| Formation file change → graph update | All | < 2s |
| Indexing throughput | Lite | ~3 notes/min |
| Indexing throughput | Standard | ~15 notes/min |
| Indexing throughput | Pro | ~50 notes/min |

These are targets, not guarantees. Performance depends heavily on note size and complexity.

---

## 13. MVP Scope

### V1.0 — Must Have

- [ ] Tauri shell with React UI (left pane / right pane / staging tray)
- [ ] Formation selection and initialization
- [ ] Hardware detection and tier recommendation
- [ ] Ollama integration with one model per tier
- [ ] SurrealDB embedded (bi-temporal graph + HNSW vectors + chat/staging docs)
- [ ] gline-rs extraction (multitask GLiNER, ONNX)
- [ ] Intent classifier (Write vs Ask)
- [ ] Entity extraction pipeline
- [ ] Fact routing to existing notes
- [ ] Diff generation and staging
- [ ] Single-note Keep/Discard
- [ ] Batch Keep All / Discard All
- [ ] Ask mode with citations
- [ ] File watcher for external edits
- [ ] Background formation indexing
- [ ] BYOK cloud fallback (Anthropic API)
- [ ] Onboarding flow
- [ ] Settings panel
- [ ] Local-first telemetry (errors only, opt-in)

### V1.1 — Should Have

- [ ] Temporal fact tracking UI (showing fact evolution over time)
- [ ] Conflict detection and resolution UI
- [ ] Multi-John disambiguation prompts
- [ ] Snapshot-based undo (10s window after commit)
- [ ] Prompt customization
- [ ] Graph visualization (read-only)
- [ ] Long-batch grouping in staging tray

### V2.0 — Nice to Have (Explicitly Out of V1)

- Voice input (Whisper integration)
- PDF/image ingestion
- Plugin system
- Mobile companion app
- Real-time collaboration
- Cross-formation references
- Calendar integration
- Recurring event support
- Obsidian plugin compatibility layer

### Explicit Non-Goals

- Multi-user / team features
- Cloud sync of app state across devices
- Built-in markdown WYSIWYG editor (CodeMirror is enough)
- Monetization, accounts, telemetry beyond opt-in error reports

---

## 14. Open Questions

Things that need resolution during prototyping:

1. **gline-rs + multitask GLiNER recall.** The crate is stable (1.0.1) but
   real-world recall on user-style notes (informal English, names with
   typos, project jargon) is unknown. Budget time for an LLM-based fallback
   path with schema-constrained grammar if zero-shot GLiNER under-performs.

2. **Embedding model in SurrealDB.** Nomic Embed (768-d) is the planned default
   but the HNSW index dimension is fixed at schema time. Either commit early or
   plan a versioned `note_chunk_v2` table for migration.

3. **SurrealDB binary size impact.** Embedded SurrealDB adds non-trivial code
   to the Tauri binary. Measure release-build size on macOS arm64 early; if
   excessive, evaluate feature flags or a thin shim layer.

4. **Model selection per tier.** Llama 3.3 vs Qwen 2.5 vs others — needs
   benchmarking on actual extraction tasks for each tier (now mostly for fact
   routing + diff generation rather than entity extraction).

5. **CodeMirror diff library.** Off-the-shelf options vs custom — needs prototype.

6. **Onboarding for non-Obsidian users.** What's the experience for someone who
   has no existing formation and no concept of markdown notes? May need a guided
   first-formation flow.

7. **Formation encryption.** Should the `.chat-notes/` directory be encrypted
   at rest? Default off, opt-in?

8. **License model for the app itself.** AGPL? MIT? Source-available? You said
   no monetization, but licensing affects contribution model.

---

## 15. Build Plan (Rough)

**Phase 1 — Foundation (4-6 weeks)**
- Tauri shell with basic UI scaffold
- Ollama sidecar integration
- Formation file I/O and watcher
- SurrealDB embedded (schema + smoke-tested temporal fact round-trip)

**Phase 2 — Memory Layer (3-4 weeks)**
- gline-rs extraction (multitask GLiNER)
- Bi-temporal write path: contradiction detection, valid_to backfill in a
  single SurrealDB transaction
- Formation → SurrealDB sync (background indexing of existing notes)
- HNSW embedding pipeline for entities and note chunks

**Phase 3 — Chat & Staging (4-6 weeks)**
- Chat UI
- Write pipeline (intent → extract → route → diff)
- Staging tray and Keep/Discard
- Diff visualization

**Phase 4 — Ask Mode (2-3 weeks)**
- Hybrid retrieval (graph + vector)
- Answer generation with citations
- Citation linking

**Phase 5 — Polish (3-4 weeks)**
- Onboarding flow
- BYOK cloud fallback
- Hardware tier detection
- Performance tuning
- Edge case handling (disambiguation, conflicts, etc.)

**Total estimate:** 16-23 weeks for V1.0. Aggressive solo timeline; realistic with focused part-time effort over 6 months.

---

## 16. Risks

| Risk | Severity | Mitigation |
|---|---|---|
| Local LLMs aren't smart enough for reliable extraction | High | gline-rs (ONNX) handles deterministic NER/RE; LLMs reserved for fact routing, diff drafting, Q&A; BYOK escape hatch |
| Zero-shot GLiNER recall on user-style notes may be poor | Medium | Multitask GLiNER chosen as a 1-file baseline; LLM-grammar fallback path; fine-tuning is an option later |
| SurrealDB binary size bloats the Tauri build | Medium | Measure release artefact size in Phase 1; gate features; explore `kv-mem`-only builds for tests |
| Hallucination → false facts in formation | High | Source attribution on every fact; confidence thresholds; periodic audits |
| Intent inference unreliable | Medium | Visible mode indicator; one-click correction; slash command override |
| Performance on Lite tier feels sluggish | Medium | Aggressive prompt minimization; lazy indexing; clear UI feedback |
| Obsidian compatibility breaks on edge cases | Medium | Extensive test corpus; frontmatter conventions; user beta testing |
| Formation corruption from concurrent edits | High | Snapshot before every commit; file locking during writes; conflict UI |

---

## Appendix A — Glossary

- **Formation:** The user's folder of markdown notes.
- **Stage / Staging:** Proposed but not yet committed changes, similar to git staging area.
- **Entity:** A person, place, project, etc. — a node in the knowledge graph.
- **Fact:** A statement about an entity, modeled as a temporal edge in the graph.
- **Tier:** Hardware capability class (Lite / Standard / Pro / BYOK).
- **BYOK:** Bring Your Own Key — opt-in cloud LLM usage with user-provided API key.
- **Write mode:** Chat input is treated as facts to file into the formation.
- **Ask mode:** Chat input is treated as a question to answer from the formation.

---

## Appendix B — Reference Links

- SurrealDB: https://surrealdb.com/
- SurrealKV (embedded backend): https://github.com/surrealdb/surrealkv
- gline-rs (fbilhaut): https://github.com/fbilhaut/gline-rs
- GLiNER: https://github.com/urchade/GLiNER
- Multitask GLiNER model (ONNX, used here): https://huggingface.co/onnx-community/gliner-multitask-large-v0.5
- Ollama: https://ollama.com/
- Tauri: https://tauri.app/
- CodeMirror 6: https://codemirror.net/
- Obsidian formation format conventions: https://help.obsidian.md/

---

*End of specification draft v0.2.*
