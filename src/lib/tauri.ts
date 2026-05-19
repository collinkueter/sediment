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
};
