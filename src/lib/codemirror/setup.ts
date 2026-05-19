import { markdown } from "@codemirror/lang-markdown";
import type { Extension } from "@codemirror/state";
import { oneDark } from "@codemirror/theme-one-dark";

export const markdownExtensions: Extension[] = [markdown()];

// Re-export so consumers can switch themes from a single import site.
export { oneDark };
