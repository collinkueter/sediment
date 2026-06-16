import { create } from "zustand";

/**
 * The visual theme. "Paper" is the warm light skin; "Strata" is the warm
 * dark skin. The choice is written to `data-theme` on <html> — the single
 * source of truth that drives both the token layer and the `dark:` variant
 * (see globals.css). Persisted to localStorage; first run seeds from the OS
 * preference.
 */
export type Theme = "paper" | "strata";

const STORAGE_KEY = "sediment.theme";

function systemTheme(): Theme {
  if (typeof window === "undefined") return "paper";
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "strata" : "paper";
}

function initialTheme(): Theme {
  if (typeof window === "undefined") return "paper";
  const saved = window.localStorage.getItem(STORAGE_KEY);
  return saved === "paper" || saved === "strata" ? saved : systemTheme();
}

function apply(theme: Theme): void {
  if (typeof document !== "undefined") {
    document.documentElement.setAttribute("data-theme", theme);
  }
}

interface ThemeState {
  theme: Theme;
  setTheme: (theme: Theme) => void;
  toggle: () => void;
}

export const useThemeStore = create<ThemeState>((set, get) => ({
  theme: initialTheme(),
  setTheme: (theme) => {
    apply(theme);
    window.localStorage.setItem(STORAGE_KEY, theme);
    set({ theme });
  },
  toggle: () => get().setTheme(get().theme === "paper" ? "strata" : "paper"),
}));

/** Write the persisted theme onto <html> as early as possible (called from main). */
export function initTheme(): void {
  apply(useThemeStore.getState().theme);
}
