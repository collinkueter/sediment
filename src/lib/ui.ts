import { create } from "zustand";

/**
 * Cross-cutting UI chrome state: which overlays are open and whether the note
 * pane is collapsed (focus mode). Kept in a store so any component can open the
 * command palette, reminders, or settings, and so the note pane can collapse
 * itself — without prop-drilling through the layout shell.
 */
interface UiState {
  notePaneCollapsed: boolean;
  paletteOpen: boolean;
  remindersOpen: boolean;
  settingsOpen: boolean;

  toggleNotePane: () => void;
  setNotePaneCollapsed: (v: boolean) => void;

  openPalette: () => void;
  closePalette: () => void;
  togglePalette: () => void;

  toggleReminders: () => void;
  closeReminders: () => void;

  openSettings: () => void;
  closeSettings: () => void;

  closeAllOverlays: () => void;
}

export const useUiStore = create<UiState>((set, get) => ({
  notePaneCollapsed: false,
  paletteOpen: false,
  remindersOpen: false,
  settingsOpen: false,

  toggleNotePane: () => set((s) => ({ notePaneCollapsed: !s.notePaneCollapsed })),
  setNotePaneCollapsed: (v) => set({ notePaneCollapsed: v }),

  openPalette: () => set({ paletteOpen: true, remindersOpen: false }),
  closePalette: () => set({ paletteOpen: false }),
  togglePalette: () => set((s) => ({ paletteOpen: !s.paletteOpen, remindersOpen: false })),

  toggleReminders: () => set((s) => ({ remindersOpen: !s.remindersOpen, paletteOpen: false })),
  closeReminders: () => set({ remindersOpen: false }),

  openSettings: () => set({ settingsOpen: true, paletteOpen: false, remindersOpen: false }),
  closeSettings: () => set({ settingsOpen: false }),

  closeAllOverlays: () => {
    const { paletteOpen, remindersOpen, settingsOpen } = get();
    if (paletteOpen || remindersOpen || settingsOpen) {
      set({ paletteOpen: false, remindersOpen: false, settingsOpen: false });
    }
  },
}));
