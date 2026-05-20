import { Channel, invoke } from "@tauri-apps/api/core";

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

export type Tier = "Lite" | "Standard" | "Pro" | "Byok";

export interface HardwareInfo {
  total_ram_gb: number;
  chip: string;
  recommended_tier: Tier;
}

export interface OnboardingState {
  complete: boolean;
  selected_tier: string | null;
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

export type ChangeKind = "create" | "update";

export interface StagedFact {
  subject_id: string;
  subject_name: string;
  subject_type: string;
  predicate: string;
  object_id: string;
  object_name: string;
  object_type: string;
  valid_from: string;
  valid_from_explicit: boolean;
  confidence: number;
  explicit_coexist: boolean;
}

export interface Conflict {
  staged_fact_index: number;
  predicate: string;
  existing_object_id: string;
  existing_object_name: string;
  existing_valid_from: string;
  existing_source_chat_id: string;
}

/** How the user resolves a staged-fact conflict. */
export type ConflictResolution = "update" | "coexist" | "discard";

export interface NoteChange {
  kind: ChangeKind;
  note_path: string;
  diff: string;
  new_content: string;
  facts: StagedFact[];
  confidence: number;
  conflicts: Conflict[];
}

export interface StagingEntry {
  id: string;
  created: string;
  chat_message_id: string;
  chat_excerpt: string;
  status: string;
  changes: NoteChange[];
}

export interface ChatWriteResult {
  source_chat_id: string;
  /** The staging entry created for review, or null when no facts were found. */
  staged: StagingEntry | null;
}

export interface CommitResult {
  /** Id of this commit; pass to undoCommit to revert it. */
  commit_id: string;
  staging_id: string;
  /** Formation-relative paths of the notes written to disk. */
  committed_notes: string[];
  /** Record ids of the facts written to the graph (undo deletes exactly these). */
  new_fact_ids: string[];
  /** The still-staged entry when only some notes were kept, else null. */
  remaining: StagingEntry | null;
}

export interface RetrievedSource {
  note_path: string;
  chunk_idx: number;
  text: string;
  distance: number;
}

export interface ChatAskResult {
  source_chat_id: string;
  sources: RetrievedSource[];
  used_graph: boolean;
}

export interface IntentResult {
  mode: "write" | "ask";
  confidence: number;
}

export const tauri = {
  appVersion: () => invoke<string>("app_version"),

  // Formation
  pickFormationDir: () => invoke<string | null>("pick_formation_dir"),
  openFormation: (path: string) => invoke<FormationSummary>("open_formation", { path }),
  restoreLastFormation: () => invoke<FormationSummary | null>("restore_last_formation"),
  listNotes: () => invoke<FormationNote[]>("list_notes"),
  readNote: (relativePath: string) => invoke<string>("read_note", { relativePath }),
  writeNote: (relativePath: string, content: string) =>
    invoke<void>("write_note", { relativePath, content }),
  indexFormation: (force: boolean) => invoke<IndexFormationResult>("index_formation", { force }),

  // Hardware + onboarding
  detectHardware: () => invoke<HardwareInfo>("detect_hardware"),
  getOnboardingState: () => invoke<OnboardingState>("get_onboarding_state"),
  completeOnboarding: (tier: string) => invoke<void>("complete_onboarding", { tier }),

  // Ollama
  ollamaStatus: () => invoke<OllamaStatus>("ollama_status"),
  ollamaEnsureRunning: () => invoke<OllamaStatus>("ollama_ensure_running"),
  ollamaListModels: () => invoke<ModelSummary[]>("ollama_list_models"),
  /** Stream tokens through `onToken`; resolves when the stream ends. */
  ollamaGenerate: (model: string, prompt: string, onToken: (t: string) => void) => {
    const channel = new Channel<string>();
    channel.onmessage = onToken;
    return invoke<void>("ollama_generate", { model, prompt, onToken: channel });
  },

  // Chat
  chatWrite: (message: string, sessionId: string) =>
    invoke<ChatWriteResult>("chat_write", { message, sessionId }),
  /** Stream the cited answer through `onToken`; resolves with retrieved sources. */
  chatAsk: (query: string, sessionId: string, onToken: (t: string) => void) => {
    const channel = new Channel<string>();
    channel.onmessage = onToken;
    return invoke<ChatAskResult>("chat_ask", { query, sessionId, onToken: channel });
  },
  classifyIntent: (message: string) => invoke<IntentResult>("classify_intent", { message }),

  // Staging tray
  listStaging: () => invoke<StagingEntry[]>("list_staging"),
  getStaging: (id: string) => invoke<StagingEntry>("get_staging", { id }),
  discardStaging: (id: string) => invoke<void>("discard_staging", { id }),
  updateStaging: (entry: StagingEntry) => invoke<void>("update_staging", { entry }),
  /** Resolve a staged-fact conflict (update / coexist / discard). */
  resolveConflict: (
    stagingId: string,
    notePath: string,
    stagedFactIndex: number,
    resolution: ConflictResolution,
  ) => invoke<void>("resolve_conflict", { stagingId, notePath, stagedFactIndex, resolution }),
  /** Commit a staging entry. Pass `notePaths` to keep only those notes. */
  keepStaging: (id: string, notePaths?: string[]) =>
    invoke<CommitResult>("keep_staging", { id, notePaths: notePaths ?? null }),
  /** Revert a commit within the undo window: restores notes, deletes facts. */
  undoCommit: (commitId: string) => invoke<void>("undo_commit", { commitId }),
};
