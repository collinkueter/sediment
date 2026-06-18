import { create } from "zustand";

/** Which primary surface fills the center hero column. */
export type WorkspaceView = "chat" | "reminders";

/**
 * Cross-cutting UI chrome state: which primary view is showing, which overlays
 * are open, and whether the note pane is collapsed (focus mode). Kept in a store
 * so any component can switch views, open the command palette or settings, and
 * so the note pane can collapse itself — without prop-drilling through the shell.
 */
interface UiState {
  notePaneCollapsed: boolean;
  paletteOpen: boolean;
  settingsOpen: boolean;
  /** The center column's primary view — Conversation or Reminders. */
  view: WorkspaceView;

  toggleNotePane: () => void;
  setNotePaneCollapsed: (v: boolean) => void;

  openPalette: () => void;
  closePalette: () => void;
  togglePalette: () => void;

  /** Switch the center column to the Conversation view. */
  showChat: () => void;
  /** Switch the center column to the Reminders view. */
  showReminders: () => void;
  /** Bell / nav toggle: flip between the Reminders view and Conversation. */
  toggleReminders: () => void;

  openSettings: () => void;
  closeSettings: () => void;

  closeAllOverlays: () => void;
}

export const useUiStore = create<UiState>((set, get) => ({
  notePaneCollapsed: false,
  paletteOpen: false,
  settingsOpen: false,
  view: "chat",

  toggleNotePane: () => set((s) => ({ notePaneCollapsed: !s.notePaneCollapsed })),
  setNotePaneCollapsed: (v) => set({ notePaneCollapsed: v }),

  openPalette: () => set({ paletteOpen: true }),
  closePalette: () => set({ paletteOpen: false }),
  togglePalette: () => set((s) => ({ paletteOpen: !s.paletteOpen })),

  showChat: () => set({ view: "chat" }),
  showReminders: () => set({ view: "reminders", paletteOpen: false }),
  toggleReminders: () =>
    set((s) => ({ view: s.view === "reminders" ? "chat" : "reminders", paletteOpen: false })),

  openSettings: () => set({ settingsOpen: true, paletteOpen: false }),
  closeSettings: () => set({ settingsOpen: false }),

  closeAllOverlays: () => {
    const { paletteOpen, settingsOpen, view } = get();
    if (paletteOpen || settingsOpen) {
      set({ paletteOpen: false, settingsOpen: false });
    } else if (view !== "chat") {
      // Esc with no overlay open returns from a secondary view to the conversation.
      set({ view: "chat" });
    }
  },
}));
