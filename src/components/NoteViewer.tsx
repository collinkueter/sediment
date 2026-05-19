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

export function NoteViewer() {
  const currentNotePath = useFormationStore((s) => s.currentNotePath);
  const content = useFormationStore((s) => s.currentNoteContent);
  const isDirty = useFormationStore((s) => s.isDirty);
  const setContent = useFormationStore((s) => s.setContent);
  const save = useFormationStore((s) => s.save);
  const prefersDark = usePrefersDark();

  // Cmd+S / Ctrl+S to save the current note.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if ((e.metaKey || e.ctrlKey) && e.key === "s") {
        e.preventDefault();
        save().catch((err) => console.error("save failed:", err));
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [save]);

  if (!currentNotePath) {
    return <EmptyEditor />;
  }

  return (
    <div className="flex h-full w-full flex-col">
      <header className="flex items-center justify-between border-b border-zinc-200 px-4 py-2 dark:border-zinc-800">
        <span className="truncate text-sm font-medium text-zinc-700 dark:text-zinc-300">
          {currentNotePath}
          {isDirty && <span className="ml-1 text-amber-500">•</span>}
        </span>
        <button
          type="button"
          onClick={() => save().catch((e) => console.error("save failed:", e))}
          disabled={!isDirty}
          className="rounded-md px-2 py-0.5 text-xs text-zinc-500 hover:bg-zinc-100 disabled:opacity-30 dark:text-zinc-400 dark:hover:bg-zinc-800"
        >
          Save (⌘S)
        </button>
      </header>
      <div className="min-h-0 flex-1 overflow-auto">
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
      </div>
    </div>
  );
}

function EmptyEditor() {
  return (
    <div className="flex h-full w-full items-center justify-center text-center">
      <p className="max-w-xs text-sm text-zinc-400 dark:text-zinc-500">
        Select a note from the sidebar, or open this formation in your file browser and add one.
      </p>
    </div>
  );
}
