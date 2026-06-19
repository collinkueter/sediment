import { Icon } from "@/components/icons";
import { useAuditStore, useFormationStore } from "@/lib/store";
import type { AuditEntry, ChatTurnAuditEntry, TaskCompletionAuditEntry } from "@/lib/tauri";
import { useState } from "react";

/// Bottom panel: the audit log (ADR-0009 §6, ADR-0010 §8). The conversation
/// is the review; this is the browsable backstop — every turn and indexer
/// append, newest-first, with per-event revert controls.
/// Collapsed it is a one-line summary; expanded it lists each entry.
export function AuditLog() {
  const [open, setOpen] = useState(false);
  const entries = useAuditStore((s) => s.entries);

  const summary =
    entries.length === 0
      ? "no entries yet"
      : `${entries.length} entr${entries.length === 1 ? "y" : "ies"}`;

  return (
    <div className="border-t border-line-strong bg-surface">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        className="flex w-full items-center justify-between px-4 py-1.5 text-xs text-ink-soft hover:bg-bg-sunk"
      >
        <span className="flex items-center gap-2">
          <Icon.Clock className="h-3.5 w-3.5 text-muted" aria-hidden />
          <span>
            History &amp; undo <span className="ml-1 text-faint">— {summary}</span>
          </span>
        </span>
        {open ? (
          <Icon.ChevronDown className="h-3.5 w-3.5" aria-hidden />
        ) : (
          <Icon.ChevronRight className="h-3.5 w-3.5" aria-hidden />
        )}
      </button>
      {open && (
        <div className="max-h-72 overflow-auto border-t border-line">
          {entries.length === 0 ? (
            <p className="px-4 py-3 text-xs text-muted">
              Every conversational turn and task check-off that changes your formation is logged
              here. Revert a whole turn, a single recorded fact, or a daily-note append.
            </p>
          ) : (
            entries.map((entry) => {
              const key = entry.kind === "chatTurn" ? entry.turnId : entry.entryId;
              return <EntryBlock key={key} entry={entry} />;
            })
          )}
        </div>
      )}
    </div>
  );
}

function EntryBlock({ entry }: { entry: AuditEntry }) {
  if (entry.kind === "chatTurn") {
    return <ChatTurnBlock entry={entry} />;
  }
  return <TaskCompletionBlock entry={entry} />;
}

/// One chat-turn audit entry: the user + reply excerpts, changed notes, Facts,
/// and whole-turn + per-Fact revert controls (ADR-0009 §6).
function ChatTurnBlock({ entry }: { entry: ChatTurnAuditEntry }) {
  const undoTurn = useAuditStore((s) => s.undoTurn);
  const undoFact = useAuditStore((s) => s.undoFact);
  const openNote = useFormationStore((s) => s.openNote);
  const [reverting, setReverting] = useState(false);
  const [revertError, setRevertError] = useState<string | null>(null);

  const factCount = entry.recordedFactIds.length;
  const noteCount = entry.changedNotes.length;
  const when = new Date(entry.created).toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });

  async function revertTurn() {
    setReverting(true);
    setRevertError(null);
    try {
      await undoTurn(entry.turnId);
    } catch (e) {
      setRevertError(e instanceof Error ? e.message : String(e));
    } finally {
      setReverting(false);
    }
  }

  async function revertFact(factId: string) {
    setReverting(true);
    setRevertError(null);
    try {
      await undoFact(entry.turnId, factId);
    } catch (e) {
      setRevertError(e instanceof Error ? e.message : String(e));
    } finally {
      setReverting(false);
    }
  }

  return (
    <div className="border-b border-line px-4 py-2.5 last:border-b-0">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <p className="truncate text-xs italic text-ink-soft">"{entry.userExcerpt}"</p>
          {entry.replyExcerpt && (
            <p className="mt-0.5 truncate text-[11px] text-muted">{entry.replyExcerpt}</p>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <Icon.Sparkle aria-label="Chat turn" className="h-3 w-3 text-faint" />
          <span className="text-[10px] uppercase tracking-wide text-faint">{when}</span>
          {(noteCount > 0 || factCount > 0) && (
            <button
              type="button"
              onClick={() => void revertTurn()}
              disabled={reverting}
              aria-label={`Revert turn from ${when} — ${entry.userExcerpt}`}
              className="rounded px-2 py-0.5 text-[11px] text-muted hover:bg-bg-sunk hover:text-ink-soft disabled:opacity-40"
            >
              Revert turn
            </button>
          )}
        </div>
      </div>

      {noteCount === 0 && factCount === 0 ? (
        <p className="mt-1.5 text-[11px] text-faint">No changes — the agent only answered.</p>
      ) : (
        <>
          {noteCount > 0 && (
            <ul className="mt-2 space-y-1">
              {entry.changedNotes.map((note) => (
                <li key={note.path} className="flex items-center gap-2 text-xs">
                  {note.wasCreate ? (
                    <Icon.Plus aria-label="New note" className="h-3 w-3 shrink-0 text-accent" />
                  ) : (
                    <Icon.Pencil
                      aria-label="Updated note"
                      className="h-3 w-3 shrink-0 text-muted"
                    />
                  )}
                  <button
                    type="button"
                    onClick={() => void openNote(note.path)}
                    className="truncate font-medium text-accent-ink hover:underline"
                  >
                    {note.path}
                  </button>
                </li>
              ))}
            </ul>
          )}
          {factCount > 0 && (
            <div className="mt-2">
              <p className="text-[11px] text-muted">
                Recorded {factCount} fact{factCount === 1 ? "" : "s"}
              </p>
              <ul className="mt-1 space-y-1">
                {entry.recordedFactIds.map((factId) => (
                  <li key={factId} className="flex items-center gap-2 text-xs">
                    <span aria-hidden className="shrink-0 text-faint">
                      •
                    </span>
                    <code className="min-w-0 flex-1 truncate font-mono text-[11px] text-muted">
                      {factId}
                    </code>
                    <button
                      type="button"
                      onClick={() => void revertFact(factId)}
                      disabled={reverting}
                      aria-label={`Revert fact ${factId}`}
                      className="shrink-0 rounded px-1.5 py-0.5 text-[11px] text-muted hover:bg-bg-sunk hover:text-ink-soft disabled:opacity-40"
                    >
                      Revert
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </>
      )}
      {revertError && (
        <RevertErrorBanner error={revertError} onDismiss={() => setRevertError(null)} />
      )}
    </div>
  );
}

/// One task-completion audit entry: shows which task was logged to which daily
/// note, with a Revert button that removes the bullet (ADR-0010 §8).
function TaskCompletionBlock({ entry }: { entry: TaskCompletionAuditEntry }) {
  const undoTaskCompletion = useAuditStore((s) => s.undoTaskCompletion);
  const [reverting, setReverting] = useState(false);
  const [revertError, setRevertError] = useState<string | null>(null);
  const [editedSince, setEditedSince] = useState(false);

  const when = new Date(entry.created).toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });

  async function revert() {
    setReverting(true);
    setRevertError(null);
    setEditedSince(false);
    try {
      const result = await undoTaskCompletion(entry.entryId);
      if (result === "editedSinceAppended") {
        setEditedSince(true);
      }
    } catch (e) {
      setRevertError(e instanceof Error ? e.message : String(e));
    } finally {
      setReverting(false);
    }
  }

  return (
    <div className="border-b border-line px-4 py-2.5 last:border-b-0">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <p className="truncate text-xs font-medium text-ink">{entry.taskTitle}</p>
          <p className="mt-0.5 truncate text-[11px] text-muted">→ {entry.dailyNotePath}</p>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <Icon.CheckSquare aria-label="Task completion" className="h-3 w-3 text-sage" />
          <span className="text-[10px] uppercase tracking-wide text-faint">{when}</span>
          {!editedSince && (
            <button
              type="button"
              onClick={() => void revert()}
              disabled={reverting}
              aria-label={`Revert task completion — ${entry.taskTitle}`}
              className="rounded px-2 py-0.5 text-[11px] text-muted hover:bg-bg-sunk hover:text-ink-soft disabled:opacity-40"
            >
              Revert
            </button>
          )}
        </div>
      </div>
      {editedSince && (
        <p className="mt-1.5 text-[11px] text-gold">
          This entry has been edited — please remove it manually.
        </p>
      )}
      {revertError && (
        <RevertErrorBanner error={revertError} onDismiss={() => setRevertError(null)} />
      )}
    </div>
  );
}

function RevertErrorBanner({ error, onDismiss }: { error: string; onDismiss: () => void }) {
  return (
    <div className="mt-2 flex items-start justify-between gap-2 rounded-md border border-danger/40 bg-danger-tint px-2 py-1.5">
      <p className="min-w-0 flex-1 text-[11px] text-danger">Revert failed — {error}</p>
      <button
        type="button"
        onClick={onDismiss}
        aria-label="Dismiss revert error"
        className="shrink-0 rounded px-1 text-[11px] text-danger hover:bg-danger/10"
      >
        <Icon.X className="h-3 w-3" aria-hidden />
      </button>
    </div>
  );
}
