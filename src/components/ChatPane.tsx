import { type ChatMessage, type ChatMode, useChatStore, useFormationStore } from "@/lib/store";
import { type ChatWriteResult, tauri } from "@/lib/tauri";
import { useEffect, useRef, useState } from "react";

export function ChatPane() {
  const sessionId = useChatStore((s) => s.sessionId);
  const messages = useChatStore((s) => s.messages);
  const appendMessage = useChatStore((s) => s.appendMessage);
  const appendToken = useChatStore((s) => s.appendToken);
  const setMessageContent = useChatStore((s) => s.setMessageContent);
  const failMessage = useChatStore((s) => s.failMessage);
  const clearFailure = useChatStore((s) => s.clearFailure);
  const [draft, setDraft] = useState("");
  const [mode, setMode] = useState<ChatMode>("write");
  // When true, `mode` tracks the intent classifier; a manual toggle clears it.
  const [autoMode, setAutoMode] = useState(true);
  const [lowConfidence, setLowConfidence] = useState(false);
  const [busy, setBusy] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);

  // Auto-scroll to the latest message / token.
  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight, behavior: "smooth" });
  });

  // Auto-classify the draft (debounced) and follow the result while autoMode.
  useEffect(() => {
    if (!autoMode) return;
    const text = draft.trim();
    if (!text || text.startsWith("/")) {
      setLowConfidence(false);
      return;
    }
    const timer = setTimeout(() => {
      tauri
        .classifyIntent(text)
        .then((r) => {
          setMode(r.mode);
          setLowConfidence(r.confidence < 0.8);
        })
        .catch(() => {});
    }, 250);
    return () => clearTimeout(timer);
  }, [draft, autoMode]);

  // Run a chat turn into the given assistant message. On failure, marks that
  // message so the user can retry the turn from its bubble.
  async function runRequest(assistantId: string, requestMode: ChatMode, body: string) {
    setBusy(true);
    try {
      if (requestMode === "write") {
        const result = await tauri.chatWrite(body, sessionId);
        setMessageContent(assistantId, formatWriteResult(result));
      } else {
        await tauri.chatAsk(body, sessionId, (token) => appendToken(assistantId, token));
      }
    } catch (e) {
      const error = e instanceof Error ? e.message : String(e);
      failMessage(assistantId, { error, mode: requestMode, body });
    } finally {
      setBusy(false);
    }
  }

  async function handleSend() {
    const text = draft.trim();
    if (!text || busy) return;

    // `/write` and `/ask` slash prefixes hard-override the mode for this turn.
    let effectiveMode = mode;
    let body = text;
    if (text.startsWith("/write ")) {
      effectiveMode = "write";
      body = text.slice("/write ".length).trim();
    } else if (text.startsWith("/ask ")) {
      effectiveMode = "ask";
      body = text.slice("/ask ".length).trim();
    }
    if (!body) return;

    appendMessage({ role: "user", content: text });
    setDraft("");
    const assistantId = appendMessage({ role: "assistant", content: "" });
    await runRequest(assistantId, effectiveMode, body);
    // Resume auto-classification for the next message.
    setAutoMode(true);
    setLowConfidence(false);
  }

  // Re-run a failed turn into its existing bubble, using the request captured
  // when it failed — e.g. after the user installs or starts the model.
  async function handleRetry(messageId: string) {
    if (busy) return;
    const failure = messages.find((m) => m.id === messageId)?.failure;
    if (!failure) return;
    clearFailure(messageId);
    await runRequest(messageId, failure.mode, failure.body);
  }

  function handleKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      void handleSend();
    }
  }

  return (
    <div className="flex h-full w-full flex-col">
      <header className="flex items-center justify-between border-b border-zinc-200 px-4 py-2 dark:border-zinc-800">
        <span className="text-sm font-medium text-zinc-500 dark:text-zinc-400">Chat</span>
        <span className="text-xs text-zinc-400 dark:text-zinc-500">
          {busy ? (mode === "write" ? "filing…" : "answering…") : `${messages.length} messages`}
        </span>
      </header>

      <div ref={scrollRef} className="min-h-0 flex-1 space-y-3 overflow-auto px-4 py-3">
        {messages.length === 0 ? (
          <EmptyState />
        ) : (
          messages.map((m) => (
            <Bubble key={m.id} message={m} busy={busy} onRetry={() => void handleRetry(m.id)} />
          ))
        )}
      </div>

      <div className="border-t border-zinc-200 px-3 py-3 dark:border-zinc-800">
        <textarea
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder={
            mode === "write"
              ? "Type a thought or fact. Cmd+Enter to send."
              : "Ask a question about your formation. Cmd+Enter to send."
          }
          rows={3}
          disabled={busy}
          className="block w-full resize-none rounded-md border border-zinc-200 bg-white px-3 py-2 text-sm placeholder:text-zinc-400 focus:border-zinc-400 focus:outline-none disabled:opacity-60 dark:border-zinc-800 dark:bg-zinc-900 dark:placeholder:text-zinc-500"
        />
        <div className="mt-2 flex items-center justify-between">
          <div className="flex items-center gap-2">
            <ModeToggle
              mode={mode}
              onChange={(m) => {
                setMode(m);
                setAutoMode(false); // a manual pick stops auto-classification
              }}
              disabled={busy}
            />
            <span className="text-[10px] text-zinc-400 dark:text-zinc-500">
              {autoMode ? (lowConfidence ? "auto · unsure" : "auto") : "manual"}
            </span>
          </div>
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

function ModeToggle({
  mode,
  onChange,
  disabled,
}: {
  mode: ChatMode;
  onChange: (m: ChatMode) => void;
  disabled: boolean;
}) {
  return (
    <div className="flex items-center gap-1 rounded-md border border-zinc-200 p-0.5 text-xs dark:border-zinc-800">
      {(["write", "ask"] as const).map((m) => (
        <button
          key={m}
          type="button"
          disabled={disabled}
          onClick={() => onChange(m)}
          className={`rounded px-2 py-0.5 capitalize ${
            mode === m
              ? "bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900"
              : "text-zinc-500 dark:text-zinc-400"
          }`}
        >
          {m}
        </button>
      ))}
    </div>
  );
}

/// Render a `chat_write` result as a readable assistant message. Facts are
/// staged for review, not committed — the message points at the tray.
function formatWriteResult(result: ChatWriteResult): string {
  const skipped = result.skipped_low_confidence + result.skipped_unresolved;
  const skippedNote =
    skipped > 0
      ? ` (${skipped} possible fact${skipped === 1 ? "" : "s"} were too unclear to file.)`
      : "";
  if (!result.staged) {
    return `No new facts found in that message.${skippedNote}`;
  }
  const { changes } = result.staged;
  const factCount = changes.reduce((n, c) => n + c.facts.length, 0);
  const noteCount = changes.length;
  const facts = `${factCount} fact${factCount === 1 ? "" : "s"}`;
  const notes = `${noteCount} note${noteCount === 1 ? "" : "s"}`;
  return `Staged ${facts} across ${notes} — review them in the tray below before they're filed.${skippedNote}`;
}

function EmptyState() {
  return (
    <div className="flex h-full items-center justify-center text-center text-sm text-zinc-400 dark:text-zinc-500">
      <p>
        <strong>Write</strong> mode files facts into your formation. <br />
        <strong>Ask</strong> mode answers questions from it, with citations.
      </p>
    </div>
  );
}

function Bubble({
  message,
  busy,
  onRetry,
}: {
  message: ChatMessage;
  busy: boolean;
  onRetry: () => void;
}) {
  const isUser = message.role === "user";

  // A failed turn renders an error + Retry instead of a normal bubble.
  if (message.failure) {
    return (
      <div className="flex justify-start">
        <div className="max-w-[80%] rounded-lg border border-red-200 bg-red-50 px-3 py-2 text-sm dark:border-red-900/60 dark:bg-red-950/40">
          <p className="whitespace-pre-wrap text-red-700 dark:text-red-300">
            ⚠️ {message.failure.error}
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
    );
  }

  const isEmpty = message.content.length === 0;
  return (
    <div className={`flex ${isUser ? "justify-end" : "justify-start"}`}>
      <div
        className={`max-w-[80%] whitespace-pre-wrap rounded-lg px-3 py-2 text-sm ${
          isUser
            ? "bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900"
            : "bg-zinc-100 text-zinc-900 dark:bg-zinc-800 dark:text-zinc-100"
        }`}
      >
        {isEmpty ? (
          <span className="opacity-50">…</span>
        ) : isUser ? (
          message.content
        ) : (
          <CitedText text={message.content} />
        )}
      </div>
    </div>
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
