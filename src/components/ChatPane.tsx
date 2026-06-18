import { EmptyState as EmptyState_ } from "@/components/EmptyState";
import { Icon } from "@/components/icons";
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
/// trail of the tools it used, then settles into a quiet receipt with an undo.
///
/// The composer never blocks: the user can keep typing and sending while the
/// agent is still thinking. Each sent message is captured as a turn right away
/// and queued, so a thought reaches the page the instant it's written; the
/// engine drains the queue one turn at a time as it becomes free.
export function ChatPane() {
  const sessionId = useChatStore((s) => s.sessionId);
  const turns = useChatStore((s) => s.turns);
  const startTurn = useChatStore((s) => s.startTurn);
  const beginTurn = useChatStore((s) => s.beginTurn);
  const appendReply = useChatStore((s) => s.appendReply);
  const appendActivity = useChatStore((s) => s.appendActivity);
  const completeTurn = useChatStore((s) => s.completeTurn);
  const failTurn = useChatStore((s) => s.failTurn);
  const markSteered = useChatStore((s) => s.markSteered);
  const redirectTurn = useChatStore((s) => s.redirectTurn);
  const removeTurn = useChatStore((s) => s.removeTurn);
  const newConversation = useChatStore((s) => s.newConversation);
  const resetTurn = useChatStore((s) => s.resetTurn);
  const refreshAudit = useAuditStore((s) => s.refresh);
  const undoTurn = useAuditStore((s) => s.undoTurn);
  const [draft, setDraft] = useState("");
  // True between an interrupt click and the running turn actually stopping, so
  // the interrupt buttons can't race two cancels against one turn.
  const [cancelling, setCancelling] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);
  // Messages waiting to run, paired with the turn already shown for each. The
  // engine processes them in order; a single `pump` loop guarded by
  // `pumpingRef` keeps turns strictly serial even as new ones are enqueued.
  const queueRef = useRef<{ id: string; message: string }[]>([]);
  const pumpingRef = useRef(false);
  // The turn the engine is running right now (set synchronously by `pump`), so a
  // New conversation can interrupt it even though it's already gone from `turns`.
  const runningIdRef = useRef<string | null>(null);
  // Reordering the queue mutates `queueRef` (not the store), so bump this to
  // re-render the place-in-line badges. Store-driven changes (a turn starting or
  // finishing) re-render on their own.
  const [, setQueueVersion] = useState(0);
  const bumpQueue = () => setQueueVersion((v) => v + 1);

  // How many turns are still in flight, for the composer's quiet status line.
  // One turn at most is running; the rest are queued behind it.
  const runningCount = turns.filter((t) => t.pending && !t.queued).length;
  const queuedCount = turns.filter((t) => t.pending && t.queued).length;
  // A queued turn's place in the processing line (1 = next), or null if it isn't
  // queued. Read from `queueRef`; `setQueueVersion` forces a re-read on reorder.
  const placeOf = (id: string) => {
    const i = queueRef.current.findIndex((m) => m.id === id);
    return i < 0 ? null : i + 1;
  };

  // Auto-scroll to the latest reply chunk / activity line. `turns` re-renders
  // the parent on every `appendReply` / `appendActivity` (the array reference
  // changes), so depending on it captures both new turns and streamed growth
  // without scrolling on unrelated re-renders like keystrokes in the textarea.
  // biome-ignore lint/correctness/useExhaustiveDependencies: `turns` is the trigger, not a value read inside the effect — biome can't see that the new identity is what we want to react to.
  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight, behavior: "smooth" });
  }, [turns]);

  // A New conversation mints a fresh `sessionId` (from the header button or the
  // command palette). React to it here so both entry points get the same
  // side-effects: drop the pending queue and interrupt-and-revert any running
  // turn — we're leaving the topic. Skips the initial mount.
  const mountedRef = useRef(false);
  // biome-ignore lint/correctness/useExhaustiveDependencies: `sessionId` is the trigger, not a value read in the effect; `bumpQueue`/`setCancelling` are stable.
  useEffect(() => {
    if (!mountedRef.current) {
      mountedRef.current = true;
      return;
    }
    queueRef.current = [];
    if (runningIdRef.current) void tauri.cancelTurn(runningIdRef.current, "redirect");
    setCancelling(false);
    bumpQueue();
  }, [sessionId]);

  // Run one conversational turn into the given local turn id. Streams reply
  // text + tool activity; on success records the receipt fields and refreshes
  // the side panels, on failure marks the turn so it can be retried in place.
  async function runTurn(turnLocalId: string, message: string) {
    try {
      const result = await tauri.chatTurn(message, sessionId, turnLocalId, (event) => {
        if (event.kind === "textDelta") {
          appendReply(turnLocalId, event.text);
        } else {
          appendActivity(turnLocalId, { tool: event.tool, summary: event.summary });
        }
      });
      // Redirected: the turn was interrupted and its work reverted on disk.
      // Collapse it to a tombstone; nothing was recorded, so skip the refreshes.
      if (result.stop === "redirected") {
        redirectTurn(turnLocalId, message);
        return;
      }
      completeTurn(
        turnLocalId,
        result.reply,
        result.turnId,
        result.changedNotes,
        result.recordedFactCount,
      );
      // Steered: interrupted but its partial work was kept and committed — badge
      // it so the possibly-partial reply reads as intentional.
      if (result.stop === "steered") markSteered(turnLocalId);
      // Refresh the Working Set panel with the authoritative state from this turn.
      useWorkingSetStore.getState().setWorkingSet(result.workingSet);
      // The turn may have updated Self.md — refresh the Self summary too (ADR-0015 §5).
      tauri
        .getSelfSummary()
        .then((s) => useWorkingSetStore.getState().setSelfSummary(s))
        .catch(() => {});
      // The turn edited notes on disk and may have recorded a task — refresh
      // the file list, the audit log, and the reminders list.
      await useFormationStore.getState().refreshNotes();
      await refreshAudit();
      await useRemindersStore.getState().refresh();
    } catch (e) {
      const error = e instanceof Error ? e.message : String(e);
      failTurn(turnLocalId, { error, body: message });
    } finally {
      // The turn settled (completed, steered, redirected, or failed) — any
      // pending interrupt is resolved, so re-enable the buttons.
      setCancelling(false);
    }
  }

  // Drain the queue one turn at a time. `pumpingRef` ensures a single active
  // loop: callers just enqueue and call `pump()`; if a loop is already running
  // it picks up the newly-enqueued turn on its next iteration. Reading
  // `queueRef.current` (a stable ref) each iteration keeps the loop current
  // even though this closure is captured from one render.
  async function pump() {
    if (pumpingRef.current) return;
    pumpingRef.current = true;
    try {
      let next = queueRef.current.shift();
      while (next) {
        runningIdRef.current = next.id;
        beginTurn(next.id);
        await runTurn(next.id, next.message);
        runningIdRef.current = null;
        next = queueRef.current.shift();
      }
    } finally {
      runningIdRef.current = null;
      pumpingRef.current = false;
    }
  }

  function handleSend() {
    const text = draft.trim();
    if (!text) return;
    setDraft("");
    // Capture the turn immediately so the thought lands on the page now, then
    // queue it. It shows as "Queued" until the engine reaches it.
    const turnLocalId = startTurn(text, true);
    queueRef.current.push({ id: turnLocalId, message: text });
    void pump();
  }

  // Re-run a failed turn in place, using the message captured when it failed.
  // Like a fresh send, this rejoins the queue rather than blocking.
  function handleRetry(turnLocalId: string) {
    const turn = turns.find((t) => t.id === turnLocalId);
    if (!turn?.failure) return;
    const message = turn.failure.body;
    resetTurn(turnLocalId);
    queueRef.current.push({ id: turnLocalId, message });
    void pump();
  }

  // Re-run a redirected turn from the message captured when it was redirected.
  function handleResume(turnLocalId: string) {
    const turn = turns.find((t) => t.id === turnLocalId);
    if (!turn?.redirected) return;
    const message = turn.redirected.body;
    resetTurn(turnLocalId);
    queueRef.current.push({ id: turnLocalId, message });
    void pump();
  }

  // Move a queued message to the front of the processing line.
  function promote(turnLocalId: string) {
    const q = queueRef.current;
    const i = q.findIndex((m) => m.id === turnLocalId);
    const item = i > 0 ? q.splice(i, 1)[0] : undefined;
    if (item) {
      q.unshift(item);
      bumpQueue();
    }
  }

  // Nudge a queued message up (-1) or down (+1) one place in the line.
  function moveQueued(turnLocalId: string, dir: -1 | 1) {
    const q = queueRef.current;
    const i = q.findIndex((m) => m.id === turnLocalId);
    const j = i + dir;
    const a = q[i];
    const b = q[j];
    if (i >= 0 && j >= 0 && j < q.length && a && b) {
      q[i] = b;
      q[j] = a;
      bumpQueue();
    }
  }

  // Drop a queued message — it never runs and leaves the transcript.
  function removeQueued(turnLocalId: string) {
    queueRef.current = queueRef.current.filter((m) => m.id !== turnLocalId);
    removeTurn(turnLocalId);
    bumpQueue();
  }

  // Interrupt the running turn so a queued message takes over now. `mode` says
  // what happens to the running turn: "steer" keeps its work, "redirect" reverts
  // it. Either way the clicked message is promoted to run next.
  function interrupt(queuedTurnId: string, mode: "steer" | "redirect") {
    const running = turns.find((t) => t.pending && !t.queued);
    if (!running || cancelling) return;
    promote(queuedTurnId);
    setCancelling(true);
    void tauri.cancelTurn(running.id, mode);
  }

  // Start a fresh conversation. Just resets the session + transcript; the
  // `sessionId` effect below does the side-effects (clear queue, interrupt any
  // running turn) so the command-palette entry point gets them too.
  function handleNewConversation() {
    if (turns.length === 0 && queueRef.current.length === 0) return;
    newConversation();
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
    <div className="flex h-full w-full min-w-0 flex-col bg-bg">
      {/* Slim header — a quiet "New conversation" affordance, shown once there's
          something to clear. The formation (long-term memory) is untouched. */}
      {(turns.length > 0 || queueRef.current.length > 0) && (
        <div className="flex flex-none items-center justify-end px-[22px] pt-3">
          <button
            type="button"
            onClick={handleNewConversation}
            className="inline-flex items-center gap-[6px] rounded-[8px] border border-line px-[10px] py-[5px] text-[11.5px] font-semibold text-muted transition-colors hover:border-line-strong hover:text-ink"
          >
            <Icon.Plus className="h-[13px] w-[13px]" />
            New conversation
          </button>
        </div>
      )}

      {/* Transcript — a centered reading column. */}
      <div ref={scrollRef} className="min-h-0 flex-1 overflow-y-auto px-[22px] pt-[30px] pb-[10px]">
        {turns.length === 0 ? (
          <EmptyState />
        ) : (
          <div className="mx-auto flex max-w-[680px] flex-col gap-6">
            <DayMark />
            {turns.map((turn) => (
              <TurnView
                key={turn.id}
                turn={turn}
                place={placeOf(turn.id)}
                canInterrupt={runningCount > 0}
                cancelling={cancelling}
                queueLen={queueRef.current.length}
                onSteer={() => interrupt(turn.id, "steer")}
                onRedirect={() => interrupt(turn.id, "redirect")}
                onMoveUp={() => moveQueued(turn.id, -1)}
                onMoveDown={() => moveQueued(turn.id, 1)}
                onRemoveQueued={() => removeQueued(turn.id)}
                onResume={() => handleResume(turn.id)}
                onRetry={() => handleRetry(turn.id)}
                onUndo={() => void undoTurn(turn.turnId ?? "")}
              />
            ))}
          </div>
        )}
      </div>

      {/* Composer — generous, hero-weight. */}
      <div className="bg-gradient-to-b from-transparent to-bg px-[22px] pt-[14px] pb-5">
        <div className="mx-auto max-w-[680px]">
          <div className="rounded-2xl border border-line-strong bg-raised px-[15px] pt-[13px] pb-[11px] shadow-sm transition-colors focus-within:border-accent">
            <textarea
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={handleKeyDown}
              placeholder="Tell Sediment a thought, or ask what it knows…"
              rows={2}
              className="block min-h-[50px] w-full resize-none border-none bg-transparent text-[14.5px] leading-relaxed text-ink outline-none placeholder:text-faint"
            />
            <div className="mt-2 flex items-center gap-[10px]">
              <span className="flex items-center gap-[7px] text-[11px] text-faint">
                <kbd className="rounded border border-line-strong px-1 font-mono text-[10px] text-muted">
                  ↵
                </kbd>
                send
                <kbd className="rounded border border-line-strong px-1 font-mono text-[10px] text-muted">
                  ⇧↵
                </kbd>
                newline
              </span>
              {/* Quiet reassurance that sending while busy is safe — the engine
                  is working and queued thoughts will be picked up in order. */}
              {runningCount > 0 && (
                <span className="flex items-center gap-[6px] text-[11px] text-muted">
                  <span className="h-[6px] w-[6px] animate-pulse rounded-full bg-sage" />
                  Thinking
                  {queuedCount > 0 && <span className="text-faint">· {queuedCount} queued</span>}
                </span>
              )}
              <button
                type="button"
                onClick={() => handleSend()}
                disabled={!draft.trim()}
                className="ml-auto inline-flex items-center gap-[7px] whitespace-nowrap rounded-[10px] bg-accent px-[15px] py-2 text-[13px] font-semibold text-white shadow-sm transition-colors hover:bg-accent-ink disabled:opacity-40"
              >
                Send
                <Icon.Send className="h-[14px] w-[14px]" />
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

function DayMark() {
  const label = new Date()
    .toLocaleDateString(undefined, { month: "long", day: "numeric" })
    .toUpperCase();
  return (
    <div className="flex items-center gap-3 text-[11px] font-semibold uppercase tracking-[0.06em] text-faint before:h-px before:flex-1 before:bg-line before:content-[''] after:h-px after:flex-1 after:bg-line after:content-['']">
      Today · {label}
    </div>
  );
}

function EmptyState() {
  return (
    <EmptyState_
      icon={Icon.Sparkle}
      title="A thinking partner"
      description="Tell Sediment what's on your mind. It records what it learns into your notes, asks when it needs more, and answers from what it already knows. Each launch starts fresh — your formation is the long-term memory."
    />
  );
}

/// One conversational turn: the user's bubble, the agent's tool-activity trail,
/// the streamed reply (or a retry affordance), and a quiet receipt with undo.
/// A queued turn instead shows its place in line and the interrupt controls; a
/// redirected turn collapses to a tombstone with Resume.
function TurnView({
  turn,
  place,
  canInterrupt,
  cancelling,
  queueLen,
  onSteer,
  onRedirect,
  onMoveUp,
  onMoveDown,
  onRemoveQueued,
  onResume,
  onRetry,
  onUndo,
}: {
  turn: ChatTurn;
  place: number | null;
  canInterrupt: boolean;
  cancelling: boolean;
  queueLen: number;
  onSteer: () => void;
  onRedirect: () => void;
  onMoveUp: () => void;
  onMoveDown: () => void;
  onRemoveQueued: () => void;
  onResume: () => void;
  onRetry: () => void;
  onUndo: () => void;
}) {
  const isQueued = turn.pending && Boolean(turn.queued);
  const hasReceipt =
    !turn.failure &&
    !turn.redirected &&
    (Boolean(turn.changedNotes?.length) || Boolean(turn.recordedFactCount));
  return (
    <div className="flex flex-col gap-3">
      {/* User message — right-aligned dark bubble with a clipped corner. */}
      <div className="flex justify-end">
        <div className="max-w-[80%] whitespace-pre-wrap rounded-[16px_16px_5px_16px] bg-user-bg px-[15px] py-[10px] text-[14px] leading-relaxed text-user-ink">
          {turn.userMessage}
        </div>
      </div>

      {isQueued ? (
        <QueuedControls
          place={place}
          canInterrupt={canInterrupt}
          cancelling={cancelling}
          queueLen={queueLen}
          onSteer={onSteer}
          onRedirect={onRedirect}
          onMoveUp={onMoveUp}
          onMoveDown={onMoveDown}
          onRemove={onRemoveQueued}
        />
      ) : turn.redirected ? (
        <RedirectedTombstone onResume={onResume} />
      ) : (
        <>
          {/* Fact-trail — a subtle muted line per tool call. */}
          {turn.activity.length > 0 && (
            <ul className="flex flex-col gap-[3px] self-start pl-[42px]">
              {turn.activity.map((a, i) => (
                <ActivityLine
                  // biome-ignore lint/suspicious/noArrayIndexKey: activity is append-only and positional
                  key={i}
                  activity={a}
                />
              ))}
            </ul>
          )}

          {/* Assistant reply, or a failed-turn retry affordance. */}
          {turn.failure ? (
            <div className="ml-[44px] max-w-[560px] rounded-xl border border-line-strong bg-danger-tint px-4 py-3">
              <p className="flex items-start gap-2 whitespace-pre-wrap text-[13px] text-ink-soft">
                <Icon.Warning className="mt-px h-4 w-4 flex-none text-danger" />
                {turn.failure.error}
              </p>
              <button
                type="button"
                onClick={onRetry}
                className="mt-2 rounded-md border border-line-strong px-2 py-0.5 text-[11.5px] font-semibold text-accent-ink hover:bg-accent-tint"
              >
                Retry
              </button>
            </div>
          ) : (
            <div className="flex gap-[14px]">
              <div className="mt-0.5 grid h-[30px] w-[30px] flex-none place-items-center rounded-[9px] bg-[linear-gradient(150deg,var(--accent),var(--accent-ink))] text-white shadow-sm">
                <Icon.Sparkle className="h-[17px] w-[17px]" />
              </div>
              <div className="max-w-[560px] font-serif text-[16.5px] leading-[1.62] text-ink">
                {turn.reply.length === 0 ? (
                  turn.pending ? (
                    <span className="italic text-muted">Thinking…</span>
                  ) : (
                    <span className="opacity-50">…</span>
                  )
                ) : (
                  <>
                    <CitedText text={turn.reply} />
                    {turn.steered && (
                      <span className="ml-2 align-middle text-[10.5px] font-sans font-semibold uppercase tracking-[0.05em] text-faint">
                        · steered
                      </span>
                    )}
                  </>
                )}
              </div>
            </div>
          )}

          {/* Receipt — the inline undo affordance, attached to the turn it describes. */}
          {hasReceipt && <Receipt turn={turn} onUndo={onUndo} />}
        </>
      )}
    </div>
  );
}

/// Controls on a queued message: its place in line, the two interrupt buttons
/// (shown only while a turn is running), reorder nudges, and a drop. Right-
/// aligned to sit under the user's bubble.
function QueuedControls({
  place,
  canInterrupt,
  cancelling,
  queueLen,
  onSteer,
  onRedirect,
  onMoveUp,
  onMoveDown,
  onRemove,
}: {
  place: number | null;
  canInterrupt: boolean;
  cancelling: boolean;
  queueLen: number;
  onSteer: () => void;
  onRedirect: () => void;
  onMoveUp: () => void;
  onMoveDown: () => void;
  onRemove: () => void;
}) {
  const iconBtn =
    "grid h-[22px] w-[22px] place-items-center rounded-[6px] border border-line text-muted transition-colors hover:border-line-strong hover:text-ink disabled:opacity-30 disabled:hover:border-line disabled:hover:text-muted";
  return (
    <div className="flex flex-wrap items-center justify-end gap-[7px] text-[11.5px]">
      <span className="text-faint">Queued{place ? ` · ${place === 1 ? "next" : place}` : ""}</span>
      {canInterrupt && (
        <>
          <button
            type="button"
            onClick={onSteer}
            disabled={cancelling}
            title="Interrupt and run this next, keeping the current turn's work"
            className="rounded-[7px] border border-line-strong px-[9px] py-[3px] font-semibold text-accent-ink transition-colors hover:bg-accent-tint disabled:opacity-40"
          >
            Steer
          </button>
          <button
            type="button"
            onClick={onRedirect}
            disabled={cancelling}
            title="Interrupt and run this instead, reverting the current turn"
            className="rounded-[7px] border border-line-strong px-[9px] py-[3px] font-semibold text-muted transition-colors hover:bg-surface hover:text-ink disabled:opacity-40"
          >
            Redirect
          </button>
        </>
      )}
      <button
        type="button"
        onClick={onMoveUp}
        disabled={place === null || place <= 1}
        title="Move up"
        className={iconBtn}
      >
        <Icon.ChevronDown className="h-3 w-3 rotate-180" />
      </button>
      <button
        type="button"
        onClick={onMoveDown}
        disabled={place === null || place >= queueLen}
        title="Move down"
        className={iconBtn}
      >
        <Icon.ChevronDown className="h-3 w-3" />
      </button>
      <button type="button" onClick={onRemove} title="Remove from queue" className={iconBtn}>
        <Icon.X className="h-3 w-3" />
      </button>
    </div>
  );
}

/// A redirected turn: interrupted with its work reverted. A thin neutral
/// tombstone with a Resume that re-runs it as a fresh turn.
function RedirectedTombstone({ onResume }: { onResume: () => void }) {
  return (
    <div className="ml-[44px] flex max-w-[560px] items-center gap-[9px] rounded-xl border border-line bg-surface py-[7px] pr-[9px] pl-[13px] text-[12px] text-muted">
      <Icon.Undo className="h-[14px] w-[14px] flex-none text-faint" />
      <span className="min-w-0 flex-1 truncate">Redirected — this turn was reverted.</span>
      <button
        type="button"
        onClick={onResume}
        className="inline-flex flex-none items-center gap-[6px] rounded-[7px] border border-line-strong bg-raised px-[13px] py-1 text-[11.5px] font-semibold text-accent-ink hover:border-accent hover:bg-accent-tint"
      >
        Resume
      </button>
    </div>
  );
}

/// The quiet receipt under a completed turn: what it recorded, which note it
/// touched, and an undo button that reverses the whole turn.
function Receipt({ turn, onUndo }: { turn: ChatTurn; onUndo: () => void }) {
  const factCount = turn.recordedFactCount ?? 0;
  const changed = turn.changedNotes ?? [];
  const firstPath = changed[0]?.path;
  const firstName = firstPath ? basename(firstPath) : undefined;
  const extra = changed.length - 1;
  // Lead with what the turn *did* — the note it touched — and mention facts only
  // when there are some. A turn that just edits a note shouldn't announce
  // "Recorded 0 facts"; that buries the real change behind a zero.
  const facts = (
    <b className="font-semibold text-ink-soft">
      {factCount} {factCount === 1 ? "fact" : "facts"}
    </b>
  );
  return (
    <div className="ml-[44px] flex max-w-[560px] items-center gap-[9px] rounded-xl border border-line bg-surface py-[7px] pr-[9px] pl-[13px] text-[12px] text-muted">
      <Icon.Check className="h-[14px] w-[14px] flex-none text-sage" />
      <span className="min-w-0 flex-1 truncate">
        {firstName ? (
          <>
            Updated <cite className="font-semibold not-italic text-accent-ink">{firstName}</cite>
            {extra > 0 && ` +${extra} more`}
            {factCount > 0 && (
              <>
                {" · recorded "}
                {facts}
              </>
            )}
          </>
        ) : (
          <>Recorded {facts}</>
        )}
      </span>
      <button
        type="button"
        onClick={onUndo}
        className="inline-flex flex-none items-center gap-[6px] rounded-[7px] border border-line-strong bg-raised px-[13px] py-1 text-[11.5px] font-semibold text-accent-ink hover:border-accent hover:bg-accent-tint"
      >
        <Icon.Undo className="h-3 w-3" />
        Undo
      </button>
    </div>
  );
}

/// One line of the inline fact-trail — a small muted entry summarising a tool
/// the agent used to produce its reply.
function ActivityLine({ activity }: { activity: ToolActivity }) {
  return (
    <li className="flex items-center gap-[7px] text-[11.5px] text-muted">
      <Icon.Sparkle className="h-[13px] w-[13px] flex-none text-sage" />
      <span className="truncate">{activity.summary}</span>
    </li>
  );
}

/// Render assistant text, turning `[[note path]]` citations into clickable
/// links that open the cited note in the reference pane.
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
              className="rounded-md bg-accent-tint px-2 font-sans text-[12.5px] font-semibold text-accent-ink"
            >
              {basename(notePath)}
            </button>
          );
        }
        // biome-ignore lint/suspicious/noArrayIndexKey: parts are positional
        return <span key={i}>{part}</span>;
      })}
    </>
  );
}

/// The display name for a note path: the basename without its `.md` extension.
function basename(path: string): string {
  const last = path.split("/").pop() ?? path;
  return last.replace(/\.md$/i, "");
}
