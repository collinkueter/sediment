//! The GitHub Copilot CLI as a *warm* conversational-agent engine — ADR-0012.
//!
//! Copilot's warm path is the **Agent Client Protocol** (ACP): `copilot --acp`
//! is a bidirectional JSON-RPC 2.0 server over stdio (NDJSON). One long-lived
//! process holds one session and serves many `session/prompt`s — the resident
//! model that gives Copilot its speed advantage over the cold-spawn Claude
//! engine. The full wire spec lives in `docs/copilot-acp-integration.md`.
//!
//! Layout:
//! - **Protocol layer** — message builders, the structural dispatch ([`classify`])
//!   that survives Copilot's colliding server-request id space, the
//!   `session/update` → [`TurnEvent`] mapping, and the permission auto-approver.
//!   Pure and unit-tested.
//! - **[`CopilotSession`]** — a resident `copilot --acp` child with async
//!   reader/writer tasks, the request/response correlator, and a streaming turn.
//! - **[`CopilotEngineHandle`]** — Tauri state holding the warm session, created
//!   lazily per formation and recycled on error or degradation (#2755).

use crate::core::agent_tone;
use crate::core::cli_launch;
use crate::core::conversation::{TurnEvent, TurnEventSink, TurnOutcome, TurnRequest, TurnStop};
use crate::error::{AppError, AppResult};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_util::sync::CancellationToken;

/// Default Copilot model — configurable in settings (M9). `claude-haiku-4.5` is
/// a fast, capable default; `gpt-5-mini` is the zero-premium-request option.
pub const DEFAULT_MODEL: &str = "claude-haiku-4.5";

/// The agent behaviour prompt (ADR-0009 §8) — prepended to the first turn of a
/// session, since Copilot has no `--system-prompt` flag.
const CONVERSATION_AGENT_PROMPT: &str = include_str!("../../../prompts/conversation-agent.md");

/// Wall-clock cap on one `session/prompt`.
const TURN_TIMEOUT: Duration = Duration::from_secs(300);
/// Cap on the `initialize` + `session/new` handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// Recycle the resident process after this many turns to dodge the #2755
/// latency-degradation bug (docs/copilot-acp-integration.md).
const MAX_TURNS_PER_PROCESS: usize = 40;
/// After sending `session/cancel`, how long to wait for the prompt to actually
/// wind down (its response to arrive) before giving up and recycling the
/// session. We don't yet know the installed CLI honours `session/cancel`
/// mid-prompt (see the plan's M4 probe); if it doesn't, the grace elapses and the
/// session is killed + recycled — colder, but always correct.
const CANCEL_GRACE: Duration = Duration::from_secs(5);

// ── Binary discovery ──────────────────────────────────────────────────────────

/// Resolve the `copilot` binary. As with `claude`, a macOS GUI app does not
/// inherit the shell `PATH`; and `copilot` is typically an npm-global under an
/// nvm-versioned path, so the login-shell probe is the reliable resolver. A few
/// common prefixes are tried first.
pub fn locate() -> Option<PathBuf> {
    if cfg!(windows) {
        return locate_windows();
    }
    locate_unix()
}

/// macOS / Linux resolver: common prefixes, then a login-shell `command -v`.
fn locate_unix() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(PathBuf::from(home).join(".local/bin/copilot"));
    }
    candidates.push(PathBuf::from("/opt/homebrew/bin/copilot"));
    candidates.push(PathBuf::from("/usr/local/bin/copilot"));
    if let Some(found) = candidates.into_iter().find(|c| c.is_file()) {
        return Some(found);
    }
    login_shell_which("copilot")
}

/// Windows resolver: the npm-global `copilot.exe`/`copilot.cmd`, then a PATH
/// `where` probe. The shared launch logic (shim handling, `where`) lives in
/// [`crate::core::cli_launch`].
fn locate_windows() -> Option<PathBuf> {
    if let Some(found) = cli_launch::windows_npm_candidates("copilot")
        .into_iter()
        .find(|c| c.is_file())
    {
        return Some(found);
    }
    cli_launch::where_which("copilot")
}

/// `$SHELL -lc "command -v <bin>"` — sources the user's shell rc so an
/// nvm/npm-global binary on `PATH` is found even when the app did not inherit it.
fn login_shell_which(bin: &str) -> Option<PathBuf> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let out = std::process::Command::new(shell)
        .args(["-lc", &format!("command -v {bin}")])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// Install status of the local Copilot CLI, for the settings UI.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CopilotStatus {
    pub installed: bool,
    pub binary_path: Option<String>,
}

/// Probe for the `copilot` binary. Copilot has no cheap `auth status` command,
/// so this reports install only — login state surfaces on the first turn.
pub fn detect() -> CopilotStatus {
    match locate() {
        Some(p) => CopilotStatus {
            installed: true,
            binary_path: Some(p.to_string_lossy().into_owned()),
        },
        None => CopilotStatus {
            installed: false,
            binary_path: None,
        },
    }
}

// ── Model discovery ───────────────────────────────────────────────────────────

/// One model the user's Copilot account can use, as advertised by the ACP
/// `session/new` handshake (`models.availableModels`).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotModel {
    pub model_id: String,
    pub name: String,
    pub description: Option<String>,
    /// Premium-request multiplier the account is billed for a turn, e.g. `"0x"`
    /// (free), `"0.33x"` (from `_meta.copilotUsage`). `None` when not reported.
    pub usage: Option<String>,
    /// Whether the account has this model enabled (`copilotEnablement == "enabled"`).
    pub enabled: bool,
}

/// The models the user's Copilot account advertises, plus its default.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotModels {
    pub available: Vec<CopilotModel>,
    /// The account's current/default model id, when reported.
    pub current_model_id: Option<String>,
}

/// Discover the Copilot models the user's account can use by running a minimal
/// `copilot --acp` handshake (`initialize` + `session/new`) and reading the
/// `models` the server advertises. No prompt is sent, so no request is spent.
/// Best-effort: errors if the binary is missing or the handshake fails, so the
/// UI can fall back to a free-text model field.
pub async fn fetch_models(cwd: &Path) -> AppResult<CopilotModels> {
    let binary = locate().ok_or_else(|| AppError::other("copilot binary not found"))?;
    let cwd_arg = cwd.to_string_lossy().into_owned();
    let mut cmd = cli_launch::tokio_command(&binary, &["--acp", "--add-dir", &cwd_arg]);
    cmd.current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = cmd
        .spawn()
        .map_err(|e| AppError::other(format!("spawn copilot --acp: {e}")))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| AppError::other("copilot: no stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::other("copilot: no stdout"))?;

    // Drive the handshake, then read responses until session/new (id 2) returns.
    const SESSION_NEW_ID: i64 = 2;
    let cwd_str = cwd.to_string_lossy().into_owned();
    let timed = tokio::time::timeout(HANDSHAKE_TIMEOUT, async move {
        stdin
            .write_all(ndjson_line(&initialize_msg(1)).as_bytes())
            .await
            .map_err(|e| AppError::other(format!("copilot write init: {e}")))?;
        stdin
            .write_all(ndjson_line(&session_new_msg(SESSION_NEW_ID, &cwd_str)).as_bytes())
            .await
            .map_err(|e| AppError::other(format!("copilot write session/new: {e}")))?;
        stdin.flush().await.ok();

        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines
            .next_line()
            .await
            .map_err(|e| AppError::other(format!("copilot read: {e}")))?
        {
            if let Incoming::Response {
                id,
                result: res,
                error,
            } = classify(&line)
            {
                if id == SESSION_NEW_ID {
                    return match error {
                        Some(e) => Err(AppError::other(format!("copilot session/new: {e}"))),
                        None => Ok(res.unwrap_or(Value::Null)),
                    };
                }
            }
        }
        Err(AppError::other(
            "copilot: stream ended before session/new response",
        ))
    })
    .await;

    // Always tear the discovery process down — we only needed the handshake.
    child.start_kill().ok();

    let resp = timed.map_err(|_| AppError::other("copilot: model discovery timed out"))??;
    Ok(parse_models(&resp))
}

/// Parse the `models` object of a `session/new` response into [`CopilotModels`].
/// Tolerant of a missing or oddly-shaped `models` field (yields an empty list).
fn parse_models(resp: &Value) -> CopilotModels {
    let models = resp.get("models");
    let current_model_id = models
        .and_then(|m| m.get("currentModelId"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let available = models
        .and_then(|m| m.get("availableModels"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    let model_id = m.get("modelId").and_then(Value::as_str)?.to_string();
                    let name = m
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or(&model_id)
                        .to_string();
                    let description = m
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let meta = m.get("_meta");
                    let usage = meta
                        .and_then(|x| x.get("copilotUsage"))
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let enabled = meta
                        .and_then(|x| x.get("copilotEnablement"))
                        .and_then(Value::as_str)
                        .map(|s| s == "enabled")
                        .unwrap_or(true);
                    Some(CopilotModel {
                        model_id,
                        name,
                        description,
                        usage,
                        enabled,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    CopilotModels {
        available,
        current_model_id,
    }
}

// ── ACP protocol (NDJSON JSON-RPC 2.0) ────────────────────────────────────────
// docs/copilot-acp-integration.md is the authoritative spec.

/// Serialize one ACP message to a single NDJSON line (trailing `\n`). ACP
/// forbids embedded newlines, so this is always compact (never pretty) JSON.
pub fn ndjson_line(msg: &Value) -> String {
    let mut s = msg.to_string();
    s.push('\n');
    s
}

/// `initialize` request. `fs` capabilities are **false** — the agent uses its
/// own file tools (scoped by `--add-dir`), so it never delegates file I/O to us.
pub fn initialize_msg(id: i64) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"initialize","params":{
        "protocolVersion":1,
        "clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false},
        "clientInfo":{"name":"sediment","version":env!("CARGO_PKG_VERSION")}}})
}

/// `session/new` request. STDIO MCP servers are deliberately NOT passed here —
/// Copilot silently drops non-http/sse servers from the ACP param; they go via
/// the `--additional-mcp-config` file instead. `cwd` is the formation root.
pub fn session_new_msg(id: i64, cwd: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"session/new","params":{
        "cwd":cwd,"mcpServers":[]}})
}

/// `session/prompt` request — one turn's text (the caller has already prepended
/// this turn's grounding, and the persona on the session's first turn).
pub fn session_prompt_msg(id: i64, session_id: &str, text: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"session/prompt","params":{
        "sessionId":session_id,"prompt":[{"type":"text","text":text}]}})
}

/// `session/cancel` notification — asks the server to stop the in-flight prompt.
/// A notification (no `id`, no response expected); the prompt's own response is
/// what tells us it wound down. The user interrupting a turn sends this.
pub fn session_cancel_msg(session_id: &str) -> Value {
    json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session_id}})
}

/// Auto-approve reply to a `session/request_permission` request — picks the
/// `allow_always` option (else any allow option, else `allow_once`). `their_id`
/// is Copilot's request id: its id space collides with ours, so we reply with
/// *their* id, not one of ours.
pub fn permission_allow_msg(their_id: i64, request_params: &Value) -> Value {
    let option_id = request_params
        .get("options")
        .and_then(Value::as_array)
        .and_then(|opts| {
            opts.iter()
                .find(|o| o.get("optionId").and_then(Value::as_str) == Some("allow_always"))
                .or_else(|| {
                    opts.iter().find(|o| {
                        matches!(
                            o.get("kind").and_then(Value::as_str),
                            Some("allow_always") | Some("allow_once")
                        )
                    })
                })
                .and_then(|o| o.get("optionId").and_then(Value::as_str))
        })
        .unwrap_or("allow_once")
        .to_string();
    json!({"jsonrpc":"2.0","id":their_id,"result":{
        "outcome":{"outcome":"selected","optionId":option_id}}})
}

/// One incoming ACP line, classified by **shape** — not id. Copilot's
/// server→client request ids start at 0 and collide with our client request
/// ids, so id alone cannot tell a request from a response.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Response {
        id: i64,
        result: Option<Value>,
        error: Option<Value>,
    },
    Request {
        id: i64,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    Other,
}

/// Classify one NDJSON line per JSON-RPC 2.0 shape.
pub fn classify(line: &str) -> Incoming {
    let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
        return Incoming::Other;
    };
    let method = v.get("method").and_then(Value::as_str).map(str::to_string);
    let id = v.get("id").and_then(Value::as_i64);
    match (method, id) {
        (Some(method), Some(id)) => Incoming::Request {
            id,
            method,
            params: v.get("params").cloned().unwrap_or(Value::Null),
        },
        (Some(method), None) => Incoming::Notification {
            method,
            params: v.get("params").cloned().unwrap_or(Value::Null),
        },
        (None, Some(id)) => Incoming::Response {
            id,
            result: v.get("result").cloned(),
            error: v.get("error").cloned(),
        },
        (None, None) => Incoming::Other,
    }
}

/// Map a `session/update` notification's params to a streamed [`TurnEvent`], or
/// `None` for updates we ignore (thoughts, plans, command/config noise). Reply
/// text comes from `agent_message_chunk`; `tool_call` surfaces as activity.
pub fn session_update_event(params: &Value) -> Option<TurnEvent> {
    let update = params.get("update")?;
    match update.get("sessionUpdate").and_then(Value::as_str) {
        Some("agent_message_chunk") => {
            let text = update
                .get("content")
                .and_then(|c| c.get("text"))
                .and_then(Value::as_str)?;
            (!text.is_empty()).then(|| TurnEvent::TextDelta {
                text: text.to_string(),
            })
        }
        Some("tool_call") => {
            let summary = update
                .get("title")
                .and_then(Value::as_str)
                .or_else(|| update.get("kind").and_then(Value::as_str))
                .unwrap_or("tool")
                .to_string();
            Some(TurnEvent::ToolActivity {
                tool: "copilot".to_string(),
                summary,
            })
        }
        _ => None,
    }
}

// ── Resident session ──────────────────────────────────────────────────────────

type PendingMap = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>>;
type ActiveSink = Arc<Mutex<Option<mpsc::UnboundedSender<Value>>>>;

/// The result of one `CopilotSession::run_turn`. Beyond the reply and how it
/// stopped, it carries whether the session must be recycled — set when an
/// interrupt's `session/cancel` was *not* acknowledged within [`CANCEL_GRACE`],
/// so the resident process can't be trusted to be idle for the next turn.
struct SessionTurn {
    reply: String,
    stop: TurnStop,
    recycle: bool,
}

/// A resident `copilot --acp` process holding one ACP session, serving many
/// turns. Created lazily per formation and reused across turns (the warm path).
struct CopilotSession {
    child: Child,
    /// Lines to write to the child's stdin (single-owner writer task drains it).
    writer_tx: mpsc::UnboundedSender<String>,
    /// Our outstanding requests, by id → a oneshot for the response.
    pending: PendingMap,
    /// The current turn's notification sink — the reader forwards `session/update`
    /// params here while a turn is in flight.
    active: ActiveSink,
    session_id: String,
    next_id: AtomicI64,
    turns: AtomicUsize,
    /// `false` until the persona has been sent (the first turn of the session).
    persona_sent: AtomicBool,
}

impl CopilotSession {
    fn next_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Spawn `copilot --acp`, wire the reader/writer tasks, and run the
    /// `initialize` + `session/new` handshake. `mcp_config_path` points at the
    /// stdio MCP config file (Copilot drops stdio servers from the ACP param).
    async fn spawn(
        binary: &Path,
        formation: &Path,
        model: &str,
        mcp_config_path: &Path,
    ) -> AppResult<Self> {
        let dir_arg = formation.to_string_lossy().into_owned();
        let mcp_arg = format!("@{}", mcp_config_path.to_string_lossy());
        let mut cmd = cli_launch::tokio_command(
            binary,
            &[
                "--acp",
                "--disable-builtin-mcps",
                "--allow-all-tools",
                "--no-custom-instructions",
                "--add-dir",
                &dir_arg,
                "--additional-mcp-config",
                &mcp_arg,
                "--model",
                model,
            ],
        );
        cmd.current_dir(formation)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| AppError::other(format!("spawn copilot --acp: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AppError::other("copilot: no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AppError::other("copilot: no stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| AppError::other("copilot: no stderr"))?;

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let active: ActiveSink = Arc::new(Mutex::new(None));
        let (writer_tx, mut writer_rx) = mpsc::unbounded_channel::<String>();

        // Writer task — single owner of the child's stdin.
        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(line) = writer_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                let _ = stdin.flush().await;
            }
        });

        // Stderr drain — Copilot logs healthy events at ERROR level, so debug it.
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                tracing::debug!("copilot acp stderr: {l}");
            }
        });

        // Reader task — route responses, auto-approve permission requests, and
        // forward `session/update` notifications to the active turn.
        {
            let pending = pending.clone();
            let active = active.clone();
            let writer_tx = writer_tx.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    match classify(&line) {
                        Incoming::Response { id, result, error } => {
                            if let Some(tx) = pending.lock().await.remove(&id) {
                                let payload = match error {
                                    Some(e) => Err(e.to_string()),
                                    None => Ok(result.unwrap_or(Value::Null)),
                                };
                                let _ = tx.send(payload);
                            }
                        }
                        Incoming::Request { id, method, params } => {
                            let reply = if method == "session/request_permission" {
                                permission_allow_msg(id, &params)
                            } else {
                                json!({"jsonrpc":"2.0","id":id,
                                    "error":{"code":-32601,"message":"method not supported"}})
                            };
                            let _ = writer_tx.send(ndjson_line(&reply));
                        }
                        Incoming::Notification { method, params } if method == "session/update" => {
                            if let Some(tx) = active.lock().await.as_ref() {
                                let _ = tx.send(params);
                            }
                        }
                        _ => {}
                    }
                }
            });
        }

        let mut session = CopilotSession {
            child,
            writer_tx,
            pending,
            active,
            session_id: String::new(),
            next_id: AtomicI64::new(1),
            turns: AtomicUsize::new(0),
            persona_sent: AtomicBool::new(false),
        };

        // Handshake.
        let init_id = session.next_id();
        session
            .request(initialize_msg(init_id), HANDSHAKE_TIMEOUT)
            .await?;
        let sn_id = session.next_id();
        let resp = session
            .request(
                session_new_msg(sn_id, formation.to_string_lossy().as_ref()),
                HANDSHAKE_TIMEOUT,
            )
            .await?;
        session.session_id = resp
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::other("copilot: session/new returned no sessionId"))?
            .to_string();

        Ok(session)
    }

    /// Send a request and await its response (no streaming). Used for the
    /// handshake. `msg` must already carry its `id`.
    async fn request(&self, msg: Value, timeout: Duration) -> AppResult<Value> {
        let id = msg
            .get("id")
            .and_then(Value::as_i64)
            .ok_or_else(|| AppError::other("copilot: request missing id"))?;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        self.writer_tx
            .send(ndjson_line(&msg))
            .map_err(|_| AppError::other("copilot: writer closed"))?;
        let resp = tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| AppError::other("copilot: request timed out"))?
            .map_err(|_| AppError::other("copilot: response channel closed"))?;
        resp.map_err(|e| AppError::other(format!("copilot error: {e}")))
    }

    /// Run one turn: send `session/prompt`, stream `session/update` text/tool
    /// events to `on_event`, and return the accumulated reply when the prompt
    /// resolves — or, if `cancel` trips, the partial reply as an interrupted turn.
    async fn run_turn(
        &self,
        prompt_text: &str,
        on_event: &TurnEventSink,
        cancel: &CancellationToken,
    ) -> AppResult<SessionTurn> {
        // Route this turn's notifications.
        let (notif_tx, mut notif_rx) = mpsc::unbounded_channel::<Value>();
        *self.active.lock().await = Some(notif_tx);

        let id = self.next_id();
        let (resp_tx, resp_rx) = oneshot::channel::<Result<Value, String>>();
        self.pending.lock().await.insert(id, resp_tx);
        self.writer_tx
            .send(ndjson_line(&session_prompt_msg(
                id,
                &self.session_id,
                prompt_text,
            )))
            .map_err(|_| AppError::other("copilot: writer closed"))?;

        /// How the streaming loop ended.
        enum LoopEnd {
            /// The `session/prompt` response arrived (its raw oneshot payload).
            Responded(Result<Value, String>),
            /// The user interrupted the turn.
            Cancelled,
        }

        let mut reply = String::new();
        let mut resp_rx = resp_rx;
        let loop_end = tokio::time::timeout(TURN_TIMEOUT, async {
            loop {
                tokio::select! {
                    biased;
                    r = &mut resp_rx => return LoopEnd::Responded(
                        r.unwrap_or_else(|_| Err("response channel closed".to_string())),
                    ),
                    _ = cancel.cancelled() => return LoopEnd::Cancelled,
                    maybe = notif_rx.recv() => {
                        if let Some(params) = maybe {
                            if let Some(ev) = session_update_event(&params) {
                                if let TurnEvent::TextDelta { ref text } = ev {
                                    reply.push_str(text);
                                }
                                on_event(ev);
                            }
                        }
                    }
                }
            }
        })
        .await
        .map_err(|_| AppError::other("copilot: turn timed out"))?;

        match loop_end {
            LoopEnd::Cancelled => {
                // Courtesy `session/cancel`, then wait a grace window for the
                // prompt to actually wind down. If its response arrives, the
                // session is idle again and stays warm; if the grace elapses the
                // CLI likely ignored the cancel, so flag the session for recycle.
                let _ =
                    self.writer_tx
                        .send(ndjson_line(&session_cancel_msg(&self.session_id)));
                let acknowledged = tokio::time::timeout(CANCEL_GRACE, &mut resp_rx)
                    .await
                    .is_ok();
                *self.active.lock().await = None;
                self.turns.fetch_add(1, Ordering::SeqCst);
                Ok(SessionTurn {
                    reply,
                    stop: TurnStop::Interrupted,
                    recycle: !acknowledged,
                })
            }
            LoopEnd::Responded(payload) => {
                *self.active.lock().await = None;
                // Drain any notifications buffered alongside the response.
                while let Ok(params) = notif_rx.try_recv() {
                    if let Some(ev) = session_update_event(&params) {
                        if let TurnEvent::TextDelta { ref text } = ev {
                            reply.push_str(text);
                        }
                        on_event(ev);
                    }
                }
                let resp = payload.map_err(|e| AppError::other(format!("copilot error: {e}")))?;
                if resp.get("stopReason").and_then(Value::as_str) == Some("refusal") {
                    return Err(AppError::other("copilot refused the request"));
                }
                self.turns.fetch_add(1, Ordering::SeqCst);
                Ok(SessionTurn {
                    reply,
                    stop: TurnStop::Completed,
                    recycle: false,
                })
            }
        }
    }

    /// Close stdin (EOF → clean ACP shutdown) and kill the child as a backstop.
    async fn shutdown(mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
        // `writer_tx` drops with `self`, ending the writer task and closing stdin.
    }
}

// ── Persistent engine handle (Tauri state) ────────────────────────────────────

struct ResidentEngine {
    session: CopilotSession,
    formation: PathBuf,
    model: String,
    /// The tone the persona was sent with. The warm session caches the persona
    /// on its first turn, so a tone change in Settings only takes effect by
    /// recycling the session — mirrors how a model change is handled.
    tone: String,
    /// The chat session this resident process is serving. A New conversation
    /// changes it, which recycles the session so no server-side context (Copilot
    /// retains history in-process) bleeds from the old topic into the new one.
    conversation_id: String,
}

/// Tauri state holding the warm Copilot session. Lazily created per formation,
/// reused across turns, and recycled on error, a formation/model change, or
/// after `MAX_TURNS_PER_PROCESS` turns (the #2755 degradation guard).
#[derive(Default)]
pub struct CopilotEngineHandle {
    inner: Mutex<Option<ResidentEngine>>,
}

impl CopilotEngineHandle {
    /// Run one turn on the warm session. Holding the lock across the turn also
    /// serializes turns — one in-flight `session/prompt` per session, as ACP
    /// requires.
    pub async fn run_turn(
        &self,
        turn: &TurnRequest,
        on_event: &TurnEventSink,
        model: &str,
    ) -> AppResult<TurnOutcome> {
        let binary = locate().ok_or_else(|| {
            AppError::other(
                "GitHub Copilot CLI not found. Install with `npm install -g @github/copilot`.",
            )
        })?;

        let mut guard = self.inner.lock().await;

        let need_new = match guard.as_ref() {
            None => true,
            Some(re) => {
                re.formation != turn.formation_root
                    || re.model != model
                    || re.tone != turn.tone
                    || re.conversation_id != turn.conversation_id
                    || re.session.turns.load(Ordering::SeqCst) >= MAX_TURNS_PER_PROCESS
            }
        };
        if need_new {
            if let Some(old) = guard.take() {
                old.session.shutdown().await;
            }
            let mcp_path = write_mcp_config(
                &turn.formation_root,
                &turn.source_chat_id,
                &turn.embedding_provider,
                turn.ollama_url.as_deref().unwrap_or(""),
            )?;
            let session =
                CopilotSession::spawn(&binary, &turn.formation_root, model, &mcp_path).await?;
            *guard = Some(ResidentEngine {
                session,
                formation: turn.formation_root.clone(),
                model: model.to_string(),
                tone: turn.tone.clone(),
                conversation_id: turn.conversation_id.clone(),
            });
        }

        let result = {
            let re = guard.as_ref().expect("session present after (re)create");
            let first = !re.session.persona_sent.swap(true, Ordering::SeqCst);
            let prompt = render_copilot_prompt(turn, first);
            re.session.run_turn(&prompt, on_event, &turn.cancel).await
        };

        match result {
            Ok(t) => {
                // An interrupt whose `session/cancel` wasn't acknowledged leaves
                // the resident process untrustworthy — recycle it so the next
                // turn starts clean. A clean stop keeps the session warm.
                if t.recycle {
                    if let Some(old) = guard.take() {
                        old.session.shutdown().await;
                    }
                }
                Ok(TurnOutcome {
                    reply: t.reply,
                    stop: t.stop,
                })
            }
            Err(e) => {
                // Recycle on error so the next turn starts from a clean process.
                if let Some(old) = guard.take() {
                    old.session.shutdown().await;
                }
                Err(e)
            }
        }
    }
}

/// Build the per-turn prompt. On the first turn of a session the persona and the
/// formation path are prepended (Copilot has no `--system-prompt`); the warm
/// session retains history server-side, so later turns send only this turn's
/// grounding + the new message.
fn render_copilot_prompt(turn: &TurnRequest, first: bool) -> String {
    let mut out = String::new();
    if first {
        // Tone is a parameter of the one behaviour prompt (ADR-0009 §8). The
        // persona is sent only on the first turn of a warm session; a tone
        // change recycles the session (see `need_new`) so the new persona is
        // sent next turn.
        let persona = agent_tone::render_system_prompt(
            CONVERSATION_AGENT_PROMPT,
            agent_tone::AgentTone::from_config(Some(&turn.tone)),
        );
        out.push_str(&persona);
        out.push_str("\n\n# Your formation\n\n");
        out.push_str(
            "Read and write notes only inside this folder, using absolute paths under it:\n",
        );
        out.push_str(turn.formation_root.to_string_lossy().trim());
        out.push_str("\n\n");
    }
    if let Some(ctx) = turn.injected_context.as_deref() {
        let ctx = ctx.trim();
        if !ctx.is_empty() {
            out.push_str("# What you already know\n\n");
            out.push_str(ctx);
            out.push_str("\n\n");
        }
    }
    out.push_str("# New message\n\n");
    out.push_str(turn.message.trim());
    out
}

/// Write the stdio MCP config file Copilot loads via `--additional-mcp-config`.
/// Points the `sediment` server at this same binary re-invoked with
/// `--mcp-stdio` (the existing graph-only MCP server), threading the formation
/// and provenance through env vars — mirrors the Claude Code engine's MCP config.
fn write_mcp_config(
    formation: &Path,
    source_chat_id: &str,
    embedding_provider: &str,
    ollama_url: &str,
) -> AppResult<PathBuf> {
    let self_exe =
        std::env::current_exe().map_err(|e| AppError::other(format!("current_exe: {e}")))?;
    let cfg = json!({"mcpServers":{"sediment":{
        "type":"local",
        "command": self_exe.to_string_lossy(),
        "args":["--mcp-stdio"],
        "env":{
            "SEDIMENT_FORMATION": formation.to_string_lossy(),
            "SEDIMENT_SOURCE_CHAT_ID": source_chat_id,
            "SEDIMENT_EMBEDDING_PROVIDER": embedding_provider,
            "SEDIMENT_OLLAMA_URL": ollama_url,
        },
        "tools":["*"]
    }}});
    let dir = formation.join(".chat-notes").join("copilot");
    std::fs::create_dir_all(&dir).map_err(|e| AppError::other(format!("mkdir copilot: {e}")))?;
    let path = dir.join("mcp-config.json");
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&cfg).expect("mcp config serializes"),
    )
    .map_err(|e| AppError::other(format!("write mcp config: {e}")))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `parse_models` reads the `models` block of a real `session/new` response —
    /// names, the premium-request multiplier, enablement, and the default — and
    /// tolerates a missing block without panicking.
    #[test]
    fn parse_models_reads_available_and_current() {
        let resp = json!({
            "sessionId": "x",
            "models": {
                "currentModelId": "gpt-5-mini",
                "availableModels": [
                    {"modelId":"auto","name":"Auto","description":"Let Copilot pick the best model"},
                    {"modelId":"gpt-5-mini","name":"GPT-5 mini","_meta":{"copilotUsage":"0x","copilotEnablement":"enabled"}},
                    {"modelId":"claude-haiku-4.5","name":"Claude Haiku 4.5","_meta":{"copilotUsage":"0.33x","copilotEnablement":"enabled"}}
                ]
            }
        });
        let m = parse_models(&resp);
        assert_eq!(m.current_model_id.as_deref(), Some("gpt-5-mini"));
        assert_eq!(m.available.len(), 3);
        let mini = m
            .available
            .iter()
            .find(|x| x.model_id == "gpt-5-mini")
            .expect("gpt-5-mini present");
        assert_eq!(mini.name, "GPT-5 mini");
        assert_eq!(mini.usage.as_deref(), Some("0x"));
        assert!(mini.enabled);

        // A response without a `models` block degrades to empty, never a panic.
        let empty = parse_models(&json!({ "sessionId": "x" }));
        assert!(empty.available.is_empty());
        assert!(empty.current_model_id.is_none());
    }

    #[test]
    fn ndjson_line_is_compact_and_newline_terminated() {
        let line = ndjson_line(&json!({"a":1,"b":[2,3]}));
        assert!(line.ends_with('\n'));
        assert!(!line.trim_end().contains('\n'), "no embedded newlines");
        assert_eq!(line.trim_end(), r#"{"a":1,"b":[2,3]}"#);
    }

    #[test]
    fn classify_dispatches_by_shape_not_id() {
        match classify(r#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"s1"}}"#) {
            Incoming::Response { id, result, .. } => {
                assert_eq!(id, 2);
                assert_eq!(result.unwrap()["sessionId"], "s1");
            }
            other => panic!("expected Response, got {other:?}"),
        }
        // A request FROM Copilot — id 0 collides with our client id space.
        match classify(
            r#"{"jsonrpc":"2.0","id":0,"method":"session/request_permission","params":{"options":[]}}"#,
        ) {
            Incoming::Request { id, method, .. } => {
                assert_eq!(id, 0);
                assert_eq!(method, "session/request_permission");
            }
            other => panic!("expected Request, got {other:?}"),
        }
        match classify(r#"{"jsonrpc":"2.0","method":"session/update","params":{}}"#) {
            Incoming::Notification { method, .. } => assert_eq!(method, "session/update"),
            other => panic!("expected Notification, got {other:?}"),
        }
        assert_eq!(classify("[ERROR] a stray log line"), Incoming::Other);
    }

    #[test]
    fn agent_message_chunk_becomes_text_delta_thought_is_ignored() {
        let chunk = json!({"sessionId":"s1","update":{
            "sessionUpdate":"agent_message_chunk",
            "content":{"type":"text","text":"READY"}}});
        match session_update_event(&chunk) {
            Some(TurnEvent::TextDelta { text }) => assert_eq!(text, "READY"),
            other => panic!("expected TextDelta, got {other:?}"),
        }
        let thought = json!({"update":{"sessionUpdate":"agent_thought_chunk",
            "content":{"type":"text","text":"hmm"}}});
        assert!(session_update_event(&thought).is_none());
    }

    #[test]
    fn permission_reply_picks_allow_always_with_their_id() {
        let req = json!({"options":[
            {"optionId":"allow_once","kind":"allow_once","name":"Allow once"},
            {"optionId":"allow_always","kind":"allow_always","name":"Always allow"},
            {"optionId":"reject_once","kind":"reject_once","name":"Deny"}]});
        let reply = permission_allow_msg(0, &req);
        assert_eq!(reply["id"], 0, "reply uses Copilot's request id, not ours");
        assert_eq!(reply["result"]["outcome"]["optionId"], "allow_always");
    }

    #[test]
    fn builders_are_valid_jsonrpc() {
        assert_eq!(initialize_msg(1)["method"], "initialize");
        assert_eq!(initialize_msg(1)["params"]["protocolVersion"], 1);
        assert_eq!(session_new_msg(2, "/f")["params"]["cwd"], "/f");
        let p = session_prompt_msg(3, "s1", "hi");
        assert_eq!(p["params"]["sessionId"], "s1");
        assert_eq!(p["params"]["prompt"][0]["text"], "hi");
    }

    #[test]
    fn render_prompt_includes_persona_only_on_first_turn() {
        let turn = TurnRequest {
            message: "Josh joined Stripe.".to_string(),
            history: vec![],
            formation_root: PathBuf::from("/f"),
            source_chat_id: "chat_message:1".to_string(),
            embedding_provider: "ollama".to_string(),
            ollama_url: None,
            injected_context: Some("## Currently in play".to_string()),
            tone: String::new(),
            cancel: CancellationToken::new(),
            conversation_id: String::new(),
        };
        let first = render_copilot_prompt(&turn, true);
        assert!(
            first.contains("conversational agent"),
            "persona on first turn"
        );
        assert!(first.contains("# What you already know"));
        assert!(first.trim_end().ends_with("Josh joined Stripe."));

        let later = render_copilot_prompt(&turn, false);
        assert!(
            !later.contains("conversational agent"),
            "no persona after first turn"
        );
        assert!(later.contains("# What you already know"));
    }

    /// Live: spawn a real `copilot --acp`, do the handshake, and run one warm
    /// turn — proving the resident driver end-to-end (spawn → initialize →
    /// session/new → session/prompt → streamed reply). Excluded from CI
    /// (`#[ignore]`); needs the binary + login + a little quota. Uses the
    /// 0-premium-request `gpt-5-mini` model and no MCP server (the ACP driver is
    /// what's under test here).
    #[tokio::test]
    #[ignore]
    async fn live_warm_turn() {
        let Some(binary) = locate() else {
            println!("copilot not found — skipping live_warm_turn");
            return;
        };
        let dir = std::env::temp_dir()
            .join("sediment-copilot-live")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&dir).expect("temp dir");
        let mcp = dir.join("mcp.json");
        std::fs::write(&mcp, br#"{"mcpServers":{}}"#).expect("write mcp config");

        let session = CopilotSession::spawn(&binary, &dir, "gpt-5-mini", &mcp)
            .await
            .expect("spawn + handshake");

        let collected = Arc::new(std::sync::Mutex::new(String::new()));
        let c2 = collected.clone();
        let sink: TurnEventSink = Box::new(move |ev| {
            if let TurnEvent::TextDelta { text } = ev {
                c2.lock().unwrap().push_str(&text);
            }
        });

        let reply = session
            .run_turn(
                "Reply with exactly the word READY and nothing else.",
                &sink,
                &CancellationToken::new(),
            )
            .await
            .expect("run_turn")
            .reply;
        println!("reply: {reply:?}");
        assert!(
            reply.to_uppercase().contains("READY"),
            "expected READY, got: {reply:?}"
        );
        assert_eq!(
            reply,
            *collected.lock().unwrap(),
            "streamed deltas match the accumulated reply"
        );
        session.shutdown().await;
    }
}
