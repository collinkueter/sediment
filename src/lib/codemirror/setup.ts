import { markdown } from "@codemirror/lang-markdown";
import { unifiedMergeView } from "@codemirror/merge";
import type { Extension } from "@codemirror/state";
import { oneDark } from "@codemirror/theme-one-dark";

export const markdownExtensions: Extension[] = [markdown()];

/// Markdown editing plus a unified merge view against `original`. The editor
/// document is the proposed (staged) note; `original` is the on-disk version.
/// Each changed chunk gets accept/reject gutter controls.
export function diffExtensions(original: string): Extension[] {
  return [
    markdown(),
    unifiedMergeView({
      original,
      mergeControls: true,
      collapseUnchanged: { margin: 3, minSize: 4 },
    }),
  ];
}

// Re-export so consumers can switch themes from a single import site.
export { oneDark };
