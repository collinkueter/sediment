import { useAuditStore } from "@/lib/store";
import { useState } from "react";

/// Quiet undo toast shown for ~10 seconds after a turn or task-completion
/// lands (ADR-0009 §6, ADR-0010 §8). Clicking Undo dispatches to the
/// appropriate undo path for the action kind.
export function UndoToast() {
  const undoable = useAuditStore((s) => s.undoable);
  const undoFromToast = useAuditStore((s) => s.undoFromToast);
  const dismiss = useAuditStore((s) => s.dismissUndo);
  const [editedMsg, setEditedMsg] = useState(false);

  if (!undoable) return null;

  let summary: string;
  let canUndo: boolean;

  if (undoable.kind === "chatTurn") {
    const { changedNoteCount, recordedFactCount } = undoable;
    const parts: string[] = [];
    if (changedNoteCount > 0) {
      parts.push(`${changedNoteCount} note${changedNoteCount === 1 ? "" : "s"}`);
    }
    if (recordedFactCount > 0) {
      parts.push(`${recordedFactCount} fact${recordedFactCount === 1 ? "" : "s"}`);
    }
    summary =
      parts.length > 0 ? `Updated ${parts.join(" and ")}.` : "Turn complete — nothing changed.";
    canUndo = changedNoteCount > 0 || recordedFactCount > 0;
  } else {
    // taskCompletion
    summary = editedMsg
      ? "That entry has been edited — please remove it manually."
      : `Logged '${undoable.taskTitle}' to today.`;
    canUndo = !editedMsg;
  }

  async function handleUndo() {
    const result = await undoFromToast();
    if (result === "editedSinceAppended") {
      setEditedMsg(true);
      // Auto-dismiss after 3 seconds so the message is visible but not sticky.
      setTimeout(() => {
        setEditedMsg(false);
        dismiss();
      }, 3000);
    }
  }

  return (
    <div className="-translate-x-1/2 fixed bottom-10 left-1/2 z-50 flex items-center gap-3 rounded-lg bg-zinc-900 px-4 py-2 text-sm text-white shadow-lg dark:bg-zinc-100 dark:text-zinc-900">
      <span>{summary}</span>
      {canUndo && (
        <button
          type="button"
          onClick={() => void handleUndo()}
          className="rounded bg-white/15 px-2 py-0.5 text-xs font-medium hover:bg-white/25 dark:bg-zinc-900/10 dark:hover:bg-zinc-900/20"
        >
          Undo
        </button>
      )}
      {!editedMsg && (
        <button
          type="button"
          aria-label="Dismiss"
          onClick={dismiss}
          className="text-zinc-400 hover:text-white dark:text-zinc-500 dark:hover:text-zinc-900"
        >
          ✕
        </button>
      )}
    </div>
  );
}
