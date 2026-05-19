import { type ChatMessage, useChatStore } from "@/lib/store";
import { tauri } from "@/lib/tauri";
import { useEffect, useRef, useState } from "react";

// Phase-1 default model. M5 onboarding will let the user pick / pull others.
const DEFAULT_MODEL = "llama3.2:3b";

export function ChatPane() {
  const messages = useChatStore((s) => s.messages);
  const appendMessage = useChatStore((s) => s.appendMessage);
  const appendToken = useChatStore((s) => s.appendToken);
  const setMessageContent = useChatStore((s) => s.setMessageContent);
  const [draft, setDraft] = useState("");
  const [streaming, setStreaming] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);

  // Auto-scroll to the latest token as it streams in.
  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight, behavior: "smooth" });
  }, []);

  async function handleSend() {
    const text = draft.trim();
    if (!text || streaming) return;
    appendMessage({ role: "user", content: text });
    setDraft("");

    const assistantId = appendMessage({ role: "assistant", content: "" });
    setStreaming(true);
    try {
      await tauri.ollamaGenerate(DEFAULT_MODEL, text, (token) => {
        appendToken(assistantId, token);
      });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setMessageContent(
        assistantId,
        `⚠️ Ollama error: ${msg}\n\nMake sure \`ollama serve\` is running and the model \`${DEFAULT_MODEL}\` is pulled (\`ollama pull ${DEFAULT_MODEL}\`).`,
      );
    } finally {
      setStreaming(false);
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
          {streaming ? "streaming…" : `${messages.length} messages`}
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
          placeholder="Type a thought or question. Cmd+Enter to send."
          rows={3}
          disabled={streaming}
          className="block w-full resize-none rounded-md border border-zinc-200 bg-white px-3 py-2 text-sm placeholder:text-zinc-400 focus:border-zinc-400 focus:outline-none disabled:opacity-60 dark:border-zinc-800 dark:bg-zinc-900 dark:placeholder:text-zinc-500"
        />
        <div className="mt-2 flex items-center justify-between">
          <span className="text-xs text-zinc-400 dark:text-zinc-500">
            Treating as: <span className="font-medium">Write</span> · model:{" "}
            <span className="font-mono">{DEFAULT_MODEL}</span>
          </span>
          <button
            type="button"
            onClick={() => void handleSend()}
            disabled={!draft.trim() || streaming}
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
      <p>
        Brain-dump thoughts. Ask questions. <br />
        Sediment will sort them into your formation.
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
