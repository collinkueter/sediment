import { useStagingStore } from "@/lib/store";

/// Floating toast shown for ~10 seconds after a commit. Clicking Undo reverts
/// the notes and facts the commit wrote and puts the review back in the tray.
export function UndoToast() {
  const undoable = useStagingStore((s) => s.undoable);
  const undo = useStagingStore((s) => s.undo);
  const dismiss = useStagingStore((s) => s.dismissUndo);

  if (!undoable) return null;

  const n = undoable.committed_notes.length;

  return (
    <div className="-translate-x-1/2 fixed bottom-10 left-1/2 z-50 flex items-center gap-3 rounded-lg bg-zinc-900 px-4 py-2 text-sm text-white shadow-lg dark:bg-zinc-100 dark:text-zinc-900">
      <span>
        Committed {n} note{n === 1 ? "" : "s"} to your formation.
      </span>
      <button
        type="button"
        onClick={() => void undo()}
        className="rounded bg-white/15 px-2 py-0.5 text-xs font-medium hover:bg-white/25 dark:bg-zinc-900/10 dark:hover:bg-zinc-900/20"
      >
        Undo
      </button>
      <button
        type="button"
        aria-label="Dismiss"
        onClick={dismiss}
        className="text-zinc-400 hover:text-white dark:text-zinc-500 dark:hover:text-zinc-900"
      >
        ✕
      </button>
    </div>
  );
}
