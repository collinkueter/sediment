# Sediment — Plan: conversational agent

**Status:** Accepted (2026-05-22) — see [ADR-0009](../adr/0009-conversational-agent.md),
refined through a design review and accepted for implementation the same day.
**Predecessor:** current HEAD `6e9af73`.

This reworks chat from a Write/Ask command bus into a single conversational
agent loop: each turn the agent grounds itself, records what it learns, and
replies — asking questions, flagging contradictions, surfacing related notes.
See ADR-0009 for the rationale and the resolved design decisions; this plan is
the build order.

---

## Context for a fresh session

Today `commands/chat.rs` exposes `chat_write` (extract → route → diff →
`StagingEntry`), `chat_ask` (retrieve → cited answer), and `classify_intent`.
ADR-0009 replaces all three with one streaming `chat_turn` command driven by a
`ConversationEngine`.

V1 ships **one engine — the Claude Code CLI**, spawned as an *agent* (the
reverse of ADR-0008's hardened answerer). It edits notes with its own native
file tools; Sediment exposes a small **graph-only MCP server** for the
bi-temporal graph and the embedding index. The Gemini CLI is the second engine
(M6); API engines come later.

Kept substrate: `core/memory.rs` (bi-temporal graph + vectors), the
snapshot/`UndoRecord` machinery in `core/staging.rs`, `core/ollama_sidecar.rs`
(embeddings only), `core/watcher.rs`, the formation-on-disk model.

Removed (M7): `core/extraction.rs`, `core/llm_extractor.rs`, `core/intent.rs`,
`core/router.rs`, `core/diff_gen.rs`, the `gline-rs`/ONNX deps, the hardware-tier
strategy. ADR-0008's `core/claude_code.rs` is re-architected, not deleted.

### Incremental landing

M1–M4 deliver a working Claude Code conversational agent end-to-end; M5 makes it
usable in the UI. The old `chat_write`/`chat_ask` commands stay wired until M5
flips the UI, so the app builds and runs at every milestone. M7 deletes the dead
code last.

---

## Architecture

```
chat_turn(message) ──► persist user msg ──► snapshot formation ──► ConversationEngine::run_turn
                                                                          │
                                                          ClaudeCodeEngine (V1)
                                                          spawn `claude` as an agent:
                                                            cwd = formation root
                                                            native file tools (notes)   ─────► formation/*.md
                                                            --mcp-config → formation MCP ──┐
                                                            --system-prompt = behaviour    │
                                                            stream-json (text + tool_use)  │
                                                                          │                │
                              turn ends ◄─────────────────────────────────┘                ▼
                                  │                                             formation_mcp  (stdio)
                    diff snapshot-vs-after → changed notes              ┌──────► formation_tools:
                    + recorded/retracted fact ids (from MCP log)        │        search_notes, find_entity,
                                  │                                     │        related_facts, find_contradiction,
                    append audit-log entry ──► quiet undo               └──────  record_fact, retract_fact, record_task
                                  │                                              → MemoryStore (bi-temporal graph + vectors)
                    stream reply + tool activity to chat
```

The agent reads/writes **notes** through Claude Code's native file tools
(ADR-0009 §5, Option B). The **graph** and the **embedding index** are reached
only through the MCP server — note read/write is deliberately *not* an MCP tool.

---

## Milestones

### M0 — ADR + plan + deps
- Write ADR-0009 and this plan. *(done)*
- Evaluate the Rust MCP SDK (`rmcp`) for the M2 stdio server; confirm `tokio`
  carries the `process` feature (it does — ADR-0008).
- **Verify:** `cargo check` clean on HEAD.

### M1 — `core/formation_tools.rs`: the graph tool surface
- Implement the seven tools from ADR-0009 §5 as plain async fns over
  `&MemoryStore` + `&Path`: `search_notes`, `find_entity`, `related_facts`,
  `find_contradiction`, `record_fact`, `retract_fact`, `record_task`.
- A `ToolSchema` (name, description, JSON-Schema params) per tool and a
  `dispatch(name, json_args) -> json_result` entry point.
- `record_fact` / `retract_fact` / `record_task` log every graph write to a
  per-turn change record (the audit log's discrete half).
- Register the module in `core/mod.rs`.
- **Verify:** unit tests per tool against a temp `MemoryStore` (the
  `tempdir_for_test` pattern); `dispatch` round-trips JSON; a
  `find_contradiction` test seeds a conflicting fact and asserts it is found.

### M2 — `core/formation_mcp.rs`: the stdio MCP server
- A stdio MCP server that exposes `formation_tools` via `dispatch`, served by a
  hidden binary subcommand `sediment --mcp-stdio` (formation path via env var).
- One server process per turn for V1 (ADR-0009 §5).
- **Verify:** drive the server through the MCP protocol — list-tools returns the
  seven schemas; call-tool round-trips a `find_entity` and a `record_fact`
  against a temp formation.

### M3 — `ConversationEngine` trait + the Claude Code agent engine
- `trait ConversationEngine { async fn run_turn(ctx, on_event) -> AppResult<TurnResult> }`
  — `ctx` carries the recent-window transcript; `on_event` streams `TextDelta` /
  `ToolActivity` / `Done`; `TurnResult` carries the reply text.
- Re-architect `core/claude_code.rs` into the `ClaudeCodeEngine`: spawn `claude`
  with cwd = formation root, native file tools enabled (Bash **off**),
  `--mcp-config` → the M2 server, `--strict-mcp-config`, `--system-prompt` = the
  behaviour prompt, `--no-session-persistence`. Extend the existing `stream-json`
  parser to surface `tool_use` events as `ToolActivity`.
- The behaviour prompt `prompts/conversation-agent.md` lands here — agent
  persona, questioning discipline, the recommended section vocabulary
  (ADR-0009 §3, §8).
- **Verify:** `stream-json` `tool_use` parsing covered by a captured-transcript
  fixture; a `#[ignore]` live test drives one real `claude` turn that records a
  fact through the MCP and edits a note.

### M4 — `chat_turn` command: recording, snapshot, audit, undo
- New streaming command `commands::chat::chat_turn(message, session_id,
  on_event: Channel<TurnEvent>)`: persist the user message; assemble the
  recent-window transcript from `chat_message`; snapshot the whole formation
  pre-turn into `.chat-notes/snapshots/<turn>/`; run the engine; diff
  snapshot-vs-after for changed notes; combine with the MCP graph-write log into
  one audit-log entry; persist the assistant message.
- Per-Fact + per-note undo, reusing `core/staging.rs` snapshot/`UndoRecord`.
- The watcher keeps eager embedding re-index on external edits and marks notes
  "externally changed"; graph reconciliation is the agent's job on next mention
  (ADR-0009 §4) — no dedicated sync code.
- **Verify:** a deterministic test with a `ScriptedEngine` (fake) — a turn edits
  a note and records two facts; the audit entry captures both; per-fact undo
  reverts one fact and the note snapshot, leaving the other.

### M5 — frontend: one conversation
- `ChatPane.tsx` — remove the mode toggle and `classify_intent`; call
  `chat_turn`; render streamed text + an inline tool-activity trail
  (*"searched your notes", "filed Josh → works_at → Cloudflare"*).
- Replace `StagingTray.tsx` with an audit-log panel: per-turn entries, per-Fact
  revert, 10-second quiet undo (reuse `UndoToast`).
- Engine setup in `SettingsModal.tsx` / `Onboarding.tsx` — reuse ADR-0008's
  `detect_claude_code`; show install/sign-in state; state plainly that the
  conversation goes to the user's Claude subscription (ADR-0009: local-first
  downgraded).
- `tauri.ts` — `chat_turn` wrapper + `TurnEvent` types.
- **Verify:** `npm run tauri dev` — hold a multi-turn conversation that records
  facts, asks a clarifying question, flags a contradiction; confirm notes +
  graph update and per-fact undo works. Build + lint clean.

### M6 — Gemini CLI engine
- A second `ConversationEngine` impl for the Gemini CLI; confirm the trait
  absorbs its different session/tool-config surface (ADR-0009 Open question 3).
- Engine picker in settings gains the second option.
- **Verify:** `#[ignore]` live test mirroring M3; manual `npm run tauri dev`.

### M7 — delete dead code + docs
- Remove `core/extraction.rs`, `core/llm_extractor.rs`, `core/intent.rs`,
  `core/router.rs`, `core/diff_gen.rs`, `commands/extraction.rs`, the tier logic
  in `core/hardware.rs` + `commands/hardware.rs`, the old
  `chat_write`/`chat_ask`/`classify_intent` and the superseded
  `commands/staging.rs` review commands. Drop `gline-rs` + ONNX from
  `Cargo.toml`; delete the GLiNER download command and
  `docs/scripts/download-gliner.sh`.
- Update `lib.rs` `invoke_handler`, `README.md`; flag a `sediment-tech-spec`
  v0.3 rewrite (ADR-0009 supersedes spec §2–§8).
- **Verify:** `cargo build`, `cargo clippy`, `cargo test`, `npm run build`, and
  `biome` all clean.

---

## Open questions / out of scope

- **Per-turn budget** — resolved: a 300s wall-clock timeout on the `claude`
  subprocess (M3), no tool-call count cap for V1.
- **Snapshot retention** — resolved: full pre-turn snapshots kept for the last
  20 turns (M4).
- **API engines** — Anthropic/OpenAI/Gemini HTTP tool-use loops are out of scope
  for this plan; the `ConversationEngine` trait leaves room for them.
- **Autonomous background organise/connect pass** — out of scope.
- **Cloud embeddings** — embeddings stay local (Ollama `nomic-embed-text`).

## Test strategy

The deterministic half is the CI gate, per the repo convention.
`formation_tools` (M1) and the `chat_turn` snapshot/audit/undo path with a
`ScriptedEngine` (M4) are plain unit tests against temp stores. The MCP server
(M2) is tested through the protocol; the `stream-json` `tool_use` parser (M3)
against a captured transcript. Every binary-dependent test — the real `claude`
turn (M3, M6) — is `#[ignore]`d and run manually, matching ADR-0006's Layer 2
convention. The frontend (M5) is verified manually via `npm run tauri dev`.
