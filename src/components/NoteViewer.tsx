import { NotePreview } from "@/components/NotePreview";
import { markdownExtensions, oneDark } from "@/lib/codemirror/setup";
import { useFormationStore } from "@/lib/store";
import CodeMirror from "@uiw/react-codemirror";
import { useEffect, useState } from "react";

function usePrefersDark(): boolean {
  const [prefers, setPrefers] = useState(
    () =>
      typeof window !== "undefined" && window.matchMedia("(prefers-color-scheme: dark)").matches,
  );
  useEffect(() => {
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const handler = (e: MediaQueryListEvent) => setPrefers(e.matches);
    mq.addEventListener("change", handler);
    return () => mq.removeEventListener("change", handler);
  }, []);
  return prefers;
}

type Mode = "preview" | "source";

/**
 * The note pane. The conversational agent edits notes directly on disk
 * (ADR-0009 §5) — there is no staged-diff review in the editor; the audit log
 * is the backstop.
 *
 * Two modes, Obsidian-style:
 * - **Preview** (default) — rendered markdown with clickable `[[wiki-links]]`
 *   that navigate to other notes, headings/lists/code/tables styled, external
 *   links open in the OS browser.
 * - **Source** — the raw CodeMirror editor, for direct hand edits.
 */
export function NoteViewer() {
  const currentNotePath = useFormationStore((s) => s.currentNotePath);

  if (!currentNotePath) {
    return <EmptyEditor />;
  }
  return <Editor />;
}

function Editor() {
  const currentNotePath = useFormationStore((s) => s.currentNotePath);
  const content = useFormationStore((s) => s.currentNoteContent);
  const isDirty = useFormationStore((s) => s.isDirty);
  const setContent = useFormationStore((s) => s.setContent);
  const save = useFormationStore((s) => s.save);
  const prefersDark = usePrefersDark();
  const [mode, setMode] = useState<Mode>("preview");

  // Cmd+S / Ctrl+S to save the current note (Source mode only — Preview has
  // nothing to save). Cmd+E toggles between modes, matching Obsidian.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if ((e.metaKey || e.ctrlKey) && e.key === "s") {
        e.preventDefault();
        save().catch((err) => console.error("save failed:", err));
      } else if ((e.metaKey || e.ctrlKey) && e.key === "e") {
        e.preventDefault();
        setMode((m) => (m === "preview" ? "source" : "preview"));
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [save]);

  // When the active note changes, default back to Preview. Editing is the
  // exception, not the default — most viewing is reading.
  useEffect(() => {
    setMode("preview");
  }, [currentNotePath]);

  return (
    <div className="flex h-full w-full flex-col">
      <header className="flex items-center justify-between border-b border-zinc-200 px-4 py-2 dark:border-zinc-800">
        <span className="truncate text-sm font-medium text-zinc-700 dark:text-zinc-300">
          {currentNotePath}
          {isDirty && <span className="ml-1 text-amber-500">•</span>}
        </span>
        <div className="flex items-center gap-1">
          <ModeToggle mode={mode} setMode={setMode} />
          {mode === "source" && (
            <button
              type="button"
              onClick={() => save().catch((e) => console.error("save failed:", e))}
              disabled={!isDirty}
              className="rounded-md px-2 py-0.5 text-xs text-zinc-500 hover:bg-zinc-100 disabled:opacity-30 dark:text-zinc-400 dark:hover:bg-zinc-800"
            >
              Save (⌘S)
            </button>
          )}
        </div>
      </header>
      <div className="min-h-0 flex-1 overflow-auto">
        {mode === "preview" ? (
          <NotePreview source={content} />
        ) : (
          <CodeMirror
            value={content}
            onChange={setContent}
            extensions={markdownExtensions}
            theme={prefersDark ? oneDark : "light"}
            height="100%"
            basicSetup={{
              lineNumbers: false,
              foldGutter: false,
              highlightActiveLine: false,
              highlightActiveLineGutter: false,
            }}
          />
        )}
      </div>
    </div>
  );
}

function ModeToggle({ mode, setMode }: { mode: Mode; setMode: (m: Mode) => void }) {
  const base = "rounded-md px-2 py-0.5 text-xs transition-colors";
  const active = "bg-zinc-200 text-zinc-900 dark:bg-zinc-700 dark:text-zinc-100";
  const inactive = "text-zinc-500 hover:bg-zinc-100 dark:text-zinc-400 dark:hover:bg-zinc-800";
  return (
    <div className="flex items-center gap-0.5 rounded-md p-0.5" title="⌘E to toggle">
      <button
        type="button"
        className={`${base} ${mode === "preview" ? active : inactive}`}
        onClick={() => setMode("preview")}
      >
        Preview
      </button>
      <button
        type="button"
        className={`${base} ${mode === "source" ? active : inactive}`}
        onClick={() => setMode("source")}
      >
        Source
      </button>
    </div>
  );
}

function EmptyEditor() {
  return (
    <div className="flex h-full w-full items-center justify-center text-center">
      <p className="max-w-xs text-sm text-zinc-400 dark:text-zinc-500">
        Select a note from the sidebar, or start a conversation — the agent creates notes as it
        learns.
      </p>
    </div>
  );
}
