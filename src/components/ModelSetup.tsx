import { type ModelProgress, type ModelReadiness, type ModelRequirement, tauri } from "@/lib/tauri";
import { useState } from "react";

/// Launch-time setup screen, shown when the active tier is missing models.
/// One click downloads everything needed; progress streams in per model.
export function ModelSetup({
  readiness: initial,
  onComplete,
}: {
  readiness: ModelReadiness;
  onComplete: () => void;
}) {
  const [readiness, setReadiness] = useState(initial);
  const [progress, setProgress] = useState<Record<string, ModelProgress>>({});
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const missing = readiness.requirements.filter((r) => !r.present);
  // Ollama models can't be pulled until Ollama itself is installed.
  const blockedOnOllama = !readiness.ollama_installed && missing.some((r) => r.kind !== "gliner");

  async function downloadAll() {
    setRunning(true);
    setError(null);
    for (const req of missing) {
      if (req.kind !== "gliner" && !readiness.ollama_installed) continue;
      const onProgress = (p: ModelProgress) => setProgress((s) => ({ ...s, [req.id]: p }));
      try {
        if (req.kind === "gliner") {
          await tauri.downloadGlinerModel(onProgress);
        } else {
          await tauri.pullOllamaModel(req.id, onProgress);
        }
      } catch (e) {
        setError(`${req.label} — ${e instanceof Error ? e.message : String(e)}`);
      }
    }
    setRunning(false);
    try {
      const fresh = await tauri.checkModelReadiness();
      setReadiness(fresh);
      if (fresh.all_present) onComplete();
    } catch {
      // Leave the screen up so the user can retry.
    }
  }

  return (
    <div className="flex h-full w-full items-center justify-center bg-zinc-50 dark:bg-zinc-950">
      <div className="w-full max-w-lg space-y-5 rounded-lg border border-zinc-200 bg-white p-8 shadow-sm dark:border-zinc-800 dark:bg-zinc-900">
        <div>
          <h1 className="text-xl font-semibold text-zinc-900 dark:text-zinc-100">
            Set up your models
          </h1>
          <p className="mt-1 text-sm text-zinc-600 dark:text-zinc-400">
            The <strong>{readiness.tier}</strong> tier needs these local models. They run entirely
            on your machine — Sediment downloads them once.
          </p>
        </div>

        {blockedOnOllama && (
          <p className="rounded-md border border-amber-300 bg-amber-50 px-3 py-2 text-xs text-amber-800 dark:border-amber-800/60 dark:bg-amber-950/40 dark:text-amber-300">
            Ollama isn't installed — install it from{" "}
            <span className="font-mono">ollama.com/download</span> and relaunch to download the chat
            and embedding models.
          </p>
        )}

        <ul className="space-y-2">
          {readiness.requirements.map((req) => (
            <RequirementRow key={req.id} req={req} progress={progress[req.id]} />
          ))}
        </ul>

        {error && <p className="text-xs text-red-600 dark:text-red-400">{error}</p>}

        <div className="flex items-center justify-between gap-3">
          <button
            type="button"
            onClick={onComplete}
            disabled={running}
            className="text-xs text-zinc-400 hover:text-zinc-600 disabled:opacity-40 dark:text-zinc-500 dark:hover:text-zinc-300"
          >
            Skip for now
          </button>
          {missing.length === 0 ? (
            <button
              type="button"
              onClick={onComplete}
              className="rounded-md bg-zinc-900 px-4 py-2 text-sm font-medium text-white dark:bg-zinc-100 dark:text-zinc-900"
            >
              Continue
            </button>
          ) : (
            <button
              type="button"
              onClick={() => void downloadAll()}
              disabled={running || (blockedOnOllama && missing.every((r) => r.kind !== "gliner"))}
              className="rounded-md bg-zinc-900 px-4 py-2 text-sm font-medium text-white disabled:opacity-40 dark:bg-zinc-100 dark:text-zinc-900"
            >
              {running
                ? "Downloading…"
                : `Download ${missing.length} model${missing.length === 1 ? "" : "s"}`}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

function RequirementRow({
  req,
  progress,
}: {
  req: ModelRequirement;
  progress: ModelProgress | undefined;
}) {
  const installed = req.present || progress?.done;
  const percent =
    progress && progress.total > 0
      ? Math.min(100, Math.round((progress.completed / progress.total) * 100))
      : undefined;

  return (
    <li className="rounded-md border border-zinc-200 px-3 py-2 dark:border-zinc-800">
      <div className="flex items-center justify-between text-sm">
        <span className="flex items-center gap-2 text-zinc-800 dark:text-zinc-200">
          <span aria-hidden className={installed ? "text-emerald-500" : "text-zinc-400"}>
            {installed ? "✓" : "○"}
          </span>
          {req.label}
        </span>
        <span className="text-xs text-zinc-400 dark:text-zinc-500">
          {installed ? "ready" : req.size_hint}
        </span>
      </div>
      {progress && !progress.done && (
        <div className="mt-1.5">
          <div className="h-1 overflow-hidden rounded-full bg-zinc-200 dark:bg-zinc-800">
            <div
              className="h-full bg-zinc-900 transition-all dark:bg-zinc-100"
              style={{ width: percent !== undefined ? `${percent}%` : "33%" }}
            />
          </div>
          <p className="mt-1 truncate text-[10px] text-zinc-400 dark:text-zinc-500">
            {progress.phase}
            {percent !== undefined ? ` · ${percent}%` : ""}
          </p>
        </div>
      )}
    </li>
  );
}
