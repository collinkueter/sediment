import {
  type ChatTurn,
  useAuditStore,
  useChatStore,
  useFormationStore,
  useRemindersStore,
  useWorkingSetStore,
} from "@/lib/store";
import type { ToolActivity } from "@/lib/store";
import { tauri } from "@/lib/tauri";
import { useEffect, useRef, useState } from "react";

/// The conversation surface (ADR-0009). One conversation: the user types, the
/// agent grounds itself, records what it learns into the formation, and replies
/// — all in the same turn. Each turn streams the agent's reply plus an inline
/// trail of the tools it used, and offers a quiet undo once it completes.
export function ChatPane() {
  const sessionId = useChatStore((s) => s.sessionId);
  const turns = useChatStore((s) => s.turns);
  const startTurn = useChatStore((s) => s.startTurn);
  const appendReply = useChatStore((s) => s.appendReply);
  const appendActivity = useChatStore((s) => s.appendActivity);
  const completeTurn = useChatStore((s) => s.completeTurn);
  const failTurn = useChatStore((s) => s.failTurn);
  const resetTurn = useChatStore((s) => s.resetTurn);
  const refreshAudit = useAuditStore((s) => s.refresh);
  const armUndo = useAuditStore((s) => s.armUndo);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);

  // Auto-scroll to the latest reply chunk / activity line. `turns` re-renders
  // the parent on every `appendReply` / `appendActivity` (the array reference
  // changes), so depending on it captures both new turns and streamed growth
  // without scrolling on unrelated re-renders like keystrokes in the textarea.
  // biome-ignore lint/correctness/useExhaustiveDependencies: `turns` is the trigger, not a value read inside the effect — biome can't see that the new identity is what we want to react to.
  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight, behavior: "smooth" });
  }, [turns]);

  // Run one conversational turn into the given local turn id. Streams reply
  // text + tool activity; on success arms the quiet undo and refreshes the
  // audit panel, on failure marks the turn so it can be retried in place.
  async function runTurn(turnLocalId: string, message: string) {
    setBusy(true);
    try {
      const result = await tauri.chatTurn(message, sessionId, (event) => {
        if (event.kind === "textDelta") {
          appendReply(turnLocalId, event.text);
        } else {
          appendActivity(turnLocalId, { tool: event.tool, summary: event.summary });
        }
      });
      completeTurn(turnLocalId, result.reply, result.turnId);
      armUndo({
        kind: "chatTurn",
        turnId: result.turnId,
        changedNoteCount: result.changedNotes.length,
        recordedFactCount: result.recordedFactCount,
      });
      // Refresh the Working Set panel with the authoritative state from this turn.
      useWorkingSetStore.getState().setWorkingSet(result.workingSet);
      // The turn edited notes on disk and may have recorded a task — refresh
      // the file list, the audit log, and the reminders list.
      await useFormationStore.getState().refreshNotes();
      await refreshAudit();
      await useRemindersStore.getState().refresh();
    } catch (e) {
      const error = e instanceof Error ? e.message : String(e);
      failTurn(turnLocalId, { error, body: message });
    } finally {
      setBusy(false);
    }
  }

  async function handleSend() {
    const text = draft.trim();
    if (!text || busy) return;
    setDraft("");
    const turnLocalId = startTurn(text);
    await runTurn(turnLocalId, text);
  }

  // Re-run a failed turn in place, using the message captured when it failed.
  async function handleRetry(turnLocalId: string) {
    if (busy) return;
    const turn = turns.find((t) => t.id === turnLocalId);
    if (!turn?.failure) return;
    const message = turn.failure.body;
    resetTurn(turnLocalId);
    await runTurn(turnLocalId, message);
  }

  function handleKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key !== "Enter") return;
    // IME composition (CJK input) — never treat Enter as send while a
    // candidate is being chosen.
    if (e.nativeEvent.isComposing) return;
    // Shift+Enter — let the textarea insert a newline (browser default).
    if (e.shiftKey) return;
    // Cmd/Ctrl+Enter — explicit newline. Insert at the cursor since the
    // browser default for Cmd+Enter in a textarea is not consistent.
    if (e.metaKey || e.ctrlKey) {
      e.preventDefault();
      const target = e.currentTarget;
      const { selectionStart, selectionEnd, value } = target;
      const next = `${value.slice(0, selectionStart)}\n${value.slice(selectionEnd)}`;
      setDraft(next);
      const caret = selectionStart + 1;
      requestAnimationFrame(() => {
        target.selectionStart = caret;
        target.selectionEnd = caret;
      });
      return;
    }
    // Plain Enter — send.
    e.preventDefault();
    void handleSend();
  }

  return (
    <div className="flex h-full w-full flex-col">
      <header className="flex items-center justify-between border-b border-zinc-200 px-4 py-2 dark:border-zinc-800">
        <span className="text-sm font-medium text-zinc-500 dark:text-zinc-400">Conversation</span>
        <span className="text-xs text-zinc-400 dark:text-zinc-500">
          {busy ? "thinking…" : `${turns.length} turn${turns.length === 1 ? "" : "s"}`}
        </span>
      </header>

      <div ref={scrollRef} className="min-h-0 flex-1 space-y-4 overflow-auto px-4 py-3">
        {turns.length === 0 ? (
          <EmptyState />
        ) : (
          turns.map((turn) => (
            <TurnView
              key={turn.id}
              turn={turn}
              busy={busy}
              onRetry={() => void handleRetry(turn.id)}
            />
          ))
        )}
      </div>

      <div className="border-t border-zinc-200 px-3 py-3 dark:border-zinc-800">
        <textarea
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder="Tell Sediment a thought, or ask about your formation. Enter to send, ⌘Enter for newline."
          rows={3}
          disabled={busy}
          className="block w-full resize-none rounded-md border border-zinc-200 bg-white px-3 py-2 text-sm placeholder:text-zinc-400 focus:border-zinc-400 focus:outline-none disabled:opacity-60 dark:border-zinc-800 dark:bg-zinc-900 dark:placeholder:text-zinc-500"
        />
        <div className="mt-2 flex items-center justify-end">
          <button
            type="button"
            onClick={() => void handleSend()}
            disabled={!draft.trim() || busy}
            className="whitespace-nowrap rounded-md bg-zinc-900 px-3 py-1.5 text-xs font-medium text-white disabled:opacity-40 dark:bg-zinc-100 dark:text-zinc-900"
          >
            Send (⌘↵)
          </button>
        </div>
      </div>
    </div>
  );
}

function EmptyState() {
  return (
    <div className="flex h-full items-center justify-center text-center text-sm text-zinc-400 dark:text-zinc-500">
      <p className="max-w-xs leading-relaxed">
        Sediment is a thinking partner. Tell it what's on your mind — it records what it learns into
        your notes, asks when it needs more, and answers from what it already knows. Each launch
        starts a fresh conversation — your formation is the long-term memory.
      </p>
    </div>
  );
}

/// One conversational turn: the user's bubble, the agent's tool-activity trail,
/// and the streamed reply (or a retry affordance if the turn failed).
function TurnView({
  turn,
  busy,
  onRetry,
}: {
  turn: ChatTurn;
  busy: boolean;
  onRetry: () => void;
}) {
  return (
    <div className="space-y-2">
      {/* User message */}
      <div className="flex justify-end">
        <div className="max-w-[80%] whitespace-pre-wrap rounded-lg bg-zinc-900 px-3 py-2 text-sm text-white dark:bg-zinc-100 dark:text-zinc-900">
          {turn.userMessage}
        </div>
      </div>

      {/* Tool-activity trail — a subtle muted line per tool call */}
      {turn.activity.length > 0 && (
        <ul className="space-y-0.5 pl-1">
          {turn.activity.map((a, i) => (
            <ActivityLine
              // biome-ignore lint/suspicious/noArrayIndexKey: activity is append-only and positional
              key={i}
              activity={a}
            />
          ))}
        </ul>
      )}

      {/* Assistant reply, or a failed-turn retry affordance */}
      {turn.failure ? (
        <div className="flex justify-start">
          <div className="max-w-[80%] rounded-lg border border-red-200 bg-red-50 px-3 py-2 text-sm dark:border-red-900/60 dark:bg-red-950/40">
            <p className="whitespace-pre-wrap text-red-700 dark:text-red-300">
              ⚠️ {turn.failure.error}
            </p>
            <button
              type="button"
              onClick={onRetry}
              disabled={busy}
              className="mt-2 rounded-md border border-red-300 px-2 py-0.5 text-xs font-medium text-red-700 hover:bg-red-100 disabled:opacity-40 dark:border-red-800 dark:text-red-300 dark:hover:bg-red-900/40"
            >
              Retry
            </button>
          </div>
        </div>
      ) : (
        <div className="flex justify-start">
          <div className="max-w-[80%] whitespace-pre-wrap rounded-lg bg-zinc-100 px-3 py-2 text-sm text-zinc-900 dark:bg-zinc-800 dark:text-zinc-100">
            {turn.reply.length === 0 ? (
              turn.pending ? (
                <span className="italic text-zinc-400 dark:text-zinc-500">Thinking…</span>
              ) : (
                <span className="opacity-50">…</span>
              )
            ) : (
              <CitedText text={turn.reply} />
            )}
          </div>
        </div>
      )}
    </div>
  );
}

/// One line of the inline tool-activity trail — a small muted entry naming the
/// tool the agent used and a short summary of the call.
function ActivityLine({ activity }: { activity: ToolActivity }) {
  return (
    <li className="flex items-center gap-1.5 text-[11px] text-zinc-400 dark:text-zinc-500">
      <span aria-hidden className="text-zinc-300 dark:text-zinc-600">
        ↳
      </span>
      <span className="truncate">{activity.summary}</span>
    </li>
  );
}

/// Render assistant text, turning `[[note path]]` citations into clickable
/// links that open the cited note in the left pane.
function CitedText({ text }: { text: string }) {
  const openNote = useFormationStore((s) => s.openNote);
  // Split on [[...]] while keeping the delimiters.
  const parts = text.split(/(\[\[[^\]]+\]\])/g);
  return (
    <>
      {parts.map((part, i) => {
        const match = part.match(/^\[\[([^\]]+)\]\]$/);
        const notePath = match?.[1];
        if (notePath) {
          return (
            <button
              // biome-ignore lint/suspicious/noArrayIndexKey: parts are positional
              key={i}
              type="button"
              onClick={() => {
                openNote(notePath).catch((e) => console.error("open cited note failed:", e));
              }}
              className="rounded bg-zinc-200 px-1 font-medium text-zinc-700 hover:bg-zinc-300 dark:bg-zinc-700 dark:text-zinc-200 dark:hover:bg-zinc-600"
            >
              {notePath}
            </button>
          );
        }
        // biome-ignore lint/suspicious/noArrayIndexKey: parts are positional
        return <span key={i}>{part}</span>;
      })}
    </>
  );
}
