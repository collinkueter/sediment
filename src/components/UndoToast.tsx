import { Icon } from "@/components/icons";
import { useAuditStore } from "@/lib/store";
import { useState } from "react";

/// Quiet undo toast shown for ~10 seconds after a turn or task-completion
/// lands (ADR-0009 §6, ADR-0010 §8). Clicking Undo dispatches to the
/// appropriate undo path for the action kind. Chat-turn undos now render inline
/// in ChatPane, so in practice this mostly shows task-completions — but both
/// branches stay live.
export function UndoToast() {
  const undoable = useAuditStore((s) => s.undoable);
  const undoFromToast = useAuditStore((s) => s.undoFromToast);
  const dismiss = useAuditStore((s) => s.dismissUndo);
  const [editedMsg, setEditedMsg] = useState(false);

  if (!undoable) return null;

  let summary: React.ReactNode;
  let canUndo: boolean;

  if (undoable.kind === "chatTurn") {
    const { changedNoteCount, recordedFactCount } = undoable;
    const parts: React.ReactNode[] = [];
    if (changedNoteCount > 0) {
      parts.push(
        <span key="notes" className="font-semibold text-ink">
          {changedNoteCount} note{changedNoteCount === 1 ? "" : "s"}
        </span>,
      );
    }
    if (recordedFactCount > 0) {
      parts.push(
        <span key="facts" className="font-semibold text-ink">
          {recordedFactCount} fact{recordedFactCount === 1 ? "" : "s"}
        </span>,
      );
    }
    summary =
      parts.length > 0 ? (
        <>
          Updated{" "}
          {parts.length === 2 ? (
            <>
              {parts[0]} and {parts[1]}
            </>
          ) : (
            parts[0]
          )}
          .
        </>
      ) : (
        "Turn complete — nothing changed."
      );
    canUndo = changedNoteCount > 0 || recordedFactCount > 0;
  } else {
    // taskCompletion
    summary = editedMsg ? (
      "That entry has been edited — please remove it manually."
    ) : (
      <>
        Logged <span className="font-semibold text-ink">{undoable.taskTitle}</span> to today.
      </>
    );
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
    <div className="-translate-x-1/2 fixed bottom-6 left-1/2 z-50 flex items-center gap-3 rounded-xl border border-line-strong bg-raised px-4 py-2.5 text-ink-soft shadow-2xl">
      <span aria-hidden className="h-2 w-2 shrink-0 rounded-full bg-sage" />
      <span className="text-sm">{summary}</span>
      {canUndo && (
        <button
          type="button"
          onClick={() => void handleUndo()}
          className="flex items-center gap-1.5 rounded-lg border border-line-strong bg-raised px-3 py-1 font-semibold text-[12px] text-accent-ink hover:border-accent hover:bg-accent-tint"
        >
          <Icon.Undo className="h-3 w-3" />
          Undo
        </button>
      )}
      {!editedMsg && (
        <button
          type="button"
          aria-label="Dismiss"
          onClick={dismiss}
          className="grid h-6 w-6 place-items-center rounded-md text-muted hover:bg-bg-sunk hover:text-ink"
        >
          <Icon.X className="h-4 w-4" />
        </button>
      )}
    </div>
  );
}
