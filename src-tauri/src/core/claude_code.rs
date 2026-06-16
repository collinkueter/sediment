//! The Claude Code CLI as the conversational-agent engine — ADR-0009 §5.
//!
//! Many Sediment users already pay for a Claude Pro or Max subscription. That
//! entitlement has no public API a third-party app can call directly, but any
//! user who has installed the Claude Code CLI has already authenticated it
//! against that subscription. ADR-0009 spawns `claude` *as an agent* — the
//! reverse of ADR-0008's hardened tool-less answerer — so it reads and writes
//! the formation's notes with its own native file tools and reaches the
//! bi-temporal graph through Sediment's stdio MCP server.
//!
//! ## Scope
//!
//! This module owns three things:
//!
//! 1. **Binary discovery** (`locate`) — a macOS GUI app does not inherit the
//!    user's shell `PATH`, so we check the known install locations directly
//!    and fall back to a login-shell probe.
//! 2. **Auth detection** (`detect`) — runs `claude auth status --json` to
//!    learn whether the found binary is logged in, and to surface the
//!    `authMethod` and `subscriptionType` for the settings UI.
//! 3. **The agent engine** ([`ClaudeCodeEngine`]) — a [`ConversationEngine`]
//!    implementation that spawns `claude` per turn with the formation as its
//!    working directory, native file tools enabled (Bash off), the `sediment`
//!    graph-only MCP server, and the versioned behaviour prompt.
//!
//! ## Streaming protocol
//!
//! `--output-format stream-json --include-partial-messages` emits
//! newline-delimited JSON. `content_block_delta` events with
//! `delta.type == "text_delta"` are forwarded as reply text; `tool_use` blocks
//! surface as conversational tool activity. The terminal `{"type":"result"}`
//! line carries the authoritative full answer and the `is_error` / `subtype`
//! outcome. A `rate_limit_event` with a non-`"allowed"` status surfaces a
//! quota-exhausted error rather than an opaque one.

use crate::core::conversation::{
    ConversationEngine, TurnEvent, TurnEventSink, TurnOutcome, TurnRequest,
};
use crate::error::{AppError, AppResult};
use async_trait::async_trait;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::process::Command;

/// Default model alias when the user has not chosen one.
///
/// The full model ID (`claude-sonnet-4-5`, etc.) would pin to a specific
/// generation; the alias always resolves to the current recommended Sonnet
/// and keeps working across model roll-overs.
pub const DEFAULT_MODEL: &str = "sonnet";

// The conversational-agent surface below (`ClaudeCodeEngine`, the turn
// constants, and the helper fns) is exercised by this module's tests and
// driven in production by `commands::chat::chat_turn`.

/// Wall-clock cap on a single `claude` agent turn (ADR-0009, resolved during
/// planning). A safety net against a hung subprocess — not a per-tool budget.
/// On expiry the child is killed and `run_turn` returns an error.
const TURN_TIMEOUT: Duration = Duration::from_secs(300);

/// The agent behaviour prompt (ADR-0009 §8) — the persona, the questioning
/// discipline, and the recommended section vocabulary. A first-class versioned
/// artifact in the repo, embedded at compile time rather than a string literal.
const CONVERSATION_AGENT_PROMPT: &str = include_str!("../../../prompts/conversation-agent.md");

/// Native Claude Code tools the conversational agent is allowed to use.
///
/// ADR-0009 §5: the agent reads and writes **notes** with Claude Code's own
/// file tools (Read, Edit, Write, Grep, Glob). Bash is deliberately absent —
/// the agent is a note-keeper, not a coding agent with shell access.
const NATIVE_FILE_TOOLS: &str = "Read,Edit,Write,Grep,Glob";

// ──────────────────────────────────────────────────────────────────────────────
// Public status struct
// ──────────────────────────────────────────────────────────────────────────────

/// Install and authentication status of the local Claude Code CLI, surfaced
/// to the UI so the settings screen can show "Not installed", "Sign in
/// required", or "Connected as <email> — <plan> subscription".
#[derive(Debug, Clone, serde::Serialize)]
pub struct ClaudeCodeStatus {
    /// `true` when a `claude` binary was found at a known path or via the
    /// login-shell probe.
    pub installed: bool,
    /// Absolute path to the located binary, when `installed` is `true`.
    pub binary_path: Option<String>,
    /// `true` when `claude auth status --json` reports `loggedIn: true`.
    pub logged_in: bool,
    /// The `authMethod` field from `auth status` (e.g. `"claude.ai"` for a
    /// subscription login, `"apiKey"` for a key-authed install).
    pub auth_method: Option<String>,
    /// The `subscriptionType` from `auth status` (e.g. `"max"`, `"pro"`).
    pub subscription_type: Option<String>,
    /// The `email` of the signed-in account, when available.
    pub email: Option<String>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Binary discovery
// ──────────────────────────────────────────────────────────────────────────────

/// Resolve the path to the `claude` binary.
///
/// A macOS GUI app launched from the Finder or the Dock does not inherit
/// the user's shell `PATH`, so `Command::new("claude")` would fail for most
/// installs. We probe absolute paths first (fastest, most reliable) and fall
/// back to a login shell, which sources the user's `.zshrc`/`.bash_profile`
/// where the binary may be on PATH.
///
/// Priority order (first existing file wins):
/// 1. `$HOME/.local/bin/claude` — native installer default
/// 2. `$HOME/.claude/local/claude` — alternate native install path
/// 3. `/opt/homebrew/bin/claude` — Apple-Silicon Homebrew
/// 4. `/usr/local/bin/claude` — Intel Homebrew / manual
/// 5. Login-shell probe: `zsh -lc "command -v claude"` (uses `$SHELL` if set)
pub fn locate() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok();

    let mut candidates: Vec<PathBuf> = Vec::with_capacity(4);
    if let Some(ref h) = home {
        candidates.push(PathBuf::from(h).join(".local/bin/claude"));
        candidates.push(PathBuf::from(h).join(".claude/local/claude"));
    }
    candidates.push(PathBuf::from("/opt/homebrew/bin/claude"));
    candidates.push(PathBuf::from("/usr/local/bin/claude"));

    if let Some(path) = first_existing(&candidates) {
        return Some(path);
    }

    // Fall back to a login shell, which sources the user's shell initialisation
    // files (`.zshrc`, `.bash_profile`, etc.) where the binary may appear on
    // PATH via `nvm`, `mise`, or a custom `export PATH=…` line.
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let output = std::process::Command::new(&shell)
        .args(["-lc", "command -v claude"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;

    if output.status.success() {
        let raw = String::from_utf8_lossy(&output.stdout);
        let resolved = raw.trim();
        if !resolved.is_empty() {
            let p = PathBuf::from(resolved);
            if p.is_file() {
                return Some(p);
            }
        }
    }

    None
}

/// Return the first `PathBuf` in `candidates` that exists on the filesystem
/// as a regular file (not a directory).
///
/// Factored out to be independently unit-testable — the production code
/// constructs candidates; tests create temp files at chosen positions.
fn first_existing(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|p| p.is_file()).cloned()
}

// ──────────────────────────────────────────────────────────────────────────────
// Auth detection
// ──────────────────────────────────────────────────────────────────────────────

/// Detect whether the locally-installed Claude Code CLI is authenticated.
///
/// When `locate()` returns `None` every field is false/None. Otherwise
/// `claude auth status --json` is spawned; any parse failure or non-zero exit
/// is treated as "installed but not logged in" rather than an error, because
/// the install is real — only the login state is uncertain.
///
/// This call makes no network request and produces no generation tokens.
pub async fn detect() -> ClaudeCodeStatus {
    let binary = match locate() {
        Some(b) => b,
        None => {
            return ClaudeCodeStatus {
                installed: false,
                binary_path: None,
                logged_in: false,
                auth_method: None,
                subscription_type: None,
                email: None,
            }
        }
    };

    let binary_path_str = binary.to_string_lossy().to_string();

    // Spawn `claude auth status --json`. We ignore errors here — a non-zero
    // exit (e.g. the user is logged out) is handled by `parse_auth_status`.
    let output = tokio::process::Command::new(&binary)
        .args(["auth", "status", "--json"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .await;

    let json = match output {
        Ok(ref o) => String::from_utf8_lossy(&o.stdout).to_string(),
        Err(_) => String::new(),
    };

    let auth = parse_auth_status(&json);

    ClaudeCodeStatus {
        installed: true,
        binary_path: Some(binary_path_str),
        logged_in: auth.logged_in,
        auth_method: auth.auth_method,
        subscription_type: auth.subscription_type,
        email: auth.email,
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Auth JSON parsing (pure, unit-testable)
// ──────────────────────────────────────────────────────────────────────────────

/// Parsed subset of `claude auth status --json` — only the fields Sediment
/// cares about. `#[serde(default)]` on every field means a minimal
/// `{"loggedIn":false}` response (or total parse failure) gracefully yields
/// an all-false, all-None struct.
#[derive(Debug, Default, Deserialize)]
struct AuthStatusJson {
    #[serde(rename = "loggedIn", default)]
    logged_in: bool,
    #[serde(rename = "authMethod", default)]
    auth_method: Option<String>,
    #[serde(rename = "subscriptionType", default)]
    subscription_type: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

/// Parsed auth fields extracted from a `claude auth status --json` stdout
/// snippet. Used as the return type of the pure `parse_auth_status` helper
/// so it can be unit-tested without a subprocess.
struct ParsedAuth {
    logged_in: bool,
    auth_method: Option<String>,
    subscription_type: Option<String>,
    email: Option<String>,
}

/// Parse the stdout of `claude auth status --json` into a `ParsedAuth`.
///
/// Any JSON parse failure or missing field is tolerated — the result is
/// simply `logged_in: false` with `None` for all optional fields. This
/// keeps the detection path resilient to version changes in the CLI.
fn parse_auth_status(json: &str) -> ParsedAuth {
    let parsed: AuthStatusJson = serde_json::from_str(json).unwrap_or_default();
    ParsedAuth {
        logged_in: parsed.logged_in,
        auth_method: parsed.auth_method,
        subscription_type: parsed.subscription_type,
        email: parsed.email,
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Stream-JSON event parsing (pure, unit-testable)
// ──────────────────────────────────────────────────────────────────────────────

/// The caller-visible meaning of a single `stream-json` line.
#[derive(Debug, PartialEq)]
enum StreamLine {
    /// A text token to forward to the UI and append to the accumulator.
    Token(String),
    /// One or more `tool_use` content blocks the agent invoked, parsed from a
    /// (non-partial) `assistant` message line. Carries the complete, settled
    /// tool calls — `name` plus the fully-formed `input` arguments.
    ///
    /// `tool_use` is read from the `assistant` message rather than the partial
    /// `content_block_start`/`input_json_delta` stream events because only the
    /// settled message carries a complete `input` object — the partial events
    /// start with `input:{}` and dribble the arguments in as JSON fragments.
    ToolUses(Vec<ToolUseCall>),
    /// The terminal `result` line — carries the full answer and error flag.
    Done {
        answer: String,
        is_error: bool,
        subtype: Option<String>,
    },
    /// A `rate_limit_event` whose `status` was not `"allowed"`.
    RateLimited,
    /// Anything else (system init, content_block_start, message_start,
    /// thinking_delta, signature_delta, …) — safe to discard.
    Other,
}

/// A single settled `tool_use` content block: the tool name and its complete
/// argument object.
#[derive(Debug, PartialEq, Clone)]
struct ToolUseCall {
    /// The tool name — a native tool (`Read`, `Edit`, …) or an MCP tool
    /// (`mcp__sediment__record_fact`).
    name: String,
    /// The fully-formed argument object the agent passed to the tool.
    input: serde_json::Value,
}

// ── Deserialise helpers (lenient, all fields optional) ─────────────────────

/// Top-level envelope for every `stream-json` line.
#[derive(Deserialize, Default)]
struct StreamEnvelope {
    #[serde(rename = "type", default)]
    kind: String,
    // For stream_event lines:
    #[serde(default)]
    event: Option<StreamEvent>,
    // For rate_limit_event lines:
    #[serde(default)]
    rate_limit_info: Option<RateLimitInfo>,
    // For assistant message lines (settled tool_use blocks live here):
    #[serde(default)]
    message: Option<AssistantMessage>,
    // For result lines:
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    result: Option<String>,
}

/// The `message` payload of an `assistant` line — only its `content` blocks
/// matter to us, and only the `tool_use` ones among those.
#[derive(Deserialize, Default)]
struct AssistantMessage {
    #[serde(default)]
    content: Vec<ContentBlock>,
}

/// One content block inside an assistant message. `text`/`thinking` blocks are
/// ignored here (text streams via `content_block_delta` instead); only
/// `tool_use` blocks are extracted.
#[derive(Deserialize, Default)]
struct ContentBlock {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Option<serde_json::Value>,
}

#[derive(Deserialize, Default)]
struct StreamEvent {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    delta: Option<Delta>,
}

#[derive(Deserialize, Default)]
struct Delta {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize, Default)]
struct RateLimitInfo {
    #[serde(default)]
    status: String,
}

/// Classify a single newline-delimited `stream-json` event.
///
/// This is a pure function — it parses `line` and returns a `StreamLine`
/// variant. The `generate` driver calls it for every stdout line and acts
/// on the result without any business logic of its own. The separation
/// makes the stream protocol independently testable via captured transcripts.
///
/// A line that fails JSON parsing always yields `Other` — we never want
/// one malformed line to abort an otherwise-valid generation.
fn parse_stream_line(line: &str) -> StreamLine {
    let env: StreamEnvelope = match serde_json::from_str(line) {
        Ok(e) => e,
        Err(_) => return StreamLine::Other,
    };

    match env.kind.as_str() {
        "stream_event" => {
            let event = match &env.event {
                Some(e) => e,
                None => return StreamLine::Other,
            };
            // Only content_block_delta / text_delta reaches the caller.
            // thinking_delta and signature_delta are model-internal and must
            // not be sent to the UI.
            if event.kind == "content_block_delta" {
                if let Some(delta) = &event.delta {
                    if delta.kind == "text_delta" {
                        let text = delta.text.clone().unwrap_or_default();
                        return StreamLine::Token(text);
                    }
                    // thinking_delta / signature_delta → Other
                }
            }
            StreamLine::Other
        }

        "rate_limit_event" => {
            let allowed = env
                .rate_limit_info
                .as_ref()
                .map(|r| r.status.as_str() == "allowed")
                .unwrap_or(false);
            if allowed {
                StreamLine::Other
            } else {
                StreamLine::RateLimited
            }
        }

        "assistant" => {
            // A settled assistant message. Extract any `tool_use` content
            // blocks; ignore `text`/`thinking` blocks (text is streamed
            // separately via content_block_delta). A message with no tool_use
            // blocks → Other.
            let calls: Vec<ToolUseCall> = env
                .message
                .map(|m| m.content)
                .unwrap_or_default()
                .into_iter()
                .filter(|b| b.kind == "tool_use")
                .filter_map(|b| {
                    b.name.map(|name| ToolUseCall {
                        name,
                        input: b.input.unwrap_or(serde_json::Value::Null),
                    })
                })
                .collect();
            if calls.is_empty() {
                StreamLine::Other
            } else {
                StreamLine::ToolUses(calls)
            }
        }

        "result" => StreamLine::Done {
            answer: env.result.unwrap_or_default(),
            is_error: env.is_error,
            subtype: env.subtype,
        },

        // "system", "user", and any future types we don't recognise.
        _ => StreamLine::Other,
    }
}

/// Render a settled [`ToolUseCall`] into a short human phrase for the UI
/// activity trail.
///
/// Pure and unit-testable. The phrasing is intentionally terse — the tool name
/// plus the single argument that best identifies *what* the call touched
/// (`Read /…/Josh.md`, `record_fact works_at`, `search_notes "roadmap"`). When
/// no obvious key argument exists the bare tool name is used.
fn summarize_tool_call(call: &ToolUseCall) -> String {
    // The MCP tools are namespaced `mcp__sediment__<tool>`; show just the
    // trailing tool name so the trail reads cleanly.
    let display_name = call
        .name
        .rsplit("__")
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(&call.name);

    let obj = call.input.as_object();
    // Probe a small set of argument keys in priority order — the first that is
    // present and string-ish becomes the phrase's detail.
    let detail = obj.and_then(|o| {
        for key in [
            "file_path",
            "path",
            "query",
            "name",
            "subject",
            "title",
            "pattern",
            "fact_id",
        ] {
            if let Some(v) = o.get(key) {
                if let Some(s) = v.as_str() {
                    if !s.is_empty() {
                        return Some(s.to_string());
                    }
                }
            }
        }
        None
    });

    match detail {
        Some(d) => format!("{display_name} {d}"),
        None => display_name.to_string(),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// The conversational agent engine — ADR-0009 §5
// ──────────────────────────────────────────────────────────────────────────────

/// The Claude Code [`ConversationEngine`] — spawns `claude` as an *agent*.
///
/// This is the reverse of [`generate`] (ADR-0008's hardened, tool-less plain
/// answerer). Each turn spawns the user's `claude` binary with:
///
/// - `current_dir` = the formation root, so Claude Code's **native file tools**
///   (Read, Edit, Write, Grep, Glob) read and write the formation's notes.
/// - `--mcp-config` pointing at a generated config that launches *this binary*
///   with `--mcp-stdio` — the M2 graph-only MCP server (`sediment` server) —
///   plus `--strict-mcp-config` so the user's own MCP servers do not load.
/// - `--allowedTools` whitelisting exactly the file tools and the `sediment`
///   MCP server; `--disallowedTools Bash` as a belt-and-braces shell lockout.
/// - `--system-prompt` = the versioned behaviour prompt (ADR-0009 §8).
///
/// Sediment owns the transcript (ADR-0009 §5): the prior turns travel in the
/// [`TurnRequest`] and are rendered into the stdin prompt, not resumed from a
/// Claude Code session file (`--no-session-persistence`).
pub struct ClaudeCodeEngine {
    /// The model alias or full id to run the agent on (`sonnet`, `opus`, …).
    pub model: String,
}

impl ClaudeCodeEngine {
    /// Construct an engine for the given model.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
        }
    }
}

impl Default for ClaudeCodeEngine {
    fn default() -> Self {
        Self::new(DEFAULT_MODEL)
    }
}

/// Render the formation path, the recent-window transcript, and the new message
/// into the single stdin prompt the `claude` agent receives.
///
/// Pure and unit-testable. The prompt always opens with the formation's
/// absolute path — the agent must read and write notes there and nowhere else.
/// Without it a bare `People/X.md` is resolved against an arbitrary directory
/// (a home-directory escape was observed in testing). Prior turns are labelled
/// `User:` / `Assistant:`; the new message is the final block.
fn render_turn_prompt(turn: &TurnRequest) -> String {
    let mut out = String::new();
    out.push_str("# Your formation\n\n");
    out.push_str(
        "Every note lives inside this folder. Read and write notes only here, \
         using absolute paths under it:\n",
    );
    out.push_str(turn.formation_root.to_string_lossy().trim());
    out.push_str("\n\n");
    if let Some(ctx) = turn.injected_context.as_deref() {
        let ctx = ctx.trim();
        if !ctx.is_empty() {
            out.push_str("# What you already know\n\n");
            out.push_str(ctx);
            out.push_str("\n\n");
        }
    }
    if !turn.history.is_empty() {
        out.push_str("# Conversation so far\n\n");
        for t in &turn.history {
            let label = match t.role.as_str() {
                "assistant" => "Assistant",
                _ => "User",
            };
            out.push_str(label);
            out.push_str(": ");
            out.push_str(t.content.trim());
            out.push_str("\n\n");
        }
    }
    out.push_str("# New message\n\n");
    out.push_str(turn.message.trim());
    out
}

/// Build the `--mcp-config` JSON that makes Claude Code spawn the M2 graph-only
/// MCP server.
///
/// The server is *this same binary* re-invoked with `--mcp-stdio` (see
/// `lib::run_mcp_stdio`); it reads the formation root and provenance chat id
/// from the env vars set here. Pure so it can be asserted in a unit test.
fn mcp_config_json(self_exe: &Path, turn: &TurnRequest) -> String {
    let cfg = serde_json::json!({
        "mcpServers": {
            "sediment": {
                "command": self_exe.to_string_lossy(),
                "args": ["--mcp-stdio"],
                "env": {
                    "SEDIMENT_FORMATION": turn.formation_root.to_string_lossy(),
                    "SEDIMENT_SOURCE_CHAT_ID": turn.source_chat_id,
                }
            }
        }
    });
    // `to_string` on a json! value never fails.
    serde_json::to_string(&cfg).unwrap_or_else(|_| "{}".to_string())
}

#[async_trait]
impl ConversationEngine for ClaudeCodeEngine {
    async fn run_turn(
        &self,
        turn: &TurnRequest,
        on_event: &TurnEventSink,
    ) -> AppResult<TurnOutcome> {
        let binary = locate().ok_or_else(|| {
            AppError::other(
                "Claude Code is not installed. Install it from https://claude.com/claude-code.",
            )
        })?;

        // The MCP server is this very binary re-invoked with `--mcp-stdio`.
        let self_exe = std::env::current_exe()
            .map_err(|e| AppError::other(format!("locate own executable: {e}")))?;

        // Write the MCP config to a temp file. A NamedTempFile-style manual
        // temp path keeps the dependency surface small; it is removed in a
        // best-effort cleanup once the turn finishes.
        let mcp_config_path =
            std::env::temp_dir().join(format!("sediment-mcp-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&mcp_config_path, mcp_config_json(&self_exe, turn))
            .map_err(|e| AppError::other(format!("write MCP config: {e}")))?;

        // `--allowedTools` admits the native file tools AND the whole `sediment`
        // MCP server (the `mcp__sediment` prefix covers every tool it exposes);
        // `--disallowedTools Bash` is an explicit shell lockout on top of the
        // allowlist. Bash is never in the allowlist, so this is belt-and-braces.
        let allowed_tools = format!("{NATIVE_FILE_TOOLS},mcp__sediment");

        let prompt = render_turn_prompt(turn);

        let spawn_result = Command::new(&binary)
            .args([
                "-p",
                "--system-prompt",
                CONVERSATION_AGENT_PROMPT,
                "--output-format",
                "stream-json",
                "--include-partial-messages",
                "--verbose",
                "--mcp-config",
                &mcp_config_path.to_string_lossy(),
                "--strict-mcp-config",
                "--allowedTools",
                &allowed_tools,
                "--disallowedTools",
                "Bash",
                "--permission-mode",
                "acceptEdits",
                "--no-session-persistence",
                "--model",
                &self.model,
            ])
            .current_dir(&turn.formation_root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn();

        let mut child: Child = match spawn_result {
            Ok(c) => c,
            Err(e) => {
                let _ = std::fs::remove_file(&mcp_config_path);
                return Err(AppError::other(format!("spawn claude: {e}")));
            }
        };

        // Run the whole turn under a wall-clock cap. On expiry the child is
        // killed; the temp config is cleaned up on every exit path below.
        let outcome =
            tokio::time::timeout(TURN_TIMEOUT, drive_turn(&mut child, &prompt, on_event)).await;

        let _ = std::fs::remove_file(&mcp_config_path);

        match outcome {
            Ok(result) => result,
            Err(_elapsed) => {
                // Timed out — kill the child so it does not linger.
                let _ = child.kill().await;
                Err(AppError::other(format!(
                    "Claude Code did not finish within {}s — the turn was cancelled.",
                    TURN_TIMEOUT.as_secs()
                )))
            }
        }
    }
}

/// Drive one spawned `claude` agent turn to completion: write the prompt to
/// stdin, stream stdout, classify every line, forward events, and resolve the
/// outcome. Factored out of `run_turn` so the whole body can be wrapped in a
/// single `tokio::time::timeout`.
async fn drive_turn(
    child: &mut Child,
    prompt: &str,
    on_event: &TurnEventSink,
) -> AppResult<TurnOutcome> {
    // Write the prompt to stdin and close it (EOF).
    {
        use tokio::io::AsyncWriteExt;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| AppError::other("could not get child stdin"))?;
        stdin
            .write_all(prompt.as_bytes())
            .await
            .map_err(|e| AppError::other(format!("write stdin: {e}")))?;
    }

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::other("could not get child stdout"))?;
    let stderr_handle = child
        .stderr
        .take()
        .ok_or_else(|| AppError::other("could not get child stderr"))?;

    // ── Stream stdout ─────────────────────────────────────────────────────
    let mut lines = BufReader::new(stdout).lines();
    let mut accumulator = String::new();
    let mut done: Option<(String, bool, Option<String>)> = None;
    let mut rate_limited = false;

    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|e| AppError::other(format!("read stdout: {e}")))?
    {
        match parse_stream_line(&line) {
            StreamLine::Token(t) => {
                accumulator.push_str(&t);
                on_event(TurnEvent::TextDelta { text: t });
            }
            StreamLine::ToolUses(calls) => {
                for call in calls {
                    let summary = summarize_tool_call(&call);
                    on_event(TurnEvent::ToolActivity {
                        tool: call.name,
                        summary,
                    });
                }
            }
            StreamLine::Done {
                answer,
                is_error,
                subtype,
            } => {
                done = Some((answer, is_error, subtype));
            }
            StreamLine::RateLimited => {
                rate_limited = true;
            }
            StreamLine::Other => {}
        }
    }

    // ── Drain stderr ──────────────────────────────────────────────────────
    let mut stderr_text = String::new();
    {
        use tokio::io::AsyncReadExt;
        let mut stderr_reader = BufReader::new(stderr_handle);
        stderr_reader
            .read_to_string(&mut stderr_text)
            .await
            .unwrap_or(0);
    }

    let exit_status = child
        .wait()
        .await
        .map_err(|e| AppError::other(format!("wait for claude: {e}")))?;

    // ── Resolve outcome ───────────────────────────────────────────────────
    match done {
        Some((answer, false, _subtype)) => {
            let reply = if !answer.is_empty() {
                answer
            } else if !accumulator.is_empty() {
                accumulator
            } else {
                return Err(AppError::other(
                    "Claude Code returned an empty reply — the turn may have been blocked.",
                ));
            };
            Ok(TurnOutcome { reply })
        }
        Some((_answer, true, subtype)) => Err(AppError::other(format!(
            "Claude Code reported an error during the turn (subtype: {}).",
            subtype.as_deref().unwrap_or("unknown")
        ))),
        None => {
            if rate_limited {
                return Err(AppError::other(
                    "Claude usage limit reached. Your Claude subscription's quota is exhausted; \
                     it will reset later.",
                ));
            }
            if !exit_status.success() {
                let msg = if stderr_text.trim().is_empty() {
                    "Claude Code exited without finishing the turn — make sure you are signed \
                     in (run `claude` in a terminal)."
                        .to_string()
                } else {
                    format!(
                        "Claude Code exited with an error: {}",
                        stderr_text.trim().chars().take(500).collect::<String>()
                    )
                };
                return Err(AppError::other(msg));
            }
            if !accumulator.is_empty() {
                Ok(TurnOutcome { reply: accumulator })
            } else {
                Err(AppError::other(
                    "Claude Code exited without finishing the turn — make sure you are signed \
                     in (run `claude` in a terminal).",
                ))
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::conversation::TranscriptTurn;
    use std::fs::File;

    // ── parse_stream_line fixtures ────────────────────────────────────────

    /// The captured `stream-json` transcript from the plan, containing a
    /// `thinking` block followed by a `text` block. Asserts the exact
    /// classification of every line type.
    #[test]
    fn stream_line_transcript_fixture() {
        let transcript = [
            // system init — ignore
            r#"{"type":"system","subtype":"init","session_id":"s1","tools":[]}"#,
            // rate_limit_event allowed — ignore
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"},"session_id":"s1"}"#,
            // message_start — ignore
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"role":"assistant"}},"session_id":"s1"}"#,
            // thinking content_block_start — ignore
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}},"session_id":"s1"}"#,
            // thinking_delta — must NOT become a Token
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me check the notes."}},"session_id":"s1"}"#,
            // content_block_stop — ignore
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0},"session_id":"s1"}"#,
            // text content_block_start — ignore
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}},"session_id":"s1"}"#,
            // text_delta "Sarah " — Token
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Sarah "}},"session_id":"s1"}"#,
            // text_delta "works at Acme." — Token
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"works at Acme."}},"session_id":"s1"}"#,
            // content_block_stop — ignore
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1},"session_id":"s1"}"#,
            // message_stop — ignore
            r#"{"type":"stream_event","event":{"type":"message_stop"},"session_id":"s1"}"#,
            // result — Done
            r#"{"type":"result","subtype":"success","is_error":false,"result":"Sarah works at Acme.","session_id":"s1","total_cost_usd":0.01}"#,
        ];

        let results: Vec<StreamLine> = transcript.iter().map(|l| parse_stream_line(l)).collect();

        // thinking_delta line (index 4) → Other
        assert!(
            matches!(results[4], StreamLine::Other),
            "thinking_delta must be Other, got {:?}",
            results[4]
        );

        // text_delta lines (indices 7 and 8) → Token
        assert_eq!(results[7], StreamLine::Token("Sarah ".to_string()));
        assert_eq!(results[8], StreamLine::Token("works at Acme.".to_string()));

        // result line (index 11) → Done
        assert!(
            matches!(
                results[11],
                StreamLine::Done {
                    is_error: false,
                    ..
                }
            ),
            "result line must be Done(is_error=false)"
        );
        if let StreamLine::Done { answer, .. } = &results[11] {
            assert_eq!(answer, "Sarah works at Acme.");
        }

        // Only Token variants should have been forwarded — collect them.
        let tokens: Vec<String> = results
            .into_iter()
            .filter_map(|r| match r {
                StreamLine::Token(t) => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(tokens, vec!["Sarah ", "works at Acme."]);
    }

    /// An error result line must surface as `Done { is_error: true, subtype: Some(...) }`.
    #[test]
    fn stream_line_error_result() {
        let line =
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":""}"#;
        assert!(
            matches!(
                parse_stream_line(line),
                StreamLine::Done {
                    is_error: true,
                    subtype: Some(_),
                    ..
                }
            ),
            "error result must be Done(is_error=true)"
        );
        if let StreamLine::Done { subtype, .. } = parse_stream_line(line) {
            assert_eq!(subtype.as_deref(), Some("error_during_execution"));
        }
    }

    /// `rate_limit_event` with `status != "allowed"` → `RateLimited`.
    /// `status == "allowed"` → `Other`.
    #[test]
    fn stream_line_rate_limit() {
        let rejected = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected"}}"#;
        assert_eq!(parse_stream_line(rejected), StreamLine::RateLimited);

        let allowed = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}"#;
        assert_eq!(parse_stream_line(allowed), StreamLine::Other);
    }

    /// A line that is not valid JSON must never error — it must yield `Other`.
    #[test]
    fn stream_line_malformed_json_is_other() {
        assert_eq!(parse_stream_line("not json at all"), StreamLine::Other);
        assert_eq!(parse_stream_line(""), StreamLine::Other);
    }

    // ── parse_auth_status ─────────────────────────────────────────────────

    /// Logged-in shape from verified v2.1.144 output.
    #[test]
    fn auth_status_logged_in() {
        let json = r#"{
            "loggedIn": true,
            "authMethod": "claude.ai",
            "apiProvider": "firstParty",
            "email": "user@example.com",
            "orgId": "org_abc",
            "orgName": "Acme",
            "subscriptionType": "max"
        }"#;
        let parsed = parse_auth_status(json);
        assert!(parsed.logged_in);
        assert_eq!(parsed.auth_method.as_deref(), Some("claude.ai"));
        assert_eq!(parsed.subscription_type.as_deref(), Some("max"));
        assert_eq!(parsed.email.as_deref(), Some("user@example.com"));
    }

    /// Logged-out shape — minimal, must not panic or return logged_in=true.
    #[test]
    fn auth_status_logged_out() {
        let json = r#"{"loggedIn": false}"#;
        let parsed = parse_auth_status(json);
        assert!(!parsed.logged_in);
        assert!(parsed.auth_method.is_none());
        assert!(parsed.subscription_type.is_none());
        assert!(parsed.email.is_none());
    }

    /// Completely invalid JSON — must degrade gracefully to logged_in=false.
    #[test]
    fn auth_status_invalid_json() {
        let parsed = parse_auth_status("not json");
        assert!(!parsed.logged_in);
    }

    // ── first_existing / locate precedence ────────────────────────────────

    /// Verify that `first_existing` returns the first path that exists and
    /// ignores later ones, matching the priority order of `locate()`.
    ///
    /// Uses `std::env::temp_dir()` with a unique prefix so the test is
    /// self-contained with no additional crate dependencies.
    #[test]
    fn first_existing_precedence() {
        let base = std::env::temp_dir();
        // Use a unique-ish name derived from the thread id to avoid collisions
        // when tests run in parallel.
        let id = format!("{:?}", std::thread::current().id()).replace(['(', ')', ' '], "_");
        let a = base.join(format!("sediment_test_{id}_a"));
        let b = base.join(format!("sediment_test_{id}_b"));
        let c = base.join(format!("sediment_test_{id}_c"));

        // Clean up any leftover files from a previous run.
        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
        let _ = std::fs::remove_file(&c);

        // Create only b and c — a does not exist.
        File::create(&b).expect("create b");
        File::create(&c).expect("create c");

        let candidates = vec![a.clone(), b.clone(), c.clone()];
        let found = first_existing(&candidates);
        assert_eq!(found, Some(b.clone()), "should return b (first existing)");

        // Now create a — it has higher precedence.
        File::create(&a).expect("create a");
        let found2 = first_existing(&candidates);
        assert_eq!(found2, Some(a.clone()), "should return a once it exists");

        // Clean up.
        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
        let _ = std::fs::remove_file(&c);
    }

    /// When no candidate exists, `first_existing` returns `None`.
    #[test]
    fn first_existing_none_when_empty_or_all_missing() {
        assert_eq!(first_existing(&[]), None);

        let base = std::env::temp_dir();
        let missing = vec![
            base.join("sediment_test_definitely_missing_x_abc123"),
            base.join("sediment_test_definitely_missing_y_abc123"),
        ];
        assert_eq!(first_existing(&missing), None);
    }

    // ── Live integration test (ignored in CI) ─────────────────────────────

    /// Locate the real installed `claude` binary and detect its auth status.
    ///
    /// Excluded from CI via `#[ignore]` (ADR-0006 Layer 2 convention: only
    /// deterministic unit tests gate the build). The end-to-end agent turn is
    /// covered separately by `live_run_turn_agent`.
    #[tokio::test]
    #[ignore]
    async fn live_detect() {
        let status = detect().await;
        println!("ClaudeCodeStatus: {status:?}");
        assert!(status.installed, "expected claude binary to be installed");
    }

    // ── Extended stream parser: tool_use ──────────────────────────────────

    /// A captured `stream-json` agent transcript — a real two-iteration turn
    /// where the agent calls `Read` (a native file tool), gets a result, then
    /// produces its text reply. Asserts the settled `assistant` message with a
    /// `tool_use` block classifies as `ToolUses`, and that the partial
    /// `content_block_start`/`input_json_delta` events do NOT (they carry no
    /// settled input). The fixture lines are trimmed from a live capture.
    #[test]
    fn stream_line_agent_tool_use_fixture() {
        let transcript = [
            // system init — Other
            r#"{"type":"system","subtype":"init","session_id":"s2","tools":["Read","Edit"]}"#,
            // message_start — Other
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"role":"assistant","content":[]}},"session_id":"s2"}"#,
            // partial tool_use content_block_start — input is {} → Other
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"Read","input":{}}},"session_id":"s2"}"#,
            // input_json_delta — Other
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"file_path\":\"probe.txt\"}"}},"session_id":"s2"}"#,
            // settled assistant message carrying the complete tool_use → ToolUses
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"/f/People/Josh.md"}}]},"session_id":"s2"}"#,
            // content_block_stop — Other
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1},"session_id":"s2"}"#,
            // tool_result user line — Other
            r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_1","type":"tool_result","content":"..."}]},"session_id":"s2"}"#,
            // text_delta — Token
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Got it."}},"session_id":"s2"}"#,
            // settled assistant message with only a text block → Other (no tool_use)
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Got it."}]},"session_id":"s2"}"#,
            // result — Done
            r#"{"type":"result","subtype":"success","is_error":false,"result":"Got it.","session_id":"s2"}"#,
        ];

        let results: Vec<StreamLine> = transcript.iter().map(|l| parse_stream_line(l)).collect();

        // partial content_block_start / input_json_delta (indices 2, 3) → Other
        assert!(
            matches!(results[2], StreamLine::Other),
            "partial tool_use content_block_start must be Other, got {:?}",
            results[2]
        );
        assert!(
            matches!(results[3], StreamLine::Other),
            "input_json_delta must be Other, got {:?}",
            results[3]
        );

        // settled assistant message with a tool_use block (index 4) → ToolUses
        match &results[4] {
            StreamLine::ToolUses(calls) => {
                assert_eq!(calls.len(), 1, "one tool call");
                assert_eq!(calls[0].name, "Read");
                assert_eq!(
                    calls[0].input["file_path"].as_str(),
                    Some("/f/People/Josh.md"),
                    "settled tool_use carries the complete input"
                );
            }
            other => panic!("expected ToolUses, got {other:?}"),
        }

        // text_delta (index 7) → Token
        assert_eq!(results[7], StreamLine::Token("Got it.".to_string()));

        // settled assistant message with only a text block (index 8) → Other
        assert!(
            matches!(results[8], StreamLine::Other),
            "text-only assistant message must be Other, got {:?}",
            results[8]
        );

        // result (index 9) → Done
        assert!(matches!(
            results[9],
            StreamLine::Done {
                is_error: false,
                ..
            }
        ));
    }

    /// An assistant message can carry several `tool_use` blocks in one line —
    /// all of them must surface.
    #[test]
    fn stream_line_multiple_tool_uses_in_one_message() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[
            {"type":"thinking","thinking":"plan"},
            {"type":"tool_use","id":"t1","name":"mcp__sediment__find_contradiction","input":{"subject":"Josh","predicate":"works_at","object":"Acme"}},
            {"type":"tool_use","id":"t2","name":"mcp__sediment__record_fact","input":{"subject":"Josh","predicate":"works_at","object":"Cloudflare"}}
        ]},"session_id":"s3"}"#;

        match parse_stream_line(line) {
            StreamLine::ToolUses(calls) => {
                assert_eq!(calls.len(), 2, "two tool calls, thinking block ignored");
                assert_eq!(calls[0].name, "mcp__sediment__find_contradiction");
                assert_eq!(calls[1].name, "mcp__sediment__record_fact");
            }
            other => panic!("expected ToolUses, got {other:?}"),
        }
    }

    /// `summarize_tool_call` renders a terse human phrase: MCP namespacing is
    /// stripped and a key argument is appended when one exists.
    #[test]
    fn summarize_tool_call_phrasing() {
        // Native file tool — file_path is the detail.
        let read = ToolUseCall {
            name: "Read".to_string(),
            input: serde_json::json!({ "file_path": "/f/People/Josh.md" }),
        };
        assert_eq!(summarize_tool_call(&read), "Read /f/People/Josh.md");

        // MCP tool — namespace stripped, `subject` is the detail.
        let record = ToolUseCall {
            name: "mcp__sediment__record_fact".to_string(),
            input: serde_json::json!({
                "subject": "Josh", "predicate": "works_at", "object": "Cloudflare"
            }),
        };
        assert_eq!(summarize_tool_call(&record), "record_fact Josh");

        // search_notes — `query` is the detail.
        let search = ToolUseCall {
            name: "mcp__sediment__search_notes".to_string(),
            input: serde_json::json!({ "query": "Q3 roadmap", "k": 5 }),
        };
        assert_eq!(summarize_tool_call(&search), "search_notes Q3 roadmap");

        // No recognised key argument — bare (de-namespaced) tool name.
        let bare = ToolUseCall {
            name: "mcp__sediment__related_facts".to_string(),
            input: serde_json::json!({ "entity": "Josh" }),
        };
        assert_eq!(summarize_tool_call(&bare), "related_facts");
    }

    // ── render_turn_prompt ────────────────────────────────────────────────

    /// With no history, the prompt states the formation path and carries the
    /// (trimmed) new message — no transcript block.
    #[test]
    fn render_turn_prompt_no_history() {
        let turn = TurnRequest {
            message: "  Josh moved to Cloudflare.  ".to_string(),
            history: vec![],
            formation_root: PathBuf::from("/f"),
            source_chat_id: "chat_message:1".to_string(),
            injected_context: None,
        };
        let p = render_turn_prompt(&turn);
        assert!(p.contains("# Your formation"));
        assert!(p.contains("/f"), "the formation's absolute path is stated");
        assert!(p.contains("# New message"));
        assert!(p.trim_end().ends_with("Josh moved to Cloudflare."));
        assert!(!p.contains("# Conversation so far"), "no history block");
    }

    /// With history, prior turns are labelled and the new message is the final
    /// block.
    #[test]
    fn render_turn_prompt_with_history() {
        let turn = TurnRequest {
            message: "He reports to Devon now.".to_string(),
            history: vec![
                TranscriptTurn {
                    role: "user".to_string(),
                    content: "Josh works at Cloudflare.".to_string(),
                },
                TranscriptTurn {
                    role: "assistant".to_string(),
                    content: "Got it — filed under People/Josh.md.".to_string(),
                },
            ],
            formation_root: PathBuf::from("/f"),
            source_chat_id: "chat_message:2".to_string(),
            injected_context: None,
        };
        let p = render_turn_prompt(&turn);
        assert!(p.contains("# Your formation"));
        assert!(p.contains("# Conversation so far"));
        assert!(p.contains("User: Josh works at Cloudflare."));
        assert!(p.contains("Assistant: Got it — filed under People/Josh.md."));
        assert!(p.contains("# New message"));
        assert!(p.trim_end().ends_with("He reports to Devon now."));
    }

    /// Injected grounding (ADR-0011) renders as a `# What you already know`
    /// section ahead of the new message; absent context renders no section.
    #[test]
    fn render_turn_prompt_with_injected_context() {
        let with = TurnRequest {
            message: "He moved to Stripe.".to_string(),
            history: vec![],
            formation_root: PathBuf::from("/f"),
            source_chat_id: "chat_message:3".to_string(),
            injected_context: Some("Josh → People/Josh.md (works_at Cloudflare)".to_string()),
        };
        let p = render_turn_prompt(&with);
        assert!(p.contains("# What you already know"));
        assert!(p.contains("Josh → People/Josh.md"));
        assert!(
            p.find("# What you already know") < p.find("# New message"),
            "grounding precedes the new message",
        );

        let without = TurnRequest {
            injected_context: None,
            ..with
        };
        assert!(!render_turn_prompt(&without).contains("# What you already know"));
    }

    // ── mcp_config_json ───────────────────────────────────────────────────

    /// The generated MCP config points the `sediment` server at this binary
    /// with `--mcp-stdio` and threads the formation root + provenance chat id
    /// through env vars — exactly what `lib::run_mcp_stdio` reads.
    #[test]
    fn mcp_config_json_shape() {
        let turn = TurnRequest {
            message: "hi".to_string(),
            history: vec![],
            formation_root: PathBuf::from("/Users/x/formation"),
            source_chat_id: "chat_message:42".to_string(),
            injected_context: None,
        };
        let raw = mcp_config_json(Path::new("/Apps/Sediment.app/sediment"), &turn);
        let v: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");

        let server = &v["mcpServers"]["sediment"];
        assert_eq!(
            server["command"].as_str(),
            Some("/Apps/Sediment.app/sediment")
        );
        assert_eq!(
            server["args"].as_array().map(|a| a.len()),
            Some(1),
            "args is [--mcp-stdio]"
        );
        assert_eq!(server["args"][0].as_str(), Some("--mcp-stdio"));
        assert_eq!(
            server["env"]["SEDIMENT_FORMATION"].as_str(),
            Some("/Users/x/formation")
        );
        assert_eq!(
            server["env"]["SEDIMENT_SOURCE_CHAT_ID"].as_str(),
            Some("chat_message:42")
        );
    }

    /// The behaviour prompt is embedded at compile time and is non-trivial.
    #[test]
    fn conversation_agent_prompt_is_embedded() {
        assert!(
            CONVERSATION_AGENT_PROMPT.contains("conversational agent"),
            "the embedded behaviour prompt should be the real file"
        );
        assert!(CONVERSATION_AGENT_PROMPT.len() > 500);
    }

    // ── Live integration test (ignored in CI) ─────────────────────────────

    /// End-to-end: drive one real `claude` agent turn through `run_turn`
    /// against a throwaway formation. Asserts the agent edits a note file with
    /// its native tools and produces a non-empty reply.
    ///
    /// Excluded from CI via `#[ignore]` — needs the live binary, a login, and
    /// quota (ADR-0006 Layer 2 convention).
    #[tokio::test]
    #[ignore]
    async fn live_run_turn_agent() {
        let status = detect().await;
        if !status.installed || !status.logged_in {
            println!("claude not installed / not logged in — skipping live run_turn test.");
            return;
        }

        // A throwaway formation directory.
        let root = std::env::temp_dir()
            .join("sediment-live-run-turn")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&root).expect("create formation root");

        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let events_clone = events.clone();
        let sink: TurnEventSink = Box::new(move |ev| match ev {
            TurnEvent::TextDelta { text } => {
                events_clone.lock().unwrap().push(format!("text:{text}"));
            }
            TurnEvent::ToolActivity { tool, summary } => {
                events_clone
                    .lock()
                    .unwrap()
                    .push(format!("tool:{tool} ({summary})"));
            }
        });

        let turn = TurnRequest {
            message: "Make a note that Maria likes hiking. Put it in People/Maria.md.".to_string(),
            history: vec![],
            formation_root: root.clone(),
            source_chat_id: "chat_message:live".to_string(),
            injected_context: None,
        };

        let engine = ClaudeCodeEngine::new(DEFAULT_MODEL);
        let outcome = engine
            .run_turn(&turn, &sink)
            .await
            .expect("run_turn should succeed when logged in");

        println!("reply: {}", outcome.reply);
        println!("events: {:?}", events.lock().unwrap());
        assert!(!outcome.reply.is_empty(), "reply should not be empty");

        // The agent must write the note INSIDE the formation — a bare relative
        // path must never escape to the home directory or anywhere else.
        let wrote_into_formation = walkdir::WalkDir::new(&root)
            .into_iter()
            .flatten()
            .any(|e| e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "md"));
        assert!(
            wrote_into_formation,
            "the agent must write the note inside the formation root"
        );

        std::fs::remove_dir_all(&root).ok();
    }
}
