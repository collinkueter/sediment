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

export interface ModelRequirement {
  kind: "chat" | "embed" | "gliner";
  /** Ollama pull tag for chat/embed; "gliner" for the ONNX model. */
  id: string;
  label: string;
  size_hint: string;
  present: boolean;
}

export interface ModelReadiness {
  tier: string;
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

/** A freshly-mentioned entity that closely matches one already in the graph. */
export interface DisambiguationSuggestion {
  staged_fact_index: number;
  /** Which endpoint of the fact is the near-match: "subject" or "object". */
  endpoint: string;
  mention_name: string;
  candidate_id: string;
  candidate_name: string;
  candidate_type: string;
  candidate_note_path: string | null;
  /** Trigram name similarity in [0,1]. */
  score: number;
}

export interface NoteChange {
  kind: ChangeKind;
  note_path: string;
  diff: string;
  new_content: string;
  facts: StagedFact[];
  confidence: number;
  conflicts: Conflict[];
  suggestions: DisambiguationSuggestion[];
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
  /** Relations the extractor proposed but dropped below the confidence floor. */
  skipped_low_confidence: number;
  /** Relations whose subject or object entity was never surfaced. */
  skipped_unresolved: number;
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

/** BYOK cloud-provider config. The API key is never sent to the front end. */
export interface ByokConfig {
  /** "anthropic" | "openai", or null for local generation. */
  provider: string | null;
  model: string | null;
  /** Whether an API key is stored for the provider. */
  has_key: boolean;
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

  // Model provisioning
  checkModelReadiness: () => invoke<ModelReadiness>("check_model_readiness"),
  /** Pull an Ollama model, streaming progress through `onProgress`. */
  pullOllamaModel: (model: string, onProgress: (p: ModelProgress) => void) => {
    const channel = new Channel<ModelProgress>();
    channel.onmessage = onProgress;
    return invoke<void>("pull_ollama_model", { model, onProgress: channel });
  },
  /** Download the GLiNER model into the open formation, streaming progress. */
  downloadGlinerModel: (onProgress: (p: ModelProgress) => void) => {
    const channel = new Channel<ModelProgress>();
    channel.onmessage = onProgress;
    return invoke<void>("download_gliner_model", { onProgress: channel });
  },

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
  /** Accept a "did you mean?" suggestion — merge into the matched entity. */
  applyDisambiguation: (
    stagingId: string,
    notePath: string,
    stagedFactIndex: number,
    endpoint: string,
  ) => invoke<void>("apply_disambiguation", { stagingId, notePath, stagedFactIndex, endpoint }),
  /** Dismiss a "did you mean?" suggestion — keep the entity as genuinely new. */
  dismissDisambiguation: (
    stagingId: string,
    notePath: string,
    stagedFactIndex: number,
    endpoint: string,
  ) => invoke<void>("dismiss_disambiguation", { stagingId, notePath, stagedFactIndex, endpoint }),
  /** Commit a staging entry. Pass `notePaths` to keep only those notes. */
  keepStaging: (id: string, notePaths?: string[]) =>
    invoke<CommitResult>("keep_staging", { id, notePaths: notePaths ?? null }),
  /** Revert a commit within the undo window: restores notes, deletes facts. */
  undoCommit: (commitId: string) => invoke<void>("undo_commit", { commitId }),

  // Settings (BYOK)
  getByokConfig: () => invoke<ByokConfig>("get_byok_config"),
  /**
   * Save the BYOK config. `provider` null clears BYOK. When `provider` is set,
   * the stored key is replaced only if `apiKey` is a non-empty string.
   */
  setByokConfig: (provider: string | null, model: string | null, apiKey: string | null) =>
    invoke<void>("set_byok_config", { provider, model, apiKey }),
};
