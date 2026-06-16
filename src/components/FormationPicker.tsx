import { useFormationStore } from "@/lib/store";

export function FormationPicker() {
  const pick = useFormationStore((s) => s.pick);
  const loading = useFormationStore((s) => s.loading);

  return (
    <div className="flex h-full w-full flex-col items-center justify-center gap-6 p-8 text-center">
      <div className="max-w-md space-y-3">
        <h2 className="font-serif text-xl font-semibold text-ink">Open a formation</h2>
        <p className="text-sm leading-relaxed text-muted">
          Pick a folder of markdown notes. Sediment will treat it as your formation — an
          Obsidian-compatible folder where your chat-extracted facts settle.
        </p>
      </div>
      <button
        type="button"
        onClick={() => {
          pick().catch((e: unknown) => {
            // pick errors are surfaced by the native dialog — no UI needed
            void e;
          });
        }}
        disabled={loading}
        className="rounded-md bg-accent px-4 py-2 text-sm font-medium text-white hover:bg-accent-ink disabled:opacity-50"
      >
        {loading ? "Opening…" : "Choose folder…"}
      </button>
      <p className="max-w-md text-xs text-faint">
        A <code className="font-mono">.chat-notes/</code> directory will be created inside the
        folder to hold app state (graph and embeddings). It does not modify your existing notes.
      </p>
    </div>
  );
}
