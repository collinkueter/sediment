import { markdown } from "@codemirror/lang-markdown";
import type { Extension } from "@codemirror/state";
import { oneDark } from "@codemirror/theme-one-dark";
import { EditorView } from "@codemirror/view";

export const markdownExtensions: Extension[] = [markdown()];

/**
 * "Paper" CodeMirror theme — the warm light skin that pairs with the Paper app
 * theme. Colours are driven entirely by token CSS variables so the editor
 * tracks the rest of the chrome. Strata (dark) keeps using {@link oneDark}.
 */
export const paperTheme: Extension = EditorView.theme(
  {
    "&": {
      color: "var(--ink)",
      backgroundColor: "transparent",
    },
    ".cm-content": {
      caretColor: "var(--accent)",
      fontFamily: "var(--font-mono, ui-monospace, monospace)",
    },
    ".cm-cursor, .cm-dropCursor": {
      borderLeftColor: "var(--accent)",
    },
    "&.cm-focused .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection": {
      backgroundColor: "var(--accent-tint)",
    },
    ".cm-gutters": {
      backgroundColor: "transparent",
      color: "var(--muted)",
      border: "none",
    },
    ".cm-activeLine": {
      backgroundColor: "transparent",
    },
    ".cm-activeLineGutter": {
      backgroundColor: "transparent",
    },
  },
  { dark: false },
);

// Re-export so consumers can switch themes from a single import site.
export { oneDark };
