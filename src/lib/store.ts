import { type FormationNote, tauri } from "@/lib/tauri";
import { create } from "zustand";

export type ChatRole = "user" | "assistant" | "system";

export interface ChatMessage {
  id: string;
  role: ChatRole;
  content: string;
  createdAt: number;
}

interface ChatState {
  /** Stable id for this app-launch chat session; provenance for stored facts. */
  sessionId: string;
  messages: ChatMessage[];
  /** Adds a message and returns its id so callers can stream into it. */
  appendMessage: (msg: Omit<ChatMessage, "id" | "createdAt">) => string;
  /** Append a token chunk to an existing message (used during streaming). */
  appendToken: (id: string, token: string) => void;
  /** Overwrite a message's content (used for errors or completion markers). */
  setMessageContent: (id: string, content: string) => void;
  clear: () => void;
}

export const useChatStore = create<ChatState>((set) => ({
  sessionId: crypto.randomUUID(),
  messages: [],
  appendMessage: (msg) => {
    const id = crypto.randomUUID();
    set((state) => ({
      messages: [...state.messages, { ...msg, id, createdAt: Date.now() }],
    }));
    return id;
  },
  appendToken: (id, token) =>
    set((state) => ({
      messages: state.messages.map((m) => (m.id === id ? { ...m, content: m.content + token } : m)),
    })),
  setMessageContent: (id, content) =>
    set((state) => ({
      messages: state.messages.map((m) => (m.id === id ? { ...m, content } : m)),
    })),
  clear: () => set({ messages: [] }),
}));

interface UiState {
  stagingTrayOpen: boolean;
  toggleStagingTray: () => void;
  setStagingTrayOpen: (open: boolean) => void;
}

export const useUiStore = create<UiState>((set) => ({
  stagingTrayOpen: false,
  toggleStagingTray: () => set((s) => ({ stagingTrayOpen: !s.stagingTrayOpen })),
  setStagingTrayOpen: (open) => set({ stagingTrayOpen: open }),
}));

interface FormationState {
  formationPath: string | null;
  noteCount: number;
  notes: FormationNote[];
  currentNotePath: string | null;
  currentNoteContent: string;
  isDirty: boolean;
  loading: boolean;

  /** Try to restore the last formation on launch. Safe to call multiple times. */
  restore: () => Promise<void>;
  /** Show native picker, then open the chosen folder. */
  pick: () => Promise<void>;
  /** Open a specific folder as the formation. */
  open: (path: string) => Promise<void>;
  refreshNotes: () => Promise<void>;
  openNote: (relativePath: string) => Promise<void>;
  setContent: (content: string) => void;
  save: () => Promise<void>;
  closeFormation: () => void;

  /** Handle a debounced file-watcher event from the Rust core. */
  handleExternalChange: (paths: string[]) => Promise<void>;
}

export const useFormationStore = create<FormationState>((set, get) => ({
  formationPath: null,
  noteCount: 0,
  notes: [],
  currentNotePath: null,
  currentNoteContent: "",
  isDirty: false,
  loading: false,

  async restore() {
    set({ loading: true });
    try {
      const summary = await tauri.restoreLastFormation();
      if (summary) {
        set({ formationPath: summary.path, noteCount: summary.note_count });
        await get().refreshNotes();
      }
    } finally {
      set({ loading: false });
    }
  },

  async pick() {
    const path = await tauri.pickFormationDir();
    if (path) await get().open(path);
  },

  async open(path) {
    set({ loading: true });
    try {
      const summary = await tauri.openFormation(path);
      set({
        formationPath: summary.path,
        noteCount: summary.note_count,
        currentNotePath: null,
        currentNoteContent: "",
        isDirty: false,
      });
      await get().refreshNotes();
      // Kick off a background formation re-index (skips unchanged files).
      // Not awaited — progress arrives via `index-progress` events.
      tauri.indexFormation(false).catch((e) => console.warn("background index failed:", e));
    } finally {
      set({ loading: false });
    }
  },

  async refreshNotes() {
    const notes = await tauri.listNotes();
    set({ notes, noteCount: notes.length });
  },

  async openNote(relativePath) {
    const content = await tauri.readNote(relativePath);
    set({
      currentNotePath: relativePath,
      currentNoteContent: content,
      isDirty: false,
    });
  },

  setContent(content) {
    set((state) => ({
      currentNoteContent: content,
      isDirty: content !== state.currentNoteContent ? true : state.isDirty,
    }));
  },

  async save() {
    const { currentNotePath, currentNoteContent } = get();
    if (!currentNotePath) return;
    await tauri.writeNote(currentNotePath, currentNoteContent);
    set({ isDirty: false });
    await get().refreshNotes();
  },

  closeFormation() {
    set({
      formationPath: null,
      noteCount: 0,
      notes: [],
      currentNotePath: null,
      currentNoteContent: "",
      isDirty: false,
    });
  },

  async handleExternalChange(paths) {
    await get().refreshNotes();
    const { currentNotePath, isDirty } = get();
    if (currentNotePath && paths.includes(currentNotePath) && !isDirty) {
      // Reload the active note from disk so we don't show stale content.
      try {
        const content = await tauri.readNote(currentNotePath);
        set({ currentNoteContent: content });
      } catch (e) {
        // File may have been deleted; clear it.
        console.warn("reload after external change failed:", e);
        set({ currentNotePath: null, currentNoteContent: "" });
      }
    }
  },
}));
