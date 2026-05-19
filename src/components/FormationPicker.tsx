import { useFormationStore } from "@/lib/store";

export function FormationPicker() {
  const pick = useFormationStore((s) => s.pick);
  const loading = useFormationStore((s) => s.loading);

  return (
    <div className="flex h-full w-full flex-col items-center justify-center gap-6 p-8 text-center">
      <div className="max-w-md space-y-3">
        <h2 className="text-xl font-semibold text-zinc-900 dark:text-zinc-100">Open a formation</h2>
        <p className="text-sm leading-relaxed text-zinc-500 dark:text-zinc-400">
          Pick a folder of markdown notes. Sediment will treat it as your formation — an
          Obsidian-compatible folder where your chat-extracted facts settle.
        </p>
      </div>
      <button
        type="button"
        onClick={() => {
          pick().catch((e) => {
            console.error("formation pick failed:", e);
          });
        }}
        disabled={loading}
        className="rounded-md bg-zinc-900 px-4 py-2 text-sm font-medium text-white hover:bg-zinc-800 disabled:opacity-50 dark:bg-zinc-100 dark:text-zinc-900 dark:hover:bg-zinc-200"
      >
        {loading ? "Opening…" : "Choose folder…"}
      </button>
      <p className="max-w-md text-xs text-zinc-400 dark:text-zinc-500">
        A <code className="font-mono">.chat-notes/</code> directory will be created inside the
        folder to hold app state (graph, embeddings, staging). It does not modify your existing
        notes.
      </p>
    </div>
  );
}
