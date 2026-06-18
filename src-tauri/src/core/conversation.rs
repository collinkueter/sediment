//! The `ConversationEngine` abstraction — ADR-0009 §5, plan M3.
//!
//! ADR-0009 collapses the old Write/Ask command bus into a single
//! conversational **agent loop**. Each turn the agent grounds itself, records
//! what it learns, and replies. *Who runs that loop* is abstracted behind the
//! [`ConversationEngine`] trait. The cold-spawn engines implement it directly —
//! V1 ships the Claude Code CLI ([`crate::core::claude_code::ClaudeCodeEngine`]).
//! The warm GitHub Copilot ACP engine (ADR-0012) holds a resident session across
//! turns ([`crate::core::copilot::CopilotEngineHandle`]) and so lives in Tauri
//! state and is routed separately by `chat_turn` rather than through this trait.
//!
//! The trait is deliberately **Tauri-agnostic**: streamed events flow through a
//! plain [`TurnEventSink`] closure, not a `tauri::ipc::Channel`. M4's `chat_turn`
//! command wraps a `Channel` in a `TurnEventSink` at the edge, so the engine
//! layer never depends on Tauri's IPC types and stays unit-testable.
//!
//! Sediment owns the conversation transcript (it lives in `chat_message`, not in
//! engine session files — ADR-0009 §5). A [`TurnRequest`] therefore carries a
//! recent window of prior turns; the engine renders that window into whatever
//! shape its CLI/API wants and feeds it alongside the new message.
//!
//! The production caller is the `chat_turn` command (`commands::chat`).

use crate::error::AppResult;
use async_trait::async_trait;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

/// One prior turn of conversation.
///
/// Oldest-first history is a `Vec<TranscriptTurn>`. `role` is `"user"` or
/// `"assistant"` — a plain `String` rather than an enum so the type stays
/// trivially serializable and the engine layer imposes no schema beyond what
/// `chat_message` already records.
pub struct TranscriptTurn {
    /// `"user"` or `"assistant"`.
    pub role: String,
    /// The verbatim message text for that turn.
    pub content: String,
}

/// Everything an engine needs to run one conversational turn.
pub struct TurnRequest {
    /// The new user message that triggered this turn.
    pub message: String,
    /// Recent prior turns, oldest first — the conversational continuity window
    /// (ADR-0009 §5: ≈ the last 10–20 turns). Older context is the agent's job
    /// to pull with its own tools.
    pub history: Vec<TranscriptTurn>,
    /// The engine's working directory: the formation root. Claude Code's native
    /// file tools operate here; the MCP server is pointed at it too.
    pub formation_root: PathBuf,
    /// The user message's `chat_message` id — stamped as provenance on every
    /// graph Fact the turn records (passed to the MCP server via env var).
    pub source_chat_id: String,
    /// The note-search backend (`"ollama"` or `"none"`), forwarded to the MCP
    /// subprocess via `SEDIMENT_EMBEDDING_PROVIDER` so `search_notes` matches
    /// the user's choice. See `core::embedding::EmbeddingProvider`.
    pub embedding_provider: String,
    /// Custom Ollama endpoint (Docker/Podman/remote) forwarded to the MCP
    /// subprocess via `SEDIMENT_OLLAMA_URL` so `search_notes` embeds against the
    /// same Ollama the user configured. `None` uses the local default. See
    /// `core::ollama_sidecar`.
    pub ollama_url: Option<String>,
    /// Deterministic grounding the orchestrator pushes into the turn *before* the
    /// agent runs (ADR-0011): resolved entities + their current facts, the top
    /// related notes, and the Working Set — pre-rendered as one Markdown block.
    /// The engine splices it into the prompt; `None` means no pre-pass ran.
    pub injected_context: Option<String>,
    /// The Agent's conversational tone (`"stoic"` / `"warm"` / `"sassy"`),
    /// parsed by `core::agent_tone::AgentTone::from_config` and spliced into the
    /// behaviour prompt's `## Tone` section. An empty string means the default
    /// (warm). Reply wording only — never affects what the turn records.
    pub tone: String,
    /// Tripped when the user interrupts this turn (Steer or Redirect). Engines
    /// watch it in their stream loop and stop promptly, returning whatever they
    /// produced so far as a [`TurnStop::Interrupted`] outcome. The engine never
    /// learns *why* it was stopped — `chat_turn` decides keep-vs-revert from the
    /// cancel mode it recorded separately.
    pub cancel: CancellationToken,
    /// The chat session this turn belongs to. The warm Copilot engine recycles
    /// its resident ACP session when this changes (a New conversation), so no
    /// server-side context bleeds across topics. Cold engines ignore it — they
    /// render history from the transcript window each turn.
    pub conversation_id: String,
}

/// A streamed event during a turn.
///
/// Engines emit these through the [`TurnEventSink`] as the turn progresses, so
/// the UI can render the reply token-by-token and show an inline trail of the
/// agent's tool activity.
///
/// `TurnEvent` is `Serialize` so the `chat_turn` command can forward it
/// straight onto a Tauri [`tauri::ipc::Channel`]. The serde shape is an
/// **internally-tagged** object — a `kind` discriminator plus the variant's
/// fields — so the frontend receives a plain discriminated union:
///
/// - text delta → `{ "kind": "textDelta", "text": "Sarah " }`
/// - tool activity → `{ "kind": "toolActivity", "tool": "Edit", "summary": "Edit People/Josh.md" }`
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TurnEvent {
    /// A chunk of the assistant's reply text.
    TextDelta {
        /// The token text to append to the streamed reply.
        text: String,
    },
    /// The agent used a tool — surfaced as a human-readable line in the UI
    /// activity trail (*"searched your notes"*, *"filed Josh → works_at → …"*).
    ToolActivity {
        /// The tool name (e.g. `Edit`, `mcp__sediment__record_fact`).
        tool: String,
        /// A short human phrase describing the call — the tool plus a key
        /// argument.
        summary: String,
    },
}

/// Where streamed [`TurnEvent`]s go.
///
/// A boxed closure rather than a channel type: M4 wraps a Tauri `Channel` in
/// one of these, and the deterministic tests wrap a `Vec`-collector. The engine
/// only ever calls it — it never inspects the sink.
pub type TurnEventSink = Box<dyn Fn(TurnEvent) + Send + Sync>;

/// How a turn stopped.
///
/// `Completed` is the normal end. `Interrupted` means the user tripped
/// [`TurnRequest::cancel`] and the engine stopped early — `reply` then holds
/// whatever streamed so far. `chat_turn` maps `Interrupted` to either a kept
/// (Steer) or reverted (Redirect) turn based on the recorded cancel mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnStop {
    Completed,
    Interrupted,
}

/// The result of a turn.
pub struct TurnOutcome {
    /// The assistant reply text. For a completed turn this is the authoritative
    /// final answer; for an interrupted turn it is the partial reply streamed
    /// before the stop.
    pub reply: String,
    /// How the turn stopped — normal completion or a user interrupt.
    pub stop: TurnStop,
}

/// Runs the agent loop for one conversational turn.
///
/// Implemented by the cold-spawn engines — V1's is
/// [`crate::core::claude_code::ClaudeCodeEngine`]. The warm Copilot engine
/// (ADR-0012) is routed separately by `chat_turn`, not through this trait.
/// `run_turn` is `&self` so an engine can be shared across turns; per-turn
/// state lives entirely in the [`TurnRequest`].
#[async_trait]
pub trait ConversationEngine: Send + Sync {
    /// Run one turn: ground, record, reply. Streams [`TurnEvent`]s through
    /// `on_event` as they happen and returns the [`TurnOutcome`] when the turn
    /// completes.
    async fn run_turn(
        &self,
        turn: &TurnRequest,
        on_event: &TurnEventSink,
    ) -> AppResult<TurnOutcome>;
}
