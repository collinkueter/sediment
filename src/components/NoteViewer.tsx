import { diffExtensions, markdownExtensions, oneDark } from "@/lib/codemirror/setup";
import { useFormationStore, useStagingStore } from "@/lib/store";
import { type NoteChange, type StagingEntry, tauri } from "@/lib/tauri";
import CodeMirror from "@uiw/react-codemirror";
import { useEffect, useMemo, useRef, useState } from "react";

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

/// First staged change targeting `notePath`, with its owning entry.
function findStagedChange(
  entries: StagingEntry[],
  notePath: string,
): { entry: StagingEntry; change: NoteChange } | null {
  for (const entry of entries) {
    const change = entry.changes.find((c) => c.note_path === notePath);
    if (change) return { entry, change };
  }
  return null;
}

export function NoteViewer() {
  const currentNotePath = useFormationStore((s) => s.currentNotePath);
  const entries = useStagingStore((s) => s.entries);

  if (!currentNotePath) {
    return <EmptyEditor />;
  }

  const staged = findStagedChange(entries, currentNotePath);
  if (staged) {
    // Remount per note so the working document re-initialises cleanly.
    return <DiffViewer key={currentNotePath} entry={staged.entry} change={staged.change} />;
  }
  return <PlainEditor />;
}

/// The standard markdown editor for a note with no pending staged change.
function PlainEditor() {
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

/// How long to wait after the last accept/reject/edit before persisting the
/// working document back into the staging entry.
const PERSIST_DEBOUNCE_MS = 600;

/// Review view: the note rendered as a unified diff of the on-disk content
/// against the staged proposal, with per-chunk accept/reject controls. Edits
/// (including chunk rejections) are written back into the staged entry so a
/// partially-accepted change commits exactly what the reviewer sees.
function DiffViewer({ entry, change }: { entry: StagingEntry; change: NoteChange }) {
  const prefersDark = usePrefersDark();
  const discardChange = useStagingStore((s) => s.discardChange);
  const keepChange = useStagingStore((s) => s.keepChange);

  const [original, setOriginal] = useState<string | null>(null);
  const [doc, setDoc] = useState(change.new_content);
  const persistTimer = useRef<number | null>(null);

  // The diff base is the note as it currently exists on disk; a staged
  // "create" has no file yet, so the base is empty.
  useEffect(() => {
    let cancelled = false;
    tauri
      .readNote(change.note_path)
      .then((c) => !cancelled && setOriginal(c))
      .catch(() => !cancelled && setOriginal(""));
    return () => {
      cancelled = true;
    };
  }, [change.note_path]);

  useEffect(() => {
    return () => {
      if (persistTimer.current) clearTimeout(persistTimer.current);
    };
  }, []);

  const extensions = useMemo(
    () => (original === null ? markdownExtensions : diffExtensions(original)),
    [original],
  );

  function persistNow(content: string): Promise<void> {
    const updated: StagingEntry = {
      ...entry,
      changes: entry.changes.map((c) =>
        c.note_path === change.note_path ? { ...c, new_content: content } : c,
      ),
    };
    return tauri
      .updateStaging(updated)
      .catch((e) => console.warn("persist staged edit failed:", e));
  }

  function onDocChange(content: string) {
    setDoc(content);
    if (persistTimer.current) clearTimeout(persistTimer.current);
    persistTimer.current = window.setTimeout(() => void persistNow(content), PERSIST_DEBOUNCE_MS);
  }

  async function handleKeep() {
    if (persistTimer.current) clearTimeout(persistTimer.current);
    await persistNow(doc);
    await keepChange(entry.id, change.note_path);
  }

  return (
    <div className="flex h-full w-full flex-col">
      <header className="flex items-center justify-between gap-3 border-b border-zinc-200 px-4 py-2 dark:border-zinc-800">
        <span className="flex min-w-0 items-center gap-2">
          <span className="shrink-0 rounded bg-emerald-100 px-1.5 py-0.5 text-[10px] font-medium text-emerald-700 dark:bg-emerald-900 dark:text-emerald-300">
            {change.kind === "create" ? "NEW · STAGED" : "STAGED DIFF"}
          </span>
          <span className="truncate text-sm font-medium text-zinc-700 dark:text-zinc-300">
            {change.note_path}
          </span>
        </span>
        <span className="flex shrink-0 gap-1">
          <button
            type="button"
            onClick={() => void handleKeep()}
            className="rounded bg-zinc-900 px-2 py-0.5 text-xs font-medium text-white dark:bg-zinc-100 dark:text-zinc-900"
          >
            Keep
          </button>
          <button
            type="button"
            onClick={() => void discardChange(entry.id, change.note_path)}
            className="rounded px-2 py-0.5 text-xs text-zinc-500 hover:bg-zinc-100 dark:text-zinc-400 dark:hover:bg-zinc-800"
          >
            Discard
          </button>
        </span>
      </header>
      <div className="min-h-0 flex-1 overflow-auto">
        {original === null ? (
          <p className="px-4 py-3 text-xs text-zinc-400 dark:text-zinc-500">Loading diff…</p>
        ) : (
          <CodeMirror
            value={doc}
            onChange={onDocChange}
            extensions={extensions}
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

function EmptyEditor() {
  return (
    <div className="flex h-full w-full items-center justify-center text-center">
      <p className="max-w-xs text-sm text-zinc-400 dark:text-zinc-500">
        Select a note from the sidebar, or open this formation in your file browser and add one.
      </p>
    </div>
  );
}
