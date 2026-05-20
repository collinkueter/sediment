import { useFormationStore, useStagingStore, useUiStore } from "@/lib/store";
import type { NoteChange, StagingEntry } from "@/lib/tauri";

/// Bottom tray: extracted facts wait here for review before they touch the
/// formation. Collapsed it is a one-line summary; expanded it lists each
/// pending change with per-note and per-batch Keep / Discard controls.
export function StagingTray() {
  const open = useUiStore((s) => s.stagingTrayOpen);
  const toggle = useUiStore((s) => s.toggleStagingTray);
  const entries = useStagingStore((s) => s.entries);

  const noteCount = entries.reduce((n, e) => n + e.changes.length, 0);
  const summary =
    noteCount === 0 ? "none yet" : `${noteCount} note${noteCount === 1 ? "" : "s"} affected`;

  return (
    <div className="border-t border-zinc-200 bg-zinc-50 dark:border-zinc-800 dark:bg-zinc-900">
      <button
        type="button"
        onClick={toggle}
        className="flex w-full items-center justify-between px-4 py-1.5 text-xs text-zinc-500 hover:bg-zinc-100 dark:text-zinc-400 dark:hover:bg-zinc-800"
      >
        <span>
          Staged changes <span className="ml-1 text-zinc-400 dark:text-zinc-600">— {summary}</span>
        </span>
        <span aria-hidden>{open ? "▾" : "▸"}</span>
      </button>
      {open && (
        <div className="max-h-64 overflow-auto border-t border-zinc-200 dark:border-zinc-800">
          {entries.length === 0 ? (
            <p className="px-4 py-3 text-xs text-zinc-400 dark:text-zinc-500">
              When Sediment extracts facts from a Write-mode message, they appear here for review
              before they're filed into your formation.
            </p>
          ) : (
            entries.map((entry) => <EntryBlock key={entry.id} entry={entry} />)
          )}
        </div>
      )}
    </div>
  );
}

function EntryBlock({ entry }: { entry: StagingEntry }) {
  const discardEntry = useStagingStore((s) => s.discardEntry);
  const keepEntry = useStagingStore((s) => s.keepEntry);

  return (
    <div className="border-b border-zinc-200 px-4 py-2.5 last:border-b-0 dark:border-zinc-800">
      <div className="flex items-start justify-between gap-3">
        <p className="min-w-0 flex-1 truncate text-xs italic text-zinc-500 dark:text-zinc-400">
          “{entry.chat_excerpt}”
        </p>
        <div className="flex shrink-0 gap-1">
          <button
            type="button"
            onClick={() => void keepEntry(entry.id)}
            className="rounded bg-zinc-900 px-2 py-0.5 text-[11px] font-medium text-white dark:bg-zinc-100 dark:text-zinc-900"
          >
            Keep all
          </button>
          <button
            type="button"
            onClick={() => void discardEntry(entry.id)}
            className="rounded px-2 py-0.5 text-[11px] text-zinc-500 hover:bg-zinc-200 dark:text-zinc-400 dark:hover:bg-zinc-800"
          >
            Discard all
          </button>
        </div>
      </div>
      <ul className="mt-2 space-y-1">
        {entry.changes.map((change) => (
          <ChangeRow key={change.note_path} entryId={entry.id} change={change} />
        ))}
      </ul>
    </div>
  );
}

function ChangeRow({ entryId, change }: { entryId: string; change: NoteChange }) {
  const discardChange = useStagingStore((s) => s.discardChange);
  const keepChange = useStagingStore((s) => s.keepChange);
  const openNote = useFormationStore((s) => s.openNote);

  const factCount = change.facts.length;
  const conflictCount = change.conflicts.length;

  return (
    <li className="flex items-center gap-2 text-xs">
      <span aria-hidden title={change.kind === "create" ? "New note" : "Updated note"}>
        {change.kind === "create" ? "➕" : "✎"}
      </span>
      <span className="truncate font-medium text-zinc-700 dark:text-zinc-300">
        {change.note_path}
      </span>
      <span className="shrink-0 text-zinc-400 dark:text-zinc-500">
        +{factCount} fact{factCount === 1 ? "" : "s"}
      </span>
      {conflictCount > 0 && (
        <span
          className="shrink-0 text-amber-600 dark:text-amber-500"
          title={`${conflictCount} conflict${conflictCount === 1 ? "" : "s"}`}
        >
          ⚠ {conflictCount}
        </span>
      )}
      <div className="ml-auto flex shrink-0 gap-1">
        <button
          type="button"
          onClick={() => void openNote(change.note_path)}
          className="rounded px-1.5 py-0.5 text-zinc-500 hover:bg-zinc-200 dark:text-zinc-400 dark:hover:bg-zinc-800"
        >
          view
        </button>
        <button
          type="button"
          onClick={() => void keepChange(entryId, change.note_path)}
          className="rounded px-1.5 py-0.5 text-zinc-500 hover:bg-zinc-200 dark:text-zinc-400 dark:hover:bg-zinc-800"
        >
          keep
        </button>
        <button
          type="button"
          aria-label="Discard this note change"
          onClick={() => void discardChange(entryId, change.note_path)}
          className="rounded px-1.5 py-0.5 text-zinc-400 hover:bg-zinc-200 hover:text-zinc-700 dark:text-zinc-500 dark:hover:bg-zinc-800 dark:hover:text-zinc-200"
        >
          ✗
        </button>
      </div>
    </li>
  );
}
