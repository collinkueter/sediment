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

export interface UpsertedSpan {
  text: string;
  class: string;
  probability: number;
  entity_id: string;
  was_new: boolean;
}

export interface FactWritten {
  fact_id: string;
  subject: string;
  predicate: string;
  object: string;
  confidence: number;
}

export interface ExtractFactsResult {
  entities: UpsertedSpan[];
  facts: FactWritten[];
  skipped_low_confidence: number;
  skipped_unresolved_entity: number;
}

export interface ChatWriteResult {
  source_chat_id: string;
  extraction: ExtractFactsResult;
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
};
