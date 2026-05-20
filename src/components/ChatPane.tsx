import { type ChatMessage, useChatStore } from "@/lib/store";
import { type ChatWriteResult, tauri } from "@/lib/tauri";
import { useEffect, useRef, useState } from "react";

export function ChatPane() {
  const sessionId = useChatStore((s) => s.sessionId);
  const messages = useChatStore((s) => s.messages);
  const appendMessage = useChatStore((s) => s.appendMessage);
  const setMessageContent = useChatStore((s) => s.setMessageContent);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);

  // Auto-scroll to the latest message.
  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight, behavior: "smooth" });
  });

  async function handleSend() {
    const text = draft.trim();
    if (!text || busy) return;
    appendMessage({ role: "user", content: text });
    setDraft("");

    const assistantId = appendMessage({ role: "assistant", content: "" });
    setBusy(true);
    try {
      const result = await tauri.chatWrite(text, sessionId);
      setMessageContent(assistantId, formatWriteResult(result));
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setMessageContent(assistantId, `⚠️ ${msg}`);
    } finally {
      setBusy(false);
    }
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
          {busy ? "filing…" : `${messages.length} messages`}
        </span>
      </header>

      <div ref={scrollRef} className="min-h-0 flex-1 space-y-3 overflow-auto px-4 py-3">
        {messages.length === 0 ? (
          <EmptyState />
        ) : (
          messages.map((m) => <Bubble key={m.id} message={m} />)
        )}
      </div>

      <div className="border-t border-zinc-200 px-3 py-3 dark:border-zinc-800">
        <textarea
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder="Type a thought or fact. Cmd+Enter to send."
          rows={3}
          disabled={busy}
          className="block w-full resize-none rounded-md border border-zinc-200 bg-white px-3 py-2 text-sm placeholder:text-zinc-400 focus:border-zinc-400 focus:outline-none disabled:opacity-60 dark:border-zinc-800 dark:bg-zinc-900 dark:placeholder:text-zinc-500"
        />
        <div className="mt-2 flex items-center justify-between">
          <span className="text-xs text-zinc-400 dark:text-zinc-500">
            Treating as: <span className="font-medium">Write</span>
          </span>
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

/// Render a `chat_write` result as a readable assistant message.
function formatWriteResult(result: ChatWriteResult): string {
  const { entities, facts, skipped_low_confidence, skipped_unresolved_entity } = result.extraction;

  if (entities.length === 0 && facts.length === 0) {
    return "No entities or facts found in that message.";
  }

  const lines: string[] = [];
  if (facts.length > 0) {
    lines.push(`Filed ${facts.length} fact${facts.length === 1 ? "" : "s"}:`);
    for (const f of facts) {
      lines.push(`  • ${f.subject} —${f.predicate}→ ${f.object}`);
    }
  }
  if (entities.length > 0) {
    const names = entities.map((e) => `${e.text} (${e.class})`).join(", ");
    lines.push(`Entities: ${names}`);
  }
  const skipped = skipped_low_confidence + skipped_unresolved_entity;
  if (skipped > 0) {
    lines.push(`(${skipped} low-confidence or unresolved item${skipped === 1 ? "" : "s"} skipped)`);
  }
  return lines.join("\n");
}

function EmptyState() {
  return (
    <div className="flex h-full items-center justify-center text-center text-sm text-zinc-400 dark:text-zinc-500">
      <p>
        Brain-dump thoughts and facts. <br />
        Sediment extracts entities and relations into your formation.
      </p>
    </div>
  );
}

function Bubble({ message }: { message: ChatMessage }) {
  const isUser = message.role === "user";
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
        {isEmpty ? <span className="opacity-50">…</span> : message.content}
      </div>
    </div>
  );
}
