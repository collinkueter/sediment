import { Channel, invoke as realInvoke } from "@tauri-apps/api/core";
import { browserMock, isBrowserMock } from "./devMock";

// In a plain browser dev session (no Tauri runtime) route commands to a mock so
// the UI can be previewed with representative data. No-op in the real app and
// stripped from production builds. See devMock.ts.
const invoke: typeof realInvoke = isBrowserMock
  ? (browserMock as unknown as typeof realInvoke)
  : realInvoke;

// Real `Channel` needs Tauri internals; under the browser mock use a plain
// stand-in so streaming commands can be constructed (they resolve without
// streaming). No effect in the real app.
function makeChannel<T>(): Channel<T> {
  return isBrowserMock ? ({ onmessage: undefined } as unknown as Channel<T>) : new Channel<T>();
}

export interface FormationNote {
  relative_path: string;
  modified_secs: number;
}

export interface FormationSummary {
  path: string;
  note_count: number;
}

export interface OllamaStatus {
  installed: boolean;
  running: boolean;
  install_hint: string | null;
}

export interface ModelSummary {
  name: string;
  size: number;
  modified_at: string;
}

export interface OnboardingState {
  complete: boolean;
}

export interface IndexFormationResult {
  total: number;
  indexed: number;
  skipped: number;
  failed: number;
}

export interface IndexProgress {
  done: number;
  total: number;
  current_path: string;
}

export interface ModelRequirement {
  /** "embed" — the only local model class after ADR-0009. */
  kind: "embed";
  /** Ollama pull tag. */
  id: string;
  label: string;
  size_hint: string;
  present: boolean;
}

export interface ModelReadiness {
  ollama_installed: boolean;
  requirements: ModelRequirement[];
  all_present: boolean;
}

export interface ModelProgress {
  model: string;
  phase: string;
  completed: number;
  total: number;
  done: boolean;
}

/**
 * A streamed event during one conversational turn (ADR-0009 §5). The Channel
 * delivers an internally-tagged discriminated union keyed on `kind`.
 */
export type TurnEvent =
  /** A chunk of the assistant's reply text to append to the in-progress bubble. */
  | { kind: "textDelta"; text: string }
  /** The agent used a tool — surfaced as a line in the inline activity trail. */
  | { kind: "toolActivity"; tool: string; summary: string };

/** A note one `chat_turn` changed, learned by diffing the pre-turn snapshot. */
export interface ChangedNote {
  /** Formation-relative POSIX path of the note. */
  path: string;
  /** True when the turn created the note (it did not exist in the snapshot). */
  wasCreate: boolean;
}

/** An entity currently "in play" (ADR-0011 §3). */
export interface ActiveEntity {
  name: string;
  entityType: string;
  notePath: string | null;
}

/** An open task surfaced in the Working Set. */
export interface OpenTask {
  title: string;
  /** Due date `YYYY-MM-DD`, or null. */
  due: string | null;
}

/** An unresolved thread the agent noticed (ADR-0011 §5). */
export interface OpenLoop {
  /** `open_loop:<id>` — the handle `dismissOpenLoop` targets. */
  id: string;
  title: string;
  context: string | null;
}

/** What's currently in play — the derived Working Set (ADR-0011 §3). */
export interface WorkingSet {
  activeEntities: ActiveEntity[];
  recentNotes: string[];
  openTasks: OpenTask[];
  openLoops: OpenLoop[];
}

/** What one `chat_turn` produced, returned when the turn completes. */
export interface ChatTurnResult {
  /** The audit-entry id for this turn — the handle used to revert it. */
  turnId: string;
  /** The full assistant reply (also streamed token-by-token over `onEvent`). */
  reply: string;
  /** Notes the turn changed. */
  changedNotes: ChangedNote[];
  /** How many graph Facts the turn recorded through the MCP server. */
  recordedFactCount: number;
  /** The Working Set as of this turn — drives the "what's in play" panel. */
  workingSet: WorkingSet;
}

/** A chat-turn audit entry (ADR-0009 §6). */
export interface ChatTurnAuditEntry {
  kind: "chatTurn";
  /** Stable id; the handle the audit panel uses to revert the turn. */
  turnId: string;
  /** RFC3339 timestamp the turn ran. */
  created: string;
  /** First chars of the user message. */
  userExcerpt: string;
  /** First chars of the assistant reply. */
  replyExcerpt: string;
  /** Formation-relative path of the pre-turn snapshot directory. */
  snapshotDir: string;
  /** Notes the turn changed. */
  changedNotes: ChangedNote[];
  /** Graph Fact record ids the turn recorded — per-Fact revert targets these. */
  recordedFactIds: string[];
}

/** A task-completion audit entry — indexer-driven daily-note append (ADR-0010 §8). */
export interface TaskCompletionAuditEntry {
  kind: "taskCompletion";
  /** Stable id; the handle used to revert this append. */
  entryId: string;
  /** RFC3339 timestamp the append ran. */
  created: string;
  /** The `task` record id whose open→done transition triggered this append. */
  taskId: string;
  /** Task title at the moment the box was checked. */
  taskTitle: string;
  /** Formation-relative POSIX path of the daily note that was appended. */
  dailyNotePath: string;
  /** The verbatim bullet line that was added, e.g. `- Called the dentist`. */
  appendedBulletText: string;
}

/** One audit-log entry — chat turn OR task completion (ADR-0009 §6, ADR-0010 §8). */
export type AuditEntry = ChatTurnAuditEntry | TaskCompletionAuditEntry;

/** Result of `undo_task_completion` — the toast uses this to handle the edited case. */
export type UndoTaskCompletionResult = "removed" | "editedSinceAppended" | "fileMissing";

/** Payload of the `daily-note-appended` Tauri event (ADR-0010 §8). */
export interface DailyNoteAppendedPayload {
  /** `task_completion` audit entry id — the handle the toast uses to undo. */
  entryId: string;
  /** The completed task's id. */
  taskId: string;
  /** Formation-relative POSIX path of the daily note that was appended. */
  dailyNotePath: string;
  /** The verbatim bullet line that was added. */
  bulletText: string;
}

/** Result of detect_claude_code — reflects the locally-installed Claude Code CLI. */
export interface ClaudeCodeStatus {
  installed: boolean;
  binary_path: string | null;
  logged_in: boolean;
  /** "claude.ai" for a subscription login. */
  auth_method: string | null;
  /** "max" | "pro" | ... */
  subscription_type: string | null;
  email: string | null;
}

/** Result of detect_copilot — reflects the locally-installed GitHub Copilot CLI. */
export interface CopilotStatus {
  installed: boolean;
  binary_path: string | null;
}

/** Persisted conversational-engine selection (ADR-0009 §5). */
export interface ConversationEngineConfig {
  /** "claude-code" (default) | "copilot" */
  engine: string;
  claude_code_model: string | null;
  copilot_model: string | null;
}

export type TaskStatus = "open" | "done";

/** A reminder — the scheduling-side mirror of a Tasks.md checklist line. */
export interface Task {
  id: string;
  title: string;
  status: TaskStatus;
  /** RFC3339 due timestamp, or null. */
  due: string | null;
  /** RFC3339 — when the reminder fires. Null when the task has no reminder. */
  remind_at: string | null;
  notified: boolean;
  created: string;
  completed_at: string | null;
  source_chat_id: string | null;
}

export const tauri = {
  appVersion: () => invoke<string>("app_version"),

  // Formation
  pickFormationDir: () => invoke<string | null>("pick_formation_dir"),
  /** Native folder picker for an arbitrary directory (e.g. the models dir). */
  pickDirectory: () => invoke<string | null>("pick_directory"),
  openFormation: (path: string) => invoke<FormationSummary>("open_formation", { path }),
  restoreLastFormation: () => invoke<FormationSummary | null>("restore_last_formation"),
  listNotes: () => invoke<FormationNote[]>("list_notes"),
  readNote: (relativePath: string) => invoke<string>("read_note", { relativePath }),
  writeNote: (relativePath: string, content: string) =>
    invoke<void>("write_note", { relativePath, content }),
  indexFormation: (force: boolean) => invoke<IndexFormationResult>("index_formation", { force }),

  // Onboarding
  getOnboardingState: () => invoke<OnboardingState>("get_onboarding_state"),
  completeOnboarding: () => invoke<void>("complete_onboarding"),

  // Model provisioning
  checkModelReadiness: () => invoke<ModelReadiness>("check_model_readiness"),
  /** Pull an Ollama model, streaming progress through `onProgress`. */
  pullOllamaModel: (model: string, onProgress: (p: ModelProgress) => void) => {
    const channel = makeChannel<ModelProgress>();
    channel.onmessage = onProgress;
    return invoke<void>("pull_ollama_model", { model, onProgress: channel });
  },

  // Ollama
  ollamaStatus: () => invoke<OllamaStatus>("ollama_status"),
  ollamaEnsureRunning: () => invoke<OllamaStatus>("ollama_ensure_running"),
  ollamaListModels: () => invoke<ModelSummary[]>("ollama_list_models"),

  // Chat — one conversational turn (ADR-0009)
  /**
   * Run one conversational turn. Streams `TurnEvent`s through `onEvent` —
   * `textDelta` chunks of the reply and `toolActivity` lines — and resolves
   * with the turn's authoritative outcome when it completes.
   */
  chatTurn: (message: string, sessionId: string, onEvent: (e: TurnEvent) => void) => {
    const channel = makeChannel<TurnEvent>();
    channel.onmessage = onEvent;
    return invoke<ChatTurnResult>("chat_turn", { message, sessionId, onEvent: channel });
  },
  /** The current Working Set for the "what's in play" panel (ADR-0011 §3). */
  getWorkingSet: () => invoke<WorkingSet>("get_working_set"),
  /** The Self summary for the "in focus" panel — `## Summary` of `Self.md` (ADR-0015 §5). */
  getSelfSummary: () => invoke<string | null>("get_self_summary"),
  /** Dismiss an open loop so it stops surfacing (ADR-0011 §5). */
  dismissOpenLoop: (loopId: string) => invoke<void>("dismiss_open_loop", { loopId }),

  // Audit log — the browsable, revertable backstop (ADR-0009 §6, ADR-0010 §8)
  /** Every turn's audit entry, newest-first. */
  listAudit: () => invoke<AuditEntry[]>("list_audit"),
  /** Revert a whole turn: restore changed notes, delete recorded Facts. */
  undoTurn: (turnId: string) => invoke<void>("undo_turn", { turnId }),
  /** Revert one recorded Fact from a turn, leaving its notes and other Facts. */
  undoFact: (turnId: string, factId: string) => invoke<void>("undo_fact", { turnId, factId }),
  /** Revert one task-completion append from the daily note (ADR-0010 §8). */
  undoTaskCompletion: (entryId: string) =>
    invoke<UndoTaskCompletionResult>("undo_task_completion", { entryId }),

  // Settings (the conversational-engine selector — ADR-0009 §5)
  detectClaudeCode: () => invoke<ClaudeCodeStatus>("detect_claude_code"),
  detectCopilot: () => invoke<CopilotStatus>("detect_copilot"),
  getConversationEngine: () => invoke<ConversationEngineConfig>("get_conversation_engine"),
  setConversationEngine: (engine: string, model: string | null) =>
    invoke<void>("set_conversation_engine", { engine, model }),
  /** The Agent's conversational tone: "stoic" | "warm" | "sassy". */
  getAgentTone: () => invoke<string>("get_agent_tone"),
  /** Persist the Agent's tone ("stoic" | "warm" | "sassy"); applies next turn. */
  setAgentTone: (tone: string) => invoke<void>("set_agent_tone", { tone }),
  /** The shared models directory, or null for Ollama's default. */
  getModelsDir: () => invoke<string | null>("get_models_dir"),
  /** Set the shared models directory; null/empty clears it back to default. */
  setModelsDir: (dir: string | null) => invoke<void>("set_models_dir", { dir }),
  /** Note-search backend: "ollama" | "bundled" (in-process) | "none" (keyword). */
  getEmbeddingProvider: () => invoke<string>("get_embedding_provider"),
  /** Persist the note-search backend ("ollama" | "bundled" | "none"). */
  setEmbeddingProvider: (provider: string) => invoke<void>("set_embedding_provider", { provider }),
  /** Eagerly load/download the in-process embedding model (no-op unless bundled). */
  warmupEmbeddingModel: () => invoke<void>("warmup_embedding_model"),

  // Tasks & reminders (ADR-0007)
  listTasks: () => invoke<Task[]>("list_tasks"),
  completeTask: (id: string) => invoke<void>("complete_task", { id }),
  /** Push a task's reminder to `until` (an RFC3339 timestamp). */
  snoozeTask: (id: string, until: string) => invoke<void>("snooze_task", { id, until }),
};
