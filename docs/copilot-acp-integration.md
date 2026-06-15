# GitHub Copilot CLI — ACP integration spec (M8 reference)

The implementation blueprint for the **warm** Copilot engine (ADR-0012 §2),
produced by the M8 ACP spike (2026-06-15) against `@github/copilot` **1.0.62**,
ACP **protocolVersion 1**. Every claim was verified against a captured `copilot
--acp` session or a cited doc. This is the durable record of that spike (the raw
captures were in `/tmp`).

Copilot's warm path is the **Agent Client Protocol** (ACP) — a bidirectional
JSON-RPC 2.0 server over stdio (`copilot --acp`). One long-lived process holds one
session and serves many prompts; that is the resident model the engine uses.

## Spawn command

```
copilot --acp \
  --disable-builtin-mcps \                       # suppress the built-in github MCP
  --allow-all-tools \                            # unattended: suppress permission prompts
  --add-dir <formation_dir> \                    # fs access (repeatable)
  --additional-mcp-config @<abs path to mcp-config.json> \   # STDIO MCP servers (see below)
  --model <gpt-5-mini | claude-haiku-4.5 | auto>
```

- `--allow-all-tools` (or env `COPILOT_ALLOW_ALL=1`) is the unattended path. **Still
  implement the auto-approver** (below) as defense-in-depth — a shell call fired a
  permission request when the flag was absent.
- Pipe stdin/stdout/stderr. Parse stdout line-by-line (NDJSON); drain stderr to a
  log sink — **do not** treat it as protocol, and note Copilot logs healthy events
  at `[ERROR]` level. Rely on `stopReason` / JSON-RPC `error`, never stderr severity.

## Framing

**NDJSON** — one JSON-RPC 2.0 message per `\n`-terminated line, UTF-8, no embedded
newlines (so never pretty-print). Not LSP `Content-Length`. Write
`to_string(&msg) + "\n"`; read with `lines()`.

## Message sequence for one turn

**1. `initialize` →**
```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{
  "protocolVersion":1,
  "clientCapabilities":{"fs":{"readTextFile":true,"writeTextFile":true},"terminal":false},
  "clientInfo":{"name":"sediment","version":"0.1.0"}}}
```
Response: `{protocolVersion:1, agentCapabilities:{loadSession:true, mcpCapabilities:{http,sse}, …}, agentInfo:{name:"Copilot",version}, authMethods:[…]}`. **`mcpCapabilities` lists only http+sse — no stdio** (the tell for the MCP gotcha). If not logged in, `authMethods` says to run `copilot login`.

**2. `session/new` →** (cwd = formation; STDIO MCP goes in the config file, NOT here)
```json
{"jsonrpc":"2.0","id":2,"method":"session/new","params":{
  "cwd":"/abs/formation","mcpServers":[]}}
```
Response carries `sessionId` plus `models`, `modes`, `configOptions` (incl. `allow_all`) — parse `sessionId`, ignore the rest.

**3. `session/prompt` →**
```json
{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{
  "sessionId":"<from session/new>",
  "prompt":[{"type":"text","text":"<message>"}]}}
```
Streams `session/update` notifications, then resolves `{stopReason}`
(`end_turn | max_tokens | max_turn_requests | refusal | cancelled`).

## Response-event parsing map

Dispatch each incoming line **by structure, not id** (critical — see gotcha):

| Incoming | Action |
|---|---|
| `result` to your `session/prompt` id → `{stopReason}` | **Turn complete** |
| `session/update` · `agent_message_chunk` → `update.content.text` | **append to reply** → `TextDelta` |
| `session/update` · `agent_thought_chunk` | thinking — **ignore** |
| `session/update` · `tool_call` → `{toolCallId,title,kind,status,rawInput}` | tool started → `ToolActivity` |
| `session/update` · `tool_call_update` → `{toolCallId,status,content,rawOutput}` | tool progress/done |
| `session/update` · `plan` / `available_commands_update` / `config_option_update` | **ignore** |
| **request** `session/request_permission` (has `method` AND `id`) | **auto-approve** (below) |
| request `fs/read_text_file` / `fs/write_text_file` | honor or stub per fs policy |

**Auto-approve** reply to `session/request_permission`:
```json
{"jsonrpc":"2.0","id":<THEIR id>,"result":{"outcome":{"outcome":"selected","optionId":"allow_always"}}}
```
(pick the `allow_always` or `allow_once` option from `params.options`).

## MCP server injection (key finding)

- **STDIO MCP via the ACP `session/new.mcpServers` param is silently dropped** —
  `session/new` succeeds but the log says `Rejecting non-http/sse MCP server`. Only
  http/sse work through ACP.
- **STDIO MCP MUST use the `--additional-mcp-config @<file>` flag.** The file:
  ```json
  {"mcpServers":{"sediment":{
    "type":"local","command":"/abs/sediment","args":["--mcp-stdio"],
    "env":{"SEDIMENT_FORMATION":"/abs/formation","SEDIMENT_SOURCE_CHAT_ID":"chat_message:…"},
    "tools":["*"]}}}
  ```
  `type` is `"local"` (= stdio); `env` is an **object** (the ACP param form uses an
  array — different). `--disable-builtin-mcps` removes the built-in github MCP.

## Behaviour (system) prompt — no `--system-prompt` flag

Copilot loads custom instructions from **cwd**: `AGENTS.md`,
`.github/copilot-instructions.md`. Two options for Sediment's behaviour prompt:
- **Prompt-prefix (chosen for V1):** prepend the persona as a `{type:"text"}` block
  on the **first** `session/prompt` of a session (the warm session retains it
  server-side, so later turns send only the user message + that turn's grounding —
  no need to re-send persona or history). Avoids polluting the user's formation.
- AGENTS.md in cwd (loaded once, but writes into the user's formation; use
  `--no-custom-instructions` to opt out of all instruction loading).

## Resident model + recycle

- **One process, one session, many prompts.** Keep the child alive; reuse the
  single `sessionId`; issue sequential `session/prompt`s. History is retained
  server-side within the session (this is the warm win — Sediment need not resend
  its recent window each turn).
- **Serialize turns** — one in-flight prompt per session; send the next only after
  the prior resolves (or `session/cancel`).
- **Clean shutdown:** close the child's stdin (EOF) → `Received EOF on stdin,
  shutting down`; SIGTERM as backstop. **Recycle** (reset context or dodge the
  #2755 degradation): `session/new` again on the same process, or kill+respawn.
- **Hung turn:** `session/cancel {sessionId}`; if unresolved, kill.

## Gotchas

- **Server→client request ids start at 0 and collide with client ids.** Dispatch
  by message shape: `{method,id}` = request *to us* (reply with *their* id);
  `{result|error,id}` = response to *our* request (match our pending map);
  `{method}` no id = notification.
- STDIO MCP via ACP param is dropped (use the flag); `env` shape differs (object in
  file, array in ACP param).
- Built-in github MCP auto-loads unless `--disable-builtin-mcps`.
- Skills/slash-commands are injected (`available_commands_update`); ignore the
  notification; `--no-custom-instructions` stops AGENTS.md/skill loading.
- Permission requests block unattended runs without `--allow-all-tools` — pass it
  **and** keep the auto-approver.
- Copilot logs healthy events at `[ERROR]`; ignore stderr severity.

## Unverified / to confirm during build
- That `--allow-all-tools` fully suppresses `session/request_permission`
  (documented + implied; keep the auto-approver regardless).
- `session/cancel` and `session/load` behavior with Copilot (capability advertised;
  not exercised).
- `agent_thought_chunk` exact Copilot shape (from the ACP spec, not a capture —
  `gpt-5-mini` didn't emit one on trivial prompts).
