import { markdown } from "@codemirror/lang-markdown";
import { unifiedMergeView } from "@codemirror/merge";
import type { Extension } from "@codemirror/state";
import { oneDark } from "@codemirror/theme-one-dark";
import { EditorView } from "@uiw/react-codemirror";

export const markdownExtensions: Extension[] = [markdown()];

/// `@codemirror/merge` ships its per-chunk accept/reject controls absolutely
/// positioned — so they overlap the document text — and painted as saturated
/// green/red blocks. This theme drops the toolbar into the layout flow above
/// each chunk and tints the buttons to match the app's zinc/emerald/red palette.
function mergeControlsTheme(dark: boolean): Extension {
  const accept = dark
    ? {
        fg: "#6ee7b7",
        border: "rgba(5,150,105,0.45)",
        bg: "rgba(6,78,59,0.35)",
        hover: "rgba(6,78,59,0.6)",
      }
    : { fg: "#047857", border: "#a7f3d0", bg: "#ecfdf5", hover: "#d1fae5" };
  const reject = dark
    ? {
        fg: "#fca5a5",
        border: "rgba(220,38,38,0.45)",
        bg: "rgba(127,29,29,0.35)",
        hover: "rgba(127,29,29,0.6)",
      }
    : { fg: "#b91c1c", border: "#fecaca", bg: "#fef2f2", hover: "#fee2e2" };
  return EditorView.theme({
    ".cm-deletedChunk .cm-chunkButtons": {
      position: "static",
      display: "flex",
      justifyContent: "flex-end",
      gap: "4px",
      padding: "4px 8px",
    },
    ".cm-deletedChunk .cm-chunkButtons button": {
      border: "1px solid transparent",
      borderRadius: "4px",
      padding: "1px 8px",
      margin: "0",
      font: "inherit",
      fontSize: "11px",
      fontWeight: "500",
      lineHeight: "1.5",
      cursor: "pointer",
      backgroundColor: "transparent",
    },
    ".cm-deletedChunk .cm-chunkButtons button[name=accept]": {
      color: accept.fg,
      borderColor: accept.border,
      backgroundColor: accept.bg,
    },
    ".cm-deletedChunk .cm-chunkButtons button[name=accept]:hover": {
      backgroundColor: accept.hover,
    },
    ".cm-deletedChunk .cm-chunkButtons button[name=reject]": {
      color: reject.fg,
      borderColor: reject.border,
      backgroundColor: reject.bg,
    },
    ".cm-deletedChunk .cm-chunkButtons button[name=reject]:hover": {
      backgroundColor: reject.hover,
    },
  });
}

/// Markdown editing plus a unified merge view against `original`. The editor
/// document is the proposed (staged) note; `original` is the on-disk version.
/// Each changed chunk gets accept/reject controls, restyled by `mergeControlsTheme`.
export function diffExtensions(original: string, dark: boolean): Extension[] {
  return [
    markdown(),
    unifiedMergeView({
      original,
      mergeControls: true,
      collapseUnchanged: { margin: 3, minSize: 4 },
    }),
    mergeControlsTheme(dark),
  ];
}

// Re-export so consumers can switch themes from a single import site.
export { oneDark };
