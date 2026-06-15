# ADR-0008: Claude Code as an answer-generation engine

**Status:** Proposed (2026-05-22)
**Relates to:** ADR-0006 (LLM-backed extraction), tech-spec §15 (BYOK cloud generation)
**Plan:** [docs/plans/claude-code-answer-engine.md](../plans/claude-code-answer-engine.md)

## Context

Ask-mode answer generation has two backends today (`commands/chat.rs::chat_ask`):
the tier's local Ollama model streamed token-by-token, or — when the user
configures BYOK — one non-streaming cloud call (`core/cloud.rs`, Anthropic or
OpenAI over HTTP). BYOK lets a machine that cannot host a capable local model
still produce a strong answer, but it asks the user to create an API key, paste
the secret into Sediment, and carry per-token billing.

Many users already pay for Claude — a Pro or Max subscription. That entitlement
has **no public API**: there is no OAuth flow a third-party app can use to spend
it, and scraping the claude.ai web session is brittle and against the ToS. But a
user who has installed the Claude Code CLI has *already* authenticated it
against that subscription, and its headless print mode (`claude -p`) is a
documented, supported automation surface. Shelling out to the local `claude`
binary lets Sediment generate against the subscription the user already pays for
— no key to paste, no second bill.

This was verified against Claude Code v2.1.144: `claude auth status --json`
reports `authMethod: "claude.ai"` for a subscription login, and `claude -p`
honours that auth in non-interactive mode.

## Decision

### A `claude-code` answer engine, beside Local and BYOK

Ask-mode generation gains a third engine. It is neither an Ollama model nor an
HTTP call: it spawns the user's `claude` binary as a subprocess and reads its
output. A new `core/claude_code.rs` module owns binary discovery, auth
detection, and generation; `core/cloud.rs` stays honestly HTTP-only, matching
its module doc.

### Isolated subprocess invocation

The CLI is invoked for one self-contained Q&A turn — the retrieved note
excerpts and the question are the whole prompt (built by the existing
`build_ask_prompt`), passed on **stdin** to avoid `ARG_MAX` and shell-escaping.
The invocation is hardened so Claude Code behaves as a plain answerer, not a
coding agent with access to the user's machine:

- `--tools ""` — disables all *built-in* tools (Bash, Read, Edit, …).
- `--strict-mcp-config` — **required**: `--tools ""` does **not** disable MCP
  servers. Without this flag the user's globally-configured MCP servers still
  load (confirmed in the `system/init` event), adding startup cost and an
  unwanted tool surface. With it, and no `--mcp-config`, zero MCP servers load.
- `--disable-slash-commands` — no skills.
- `--system-prompt "<minimal answerer persona>"` — replaces Claude Code's large
  default coding-agent system prompt. This both stops agentic behaviour and
  cuts the per-call token overhead substantially (see Consequences).
- `--no-session-persistence` — leaves no session files in the user's `~/.claude`.
- a neutral working directory — so CLAUDE.md auto-discovery and git-status
  injection cannot pull in the user's filesystem.

`--bare` would cut overhead further, but it is incompatible with this decision:
its help states *"Anthropic auth is strictly ANTHROPIC_API_KEY or apiKeyHelper …
OAuth and keychain are never read"* — it disables exactly the subscription auth
this engine exists to use.

### Streaming via `stream-json`

To match the local engine's token-by-token feel, the engine runs
`--output-format stream-json --include-partial-messages` and parses the
newline-delimited event stream. Only `stream_event` events whose
`event.type == "content_block_delta"` and `delta.type == "text_delta"` are
forwarded to the `on_token` channel; `thinking_delta` / `signature_delta` blocks
are ignored. The terminal `{"type":"result"}` line carries `is_error`,
`subtype`, and the complete `result` text — used to detect a failed turn and to
persist the assistant message. A `rate_limit_event` with a non-`allowed` status
is surfaced as a "Claude usage limit reached" error rather than a generic one.

### Detection: binary + auth, with no request cost

The settings and onboarding UIs need to know whether this engine is available.
`core/claude_code.rs::locate()` resolves the binary — a macOS GUI app does not
inherit the user's shell PATH, so it checks known install paths
(`~/.local/bin/claude` for the native installer, `~/.claude/local/claude`,
Homebrew prefixes) and falls back to a login-shell PATH probe.
`status()` then runs `claude auth status --json` — exit 0 and
`{loggedIn, authMethod, subscriptionType, email}` — so login state is known
without spending a generation.

### An explicit `answer_engine` selector

Today "is BYOK on?" is implied by whether `byok_provider` is set. Three engines
need an explicit selector: `AppConfig` gains `answer_engine: Option<String>`
(`None`/`"local"` → Ollama, `"byok"` → the existing HTTP path, `"claude-code"`
→ this engine). For back-compat, a `None` `answer_engine` with a configured
`byok_provider` + key is treated as `"byok"`, so existing BYOK users are not
silently downgraded to local.

### Extraction stays local

Per ADR-0006, fact extraction remains on the local model regardless of the
answer engine. This decision touches Ask-mode *generation* only.

## Consequences

- **Positive** — a user with a Claude subscription gets strong answers with no
  API key, no paste, and no second bill. The option is offered only when
  `locate()` finds the binary, so it is invisible noise to everyone else.
- **Positive** — `core/cloud.rs` stays HTTP-only; the new module is independently
  testable and the `answer_engine` selector makes the three-way choice explicit.
- **Negative** — per-call overhead. A measured trivial call created ~23k cached
  input tokens because Claude Code injects a large default system prompt;
  `--system-prompt` mitigates this but does not eliminate it. On a subscription
  this is quota, not dollars, but heavy app use draws down the user's Claude
  allowance — the settings UI must say so.
- **Negative** — latency. Each turn spawns a Node process (~6s cold in testing),
  slower to first token than either the local stream or an HTTP BYOK call.
- **Negative** — coupling to an external CLI. Flag names and the `stream-json`
  schema can change across `claude` versions; the stream parser is pinned to
  documented v2.1.144 behaviour and covered by a fixture test.
- **Neutral** — generation now has three engines; `chat_ask` resolves an
  `AnswerEngine` enum up front instead of a two-way branch.
- **Out of scope** — using Claude Code for *extraction*, the Claude Agent SDK as
  an alternative to the raw CLI, app-bundled Claude Code installation, and a
  per-turn `--max-budget-usd` cap for API-key-authed installs.
