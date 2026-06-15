# Sediment

A desktop note-taking app where the primary input is **conversation**. You chat with an AI agent; it grounds itself in your notes, records what it learns, and questions you when something is unclear or contradicts what it already knows. Notes live as plain Markdown in an Obsidian-compatible folder (a "formation").

See [ADR-0009](docs/adr/0009-conversational-agent.md) for the current architecture. The [tech spec](sediment-tech-spec.md) predates ADR-0009 and is pending a v0.3 rewrite.

## How it works

Every chat message is one **conversational turn** (ADR-0009):

1. The message is persisted, and the whole formation is snapshotted.
2. The turn is **grounded before the agent runs** (ADR-0011): Sediment resolves the people and things you named to their existing notes and current Facts, pulls related notes, and derives a **Working Set** of what's in play — pushed into the prompt so the agent never has to guess what to search. Then a **conversational agent** runs (an agentic CLI with the formation as its working directory), reading and writing notes with its native file tools and reaching Sediment's bi-temporal knowledge graph through a small graph-only MCP server (`search_notes`, `find_entity`, `related_facts`, `find_contradiction`, `record_fact`, `retract_fact`, `record_task`, `record_open_loop`, `close_open_loop`).
3. The agent streams back a real conversational reply — acknowledging, asking clarifying questions, flagging contradictions, surfacing related notes.

There are no Write/Ask modes: recording and answering happen in the same turn. Review is an **audit log** — every turn's applied change is browsable and revertable at per-Fact granularity, with a quiet 10-second undo.

The agent runs on the user's installed **Claude Code CLI** or **GitHub Copilot CLI** (picked in Settings), under their own subscription. The Copilot engine runs *warm* — a resident `copilot --acp` session held across turns (ADR-0012). Note search runs on a local embedding model (`nomic-embed-text` via Ollama) and stays on-device, along with the formation itself.

**Tasks & reminders (ADR-0007).** The agent can file a reminder into a managed `## Tasks` region of `Tasks.md` (Obsidian-Tasks-compatible) and a SurrealDB `task` table; a background scheduler raises an OS + in-app notification when a reminder comes due. The title-bar bell opens a reminders popover.

See [docs/plans/](docs/plans/) for milestone breakdowns and [docs/adr/](docs/adr/) for the architecture decisions.

## Prerequisites

- **macOS** (Apple Silicon recommended). Linux/Windows come post-V1.
- **Rust** ≥ 1.88 — `rustup update stable`
- **Node** ≥ 22 + npm
- **Xcode Command Line Tools** — `xcode-select --install`
- **Ollama** — install from [ollama.com/download](https://ollama.com/download). The app spawns `ollama serve` for you on first launch; it backs note-search embeddings only.
- **An agent CLI** — install the [Claude Code CLI](https://claude.com/claude-code) or the **GitHub Copilot CLI** (`npm install -g @github/copilot`) and sign in. Pick which one Sediment uses in Settings.

## Quickstart

```bash
git clone <repo> sediment && cd sediment
npm install
npm run tauri dev
```

The first build compiles a large Rust dep tree (Tauri, SurrealDB) — expect ~5-10 min cold. Subsequent builds are incremental.

On first launch Sediment checks the embedding model and downloads it if missing:

```bash
ollama pull nomic-embed-text      # note-search embedding model
```

## Verifying a conversation

With a formation open and an agent CLI signed in:

1. **Record a fact.** Type `Josh works at Cloudflare.` The agent files it into `People/Josh.md` and records a graph Fact; the inline tool-activity trail shows what it did.
2. **Clarifying question.** Send something vague and watch the agent ask for the missing detail rather than guessing.
3. **Contradiction.** Send a fact that conflicts with one already recorded — the agent flags it and asks how to resolve before recording.
4. **Audit + undo.** Open the audit log; each turn's changed notes and recorded Facts are listed. Revert a whole turn or a single Fact.
5. **External edit.** Edit a formation file outside Sediment; the watcher re-indexes it within ~2s.

## Project layout

```
sediment/
├── src-tauri/                       # Rust core
│   ├── src/
│   │   ├── lib.rs                   # Bootstrap + command registration + --mcp-stdio
│   │   ├── error.rs                 # AppError + AppResult
│   │   ├── commands/                # #[tauri::command] handlers
│   │   │   ├── formation.rs         # Open / list / read / write notes
│   │   │   ├── memory.rs            # SurrealDB index + search
│   │   │   ├── ollama.rs            # Ollama status / list / generate
│   │   │   ├── chat.rs              # chat_turn — the conversational agent
│   │   │   ├── audit.rs             # Audit log: list / undo turn / undo fact
│   │   │   ├── hardware.rs          # Onboarding state
│   │   │   ├── models.rs            # Embedding-model readiness + pull
│   │   │   ├── settings.rs          # Conversation-engine selector + models dir
│   │   │   └── tasks.rs             # Task list: list / complete / snooze (ADR-0007)
│   │   └── core/                    # Long-lived subsystems
│   │       ├── formation_state.rs   # Active formation + AppConfig
│   │       ├── watcher.rs           # Debounced notify watcher
│   │       ├── memory.rs            # Embedded SurrealDB store (graph + vectors)
│   │       ├── ollama_sidecar.rs    # Ollama daemon lifecycle + client
│   │       ├── conversation.rs      # ConversationEngine trait
│   │       ├── claude_code.rs       # Claude Code CLI agent engine (cold-spawn)
│   │       ├── copilot.rs           # GitHub Copilot warm ACP engine (ADR-0012)
│   │       ├── pre_pass.rs          # Deterministic pre-pass grounding (ADR-0011)
│   │       ├── working_set.rs       # The derived Working Set (ADR-0011)
│   │       ├── formation_tools.rs   # The graph tool surface
│   │       ├── formation_mcp.rs     # The stdio MCP server
│   │       ├── audit.rs             # Per-turn snapshot / diff / audit log
│   │       ├── indexer.rs           # Background note indexer
│   │       ├── tasks.rs             # Task model + `task` table (ADR-0007)
│   │       ├── task_note.rs         # `Tasks.md` `## Tasks` render/parse
│   │       └── reminders.rs         # Background reminder scheduler
├── prompts/conversation-agent.md    # The versioned agent behaviour prompt
├── src/                             # React + TS
│   ├── App.tsx                      # Title bar + layout
│   ├── components/                  # ChatPane, NoteViewer, AuditLog, etc.
│   ├── lib/
│   │   ├── tauri.ts                 # Typed invoke wrappers
│   │   ├── store.ts                 # Zustand stores
│   │   └── codemirror/setup.ts      # CM6 extensions
│   └── styles/globals.css           # Tailwind v4 entry
├── docs/adr/                        # Architecture Decision Records
└── sediment-tech-spec.md            # Spec (v0.2 — superseded by ADR-0009)
```

## Common scripts

```bash
npm run tauri dev      # Launch the desktop app with hot reload
npm run dev            # Just the Vite frontend (useful for UI-only iteration)
npm run build          # Type-check + produce a production Vite bundle
npm run lint           # Biome lint
npm run typecheck      # tsc --noEmit

cd src-tauri
cargo check            # Fast Rust compile-check
cargo test --lib       # Unit tests (binary-dependent live tests are `#[ignore]`)
cargo clippy --lib --bins
```

## Troubleshooting

**`cargo check` fails on `arrow-arith` / chrono trait conflict** — the lockfile pins `chrono = 0.4.41` to dodge a `quarter()` trait clash in newer chrono. If a fresh clone tries a newer version, run `cargo update -p chrono --precise 0.4.41`.

**`ollama serve` doesn't auto-start** — confirm `ollama` is on PATH (`which ollama`). If the daemon refuses to start, run `ollama serve` manually and the app detects it on next launch.

**Vite port 1420 already in use** — `lsof -ti:1420 | xargs kill -9` clears a stray dev server.

**Chat fails immediately** — make sure the agent CLI selected in Settings is installed and signed in (`claude` or `gemini` in a terminal).

## License

TBD — repo is private until the license decision is made.
