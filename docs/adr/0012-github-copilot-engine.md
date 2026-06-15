# ADR-0012: GitHub Copilot as a co-equal conversation engine — and the warm path

**Status:** Proposed (2026-06-15) — from a web-sourced research spike on
subscription-authed agent runtimes (sources in *Findings*).
**Amends:** [ADR-0011](0011-working-set-and-push-grounding.md) §6 (warm +
subscription *is* reachable — via Copilot, not Claude); [ADR-0009](0009-conversational-agent.md)
§5 + Open-question 1 (retires the Gemini CLI as the second engine; Copilot takes
that slot); [ADR-0008](0008-claude-code-answer-engine.md) (generalises the
"drive the user's own subscription-authed CLI" stance; notes Anthropic's
2026-06-15 Agent-SDK-credit change).
**Keeps:** ADR-0009's `ConversationEngine` trait (now validated by a third
implementation) and single conversation.

## Context

ADR-0011 §6 wanted "warm the agent, keep the subscription," and the auth spike
found **no path for the Claude Code engine**: the Agent SDK is API-key-only;
`--continue` / `--resume` restore context but still cold-spawn. The user then
asked for **GitHub Copilot** as the next engine, on the user's Copilot
subscription. A research spike evaluated Copilot and the wider field of
subscription-authed agentic CLIs, and it reframes ADR-0011 §6.

Two findings drive this ADR:

1. **GitHub is the lone vendor that ships an official, embeddable,
   subscription-authed agent SDK** (`@github/copilot-sdk`, MIT, multi-language
   incl. a Rust binding), plus an **ACP JSON-RPC resident mode** (`--acp --stdio`)
   — i.e. a genuinely *warm*, subscription-authed loop. The thing the spike proved
   Claude cannot do, Copilot can.
2. **The third-party-use ToS landscape is the inverse of intuition.** Ranked by
   "drive the user's own subscription" defensibility: **Copilot green** (GitHub
   publishes the SDK for exactly this, including desktop apps), **Claude
   green-ish** (driving the official binary is sanctioned; token *reuse* is what's
   banned), **Codex yellow** (subscription login exists but OpenAI steers
   programmatic use to API keys), **Gemini red** (third-party use of Gemini-CLI
   OAuth is *explicitly prohibited and enforced* since 2026-03-25). The planned
   second engine — Gemini — is the riskiest of the four.

## Decision

### 1. Copilot is a first-class, co-equal `ConversationEngine`

Beside the Claude Code engine, the user picks in Settings. ADR-0009 §5's trait
absorbs it as a third implementation — no architecture change. The **Agent**
(persona, questioning discipline, tools) is identical whichever engine runs it.

### 2. Copilot is the warm path; Claude stays cold-spawn + mask

Copilot's ACP (`--acp --stdio` JSON-RPC) and the Copilot SDK hold a **resident
process across turns on the subscription** — resolving ADR-0011 §6 *for the
Copilot engine*. The Claude Code engine keeps ADR-0011 §6's cold-spawn + masking.

Mitigate the resident-process degradation (community bug #2755 — latency drifts to
~17–30 s/turn as internal state accrues) by **recycling on a persisted
`--session-id`** rather than holding one process indefinitely; session state
persists across the restart, so continuity is preserved.

### 3. Integration surface: the Copilot Rust SDK or ACP JSON-RPC — not raw `-p`

`copilot -p --output-format json` emits JSONL, but its **per-event schema is
undocumented** and the CLI churns (flags removed without deprecation; 0.0.x →
1.0.x). Drive the **specified** surface — the Rust SDK or the ACP JSON-RPC
protocol — and **pin a version**, watching the changelog.

### 4. The graph-only MCP server ports over

`--additional-mcp-config` accepts the Claude-flat config format; the stdio MCP
server maps almost verbatim. Scope to the formation via `cwd` + `--add-dir`.
Mirror Bash-off by **enumerating allowed tools** (Copilot has no single bash
switch; gate via `--allow-tool` / `--deny-tool`). **Caveat:** no
`--strict-mcp-config` analog was found, so the built-in GitHub MCP server may not
be fully suppressible — mute its tools with `--deny-tool` (Open question 3).

### 5. Retire the Gemini CLI engine

Google's Gemini CLI ToS **explicitly prohibits** third-party use of Gemini-CLI
OAuth and enforces it via abuse detection (2026-03-25). Shipping ADR-0009's M6
Gemini engine would route the user's Google OAuth through Sediment against ToS.
**Copilot takes the second-engine slot; Gemini is dropped** — revisit only if
Google sanctions a third-party path.

### 6. The engine ToS stance (the line that keeps every engine compliant)

Sediment only ever drives the user's **own installed, own-authenticated
first-party binary**. It never: extracts or reuses a vendor OAuth token inside
Sediment; offers a vendor login in-app; or routes more than one user's traffic
through a single subscription. This is the documented-allowed pattern for Claude
and Copilot, the boundary Gemini's ToS draws, and the caution Codex's docs
express. Per-user, human-in-the-loop, drive-the-real-binary.

## Findings (research spike, 2026-06-15)

Confidence and sources noted; community-reported magnitudes flagged.

| Engine | Subscription auth | Warm/resident | Headless stream | Custom MCP | Third-party-use ToS |
|---|---|---|---|---|---|
| **Claude Code** | Yes (claude.ai OAuth) | No (cold `-p`; spike) | `stream-json` | `--mcp-config`, `--strict-mcp-config` | Drive-binary allowed; token-reuse banned. **New 2026-06-15:** `-p` draws a separate monthly "Agent SDK credit." |
| **GitHub Copilot** | Yes (GitHub OAuth → Copilot quota) | **Yes** (`--acp --stdio`; SDK) | `-p --output-format json` (schema undocumented) / ACP | `--additional-mcp-config`; Claude-flat ok; built-ins maybe unsuppressible | **No prohibition found; GitHub ships an SDK for it.** |
| **OpenAI Codex** | Yes ("Sign in with ChatGPT") | Yes (`app-server`) | `exec --json` (NDJSON) | `codex mcp` | Gray — "recommend API keys for programmatic." |
| **Gemini CLI** | Yes (Google login) | Yes | `--output-format` JSONL | Yes | **Prohibited & enforced** for third-party OAuth use. |

The embeddable-SDK question: **only GitHub** offers an embeddable SDK that
authenticates via the user's consumer subscription; Anthropic (Agent SDK),
OpenAI, and Google route SDK/library use to API keys. Sources: GitHub Copilot CLI
GA changelog (2026-02-25), `@github/copilot-sdk` repo + GA changelog (2026-06-02),
Copilot SDK auth doc, GitHub ToS §J + AUP; Claude Code legal-and-compliance;
OpenAI Codex auth doc + usage policies; Gemini CLI ToS + enforcement discussion.
Unverified/flagged: Copilot `-p` per-event schema; whether the built-in GitHub MCP
server can be fully disabled; exact tool names for `--allow-tool`/`--deny-tool`;
Free-tier Copilot CLI eligibility; the #2755/#3329 latency magnitudes
(issue-tracker reports, not official benchmarks).

## Consequences

- **Positive** — gains a *warm, subscription-authed* engine (closing the ADR-0011
  §6 gap) and the most ToS-defensible engine; validates the `ConversationEngine`
  trait; the Copilot Rust SDK is a clean fit for the Rust core.
- **Positive** — retiring Gemini removes a ToS-non-compliant engine *before* it
  ships.
- **Negative** — the Copilot CLI/SDK is young and churny (flag removals, version
  jumps); pin + watch the changelog; prefer the SDK's stabler surface.
- **Negative** — resident-process degradation (#2755) needs a recycle strategy;
  an MCP-connect race on turn 1 (#3329) needs a warm-up no-op or a reused warmed
  session.
- **Negative** — every turn meters the user's premium-request quota. This
  *reinforces* the no-background-daemon choice (ADR-0011 §4), but heavy use draws
  the user's Copilot allowance; Settings must say so (as ADR-0008 says for Claude).
- **Neutral** — the two engines differ in warmth (Copilot warm; Claude cold +
  masked) and stream format (ACP/JSONL vs Anthropic `stream-json`); the trait
  abstracts both and each engine owns its parser.
- **Out of scope** — the API engines (still later); the warm-Claude `stream-json`
  prototype (ADR-0011 Open-question 1 — now lower priority, since Copilot supplies
  warmth); Free-tier Copilot eligibility.

## Spike results (M7, 2026-06-15)

Ran against `@github/copilot` **1.0.62** on the user's machine. Confirmed:

- **Subscription auth works.** A non-interactive `copilot -p … --allow-all-tools`
  turn ran and billed the Copilot subscription (`result.usage.premiumRequests`
  = 0.33). No API key.
- **`--output-format json` is JSONL**, one event per line. The schema the M8
  parser needs:
  - `assistant.message_delta` `{messageId, deltaContent}` → reply token (stream as `TextDelta`).
  - `assistant.reasoning_delta` → thinking; **ignore** (parallels Claude's `thinking_delta`).
  - `assistant.message` `{content, toolRequests[]}` → settled reply + tool calls (→ `ToolActivity`).
  - `result` `{exitCode, sessionId, usage.premiumRequests}` → terminal.
  - Skip the setup noise: `session.mcp_servers_loaded`, `session.skills_loaded`, `session.tools_updated`, `user.message`, `assistant.turn_start/turn_end`.
- **Built-in GitHub MCP is suppressible** — `--disable-builtin-mcps` (the server
  reported `status: disabled`).
- **MCP / scoping flags** — `--additional-mcp-config @<file>` takes our stdio MCP
  config file; `--add-dir <formation>` / `-C <formation>` scope file access; tool
  gating is `--allow-all-tools` / `--deny-tool` / `--available-tools` /
  `--excluded-tools`.

**Integration decision:** drive the **raw CLI as a subprocess and parse the
JSONL**, mirroring `core/claude_code.rs` (struct + spawn + `parse_stream_line` +
`drive_turn`). The Rust SDK is **not** needed — the CLI surface is consistent
with the existing engines.

**New gotchas for M8:**
- **No `--system-prompt`.** Copilot has none (unlike Claude's `--system-prompt`,
  Gemini's `GEMINI_SYSTEM_MD`). V1 prepends the behaviour prompt to the `-p` text
  in the engine's `render_turn_prompt`. (Alternatives: a `copilot-instructions` /
  `AGENTS.md` file, or `--agent`.)
- **Prompt is a CLI arg** (`-p <text>`), not stdin — watch ARG_MAX for long
  prompts; the budgeted context keeps it small, a temp-file fallback if needed.
- **Copilot injects its own context** — it loads the user's personal agent skills
  and a `todos`/SQL reminder. Harmless but adds tokens; look for a disable flag.

## Open questions

1. **Warm vs cold for M8.** The cold-spawn raw-CLI engine (above) is the simplest,
   consistent-with-existing-engines first version, but it has Claude's ~6 s
   per-turn start and so loses Copilot's *warmth* advantage (§2). The warm path —
   a resident `--acp` process fed turns, recycled on `--session-id` (degradation
   bug #2755) — is a larger lift (a persistent `EngineHandle` state). Decide
   whether M8 ships cold-first (warm as M8b) or warm directly.
2. **Recycle cadence** (warm only) — measure the #2755 degradation curve.
3. **Disable Copilot's own skills/todos injection** — find the flag, or accept the
   token overhead.
4. **Subscription policy at distribution** — unchanged; a distribution-time check
   (ADR-0011 Open-question 2). §6's stance is the mitigation, not legal advice.
