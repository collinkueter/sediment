# Sediment — Plan: Claude Code answer engine

**Status:** Proposed (2026-05-22) — see ADR-0008.
**Predecessor:** current HEAD `6e9af73`. Builds on BYOK cloud generation
(tech-spec §15, `core/cloud.rs`) and LLM-backed extraction (ADR-0006).

This adds a third Ask-mode answer engine: the user's locally-installed Claude
Code CLI, driven headlessly so generation runs against their existing Claude
Pro/Max subscription — no API key, no second bill.

---

## Context for a fresh session

`commands/chat.rs::chat_ask` generates an answer one of two ways today
([chat.rs:456](../../src-tauri/src/commands/chat.rs)):

- **Local** — streams from the tier's Ollama model token-by-token.
- **BYOK** — when `byok_cloud_config` returns `Some`, one non-streaming HTTP
  call via `core/cloud.rs` (Anthropic or OpenAI), the whole answer pushed
  through `on_token` at once.

BYOK requires the user to paste an API key and carry per-token billing. This
plan adds a `claude-code` engine that instead spawns the `claude` binary. It
honours whatever auth Claude Code already has — for a subscription login that
is the subscription's quota, with no key involved.

### Decisions locked with the user (2026-05-22)

1. **ADR / plan first.** Capture the design before any code (this document and
   ADR-0008).
2. **Streaming for v1.** Use `--output-format stream-json
   --include-partial-messages` and feed `on_token` incrementally, matching the
   local engine's feel — not the one-shot path the HTTP BYOK engine uses.

### Verified against Claude Code v2.1.144

The CLI behaviour below was confirmed by running the installed binary
(`~/.local/bin/claude`), not assumed:

- `claude auth status --json` → `{loggedIn, authMethod, apiProvider, email,
  subscriptionType}`; exit 0 when logged in. `authMethod: "claude.ai"` is a
  subscription login.
- `claude -p` JSON result carries `.result`, `.is_error`, `.subtype`,
  `.total_cost_usd`, `.usage`.
- `--tools ""` disables built-in tools but **not** MCP servers — those still
  load from the user's config unless `--strict-mcp-config` is also passed.
- `stream-json` emits the event shape tabled under M2.

---

## Architecture

```
chat_ask(query) ──► retrieval (vector + graph) ──► build_ask_prompt()
                                                          │
                                  resolve_answer_engine(app)
                                                          │
                 ┌────────────────────┬───────────────────┴────────────────┐
                 ▼                    ▼                                    ▼
          AnswerEngine::Local   AnswerEngine::Byok(CloudConfig)   AnswerEngine::ClaudeCode(cfg)
                 │                    │                                    │
        Ollama generate_stream   cloud::generate (HTTP,            claude_code::generate
        → on_token per token      one-shot) → on_token once         spawn `claude -p`,
                                                                    parse stream-json,
                                                                    → on_token per text_delta
```

`core/claude_code.rs` is new and self-contained. `core/cloud.rs` is untouched
and stays HTTP-only. Extraction (ADR-0006) is not touched — local always.

### Config model

`AppConfig` (`core/formation_state.rs`) gains:

| field               | type             | notes                                                  |
|---------------------|------------------|--------------------------------------------------------|
| `answer_engine`     | `Option<String>` | `None`/`"local"` → Ollama, `"byok"`, `"claude-code"`   |
| `claude_code_model` | `Option<String>` | model alias (`sonnet`/`opus`/`haiku`) or full id       |

The existing `byok_provider` / `byok_api_key` / `byok_model` fields stay; they
describe the HTTP BYOK engine only. **Back-compat:** when `answer_engine` is
`None` but `byok_provider` + a key are set, resolution treats the engine as
`"byok"` — existing BYOK users are not silently downgraded to local.

### The hardened invocation (M2)

Prompt text (the existing `build_ask_prompt` output) is written to the child's
**stdin**. Flags:

```
claude -p
  --system-prompt "<minimal answerer persona>"
  --output-format stream-json --include-partial-messages --verbose
  --tools ""
  --strict-mcp-config
  --disable-slash-commands
  --no-session-persistence
  --model <claude_code_model or default>
```

Spawned with a neutral working directory (e.g. a temp dir) so CLAUDE.md
auto-discovery and git-status injection cannot reach the user's files.
`--system-prompt` replaces Claude Code's large coding-agent system prompt,
which both stops agentic behaviour and cuts per-call token overhead.

---

## Milestones

### M0 — ADR-0008 + this plan + deps
- Write `docs/adr/0008-claude-code-answer-engine.md` and this plan. *(done)*
- Confirm `tokio` carries the `process` feature in `src-tauri/Cargo.toml` (for
  `tokio::process::Command`); add it if missing. No new crates are expected —
  `std`/`tokio` cover process spawning and `serde_json` is already a dep.
- **Verify:** `cargo check` clean.

### M1 — `core/claude_code.rs`: discovery + auth status
- New module with:
  - `locate() -> Option<PathBuf>` — resolve the `claude` binary. Check, in
    order: `~/.local/bin/claude`, `~/.claude/local/claude`,
    `/opt/homebrew/bin/claude`, `/usr/local/bin/claude`, then a login-shell
    probe (`zsh -lc 'command -v claude'`) since a macOS GUI app has a stripped
    PATH.
  - `struct ClaudeCodeStatus { installed, binary_path, logged_in, auth_method,
    subscription_type, email }`.
  - `async fn status(binary: &Path) -> ClaudeCodeStatus` — run
    `claude auth status --json`, parse the JSON, map fields.
- Register the module in `core/mod.rs`.
- **Verify:** unit tests — `auth status --json` parsing from a captured JSON
  string (logged-in and logged-out shapes); `locate()` path-precedence logic
  with a temp filesystem. No subprocess in the deterministic tests.

### M2 — streaming `generate()`
- `async fn generate(binary, model, prompt, on_token) -> AppResult<String>`:
  - Spawn the hardened invocation above; write `prompt` to stdin, close it.
  - Read stdout line-by-line; `serde_json::from_str` each line into a lenient
    event DTO. Forward text, accumulate the full answer, detect terminal state:

  | event line                                                        | action                                  |
  |--------------------------------------------------------------------|------------------------------------------|
  | `type:"stream_event"`, `event.type:"content_block_delta"`, `delta.type:"text_delta"` | push `delta.text` to `on_token` + buffer |
  | `delta.type:"thinking_delta"` / `"signature_delta"`                | ignore                                   |
  | `type:"rate_limit_event"`, `rate_limit_info.status != "allowed"`   | error: "Claude usage limit reached"      |
  | `type:"result"`, `is_error:true`                                   | error with `subtype`                     |
  | `type:"result"`, `is_error:false`                                  | return `.result` (authoritative answer)  |
  | `type:"system"` / `assistant` / `message_*` / `content_block_*`    | ignore                                   |

  - Non-zero exit with no `result` line → error carrying stderr (an
    unauthenticated CLI fails here; surface "run `claude` and sign in").
- **Verify:** a deterministic fixture test — feed the parser the real captured
  `stream-json` transcript (counting 1–5, with a `thinking` block before the
  `text` block) and assert it yields the text tokens only and the final answer.
  A `#[ignore]`d live test runs the real binary, mirroring ADR-0006's Layer 2.

### M3 — config + engine resolution
- Add `answer_engine` and `claude_code_model` to `AppConfig`
  (`#[serde(default)]` for back-compat with existing `config.json`).
- New `enum AnswerEngine { Local, Byok(CloudConfig), ClaudeCode(ClaudeCodeConfig) }`
  and `resolve_answer_engine(app) -> AnswerEngine` in `commands/chat.rs`,
  replacing the inline `byok_cloud_config` call. Encodes the back-compat rule
  (unset `answer_engine` + configured BYOK → `Byok`).
- **Verify:** unit tests for `resolve_answer_engine` across the matrix —
  unset/`local`/`byok`/`claude-code`, plus the legacy BYOK-without-`answer_engine`
  case.

### M4 — `chat_ask` wiring
- Replace the two-arm `match byok_cloud_config(...)` at
  [chat.rs:456](../../src-tauri/src/commands/chat.rs) with a three-arm
  `match resolve_answer_engine(&app)`. The `ClaudeCode` arm calls
  `claude_code::generate`, passing the existing `on_token` channel straight
  through; the persisted `answer` is the accumulated/`result` text.
- **Verify:** `cargo test` green; `cargo clippy` clean.

### M5 — Tauri commands
- `detect_claude_code() -> ClaudeCodeStatus` — `locate()` then `status()`, for
  the settings/onboarding UI.
- `set_answer_engine(engine, claude_code_model)` — persists to `AppConfig`;
  reject `"claude-code"` when the binary is not found or not logged in.
- Register both in `commands/mod.rs` and `lib.rs`'s `invoke_handler`.
- **Verify:** `cargo check`; invoke `detect_claude_code` from the dev console
  and confirm it reports this machine's logged-in Max subscription.

### M6 — frontend + polish
- `SettingsModal.tsx` — an engine picker (Local / BYOK / Claude Code). The
  Claude Code option calls `detect_claude_code` and shows one of: "Not
  installed", "Installed — run `claude` in a terminal to sign in", or
  "Connected as <email> — <subscription> subscription", plus a model field and
  a note that answers draw on the user's Claude usage allowance.
- `tauri.ts` wrappers + types for the new commands.
- Optionally surface the engine in `Onboarding.tsx` for users who already have
  Claude Code.
- Update `README.md` (project layout + a verification note).
- **Verify:** `npm run tauri dev` — pick the Claude Code engine, ask a
  question, confirm the answer streams in token-by-token and is cited; build +
  lint clean per the repo's completion bar.

---

## Open questions / out of scope

- **`--verbose` necessity.** Some `claude` versions require `--verbose` with
  `stream-json` in `-p` mode; v2.1.144 accepts it and the extra `system` events
  are ignored by the parser. M2 keeps the flag for safety.
- **Settings isolation.** `--setting-sources ""` (load no user/project/local
  settings) is optional hardening — verify it is accepted before relying on it;
  with `--tools ""` + `--strict-mcp-config` + `--system-prompt` the settings
  surface is already small.
- **Per-turn cost cap.** `--max-budget-usd` only matters for an API-key-authed
  Claude Code install (a subscription spends quota, not dollars) — out of scope.
- **Extraction via Claude Code** — stays local per ADR-0006; out of scope.
- **Claude Agent SDK** instead of the raw CLI — heavier integration, needs a
  token/key, does not transparently reuse the subscription login. Out of scope.
- **Bundling Claude Code** — the engine is offered only when an existing install
  is detected; Sediment does not install it.
- **Windows / Linux PATHs** — `locate()` focuses on macOS install paths first;
  other platforms fall back to the login-shell probe.

---

## Test strategy

Follows the repo convention — the deterministic half is the CI gate. The
`stream-json` parser (M2) is tested against a real captured transcript;
`locate()` and `auth status` parsing (M1) and `resolve_answer_engine` (M3) are
plain unit tests. The only model/binary-dependent test is the `#[ignore]`d live
`generate()` call, matching ADR-0006's Layer 2. The frontend path (M6) is
verified manually via `npm run tauri dev`.
