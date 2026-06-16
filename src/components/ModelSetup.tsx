import { Icon } from "@/components/icons";
import { type ModelProgress, type ModelReadiness, type ModelRequirement, tauri } from "@/lib/tauri";
import { useState } from "react";

/// Launch-time setup screen, shown when the local embedding model is missing.
/// One click downloads it; progress streams in.
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
  const [preparing, setPreparing] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Switch to the in-process model (no Ollama) and pre-load it before entering.
  async function useOnDevice() {
    setPreparing(true);
    setError(null);
    try {
      await tauri.setEmbeddingProvider("bundled");
      await tauri.warmupEmbeddingModel();
      onComplete();
    } catch (e) {
      setError(`On-device model — ${e instanceof Error ? e.message : String(e)}`);
      setPreparing(false);
    }
  }

  const missing = readiness.requirements.filter((r) => !r.present);
  // The embedding model can't be pulled until Ollama itself is installed.
  const blockedOnOllama = !readiness.ollama_installed && missing.length > 0;

  async function downloadAll() {
    setRunning(true);
    setError(null);
    for (const req of missing) {
      if (!readiness.ollama_installed) continue;
      const onProgress = (p: ModelProgress) => setProgress((s) => ({ ...s, [req.id]: p }));
      try {
        await tauri.pullOllamaModel(req.id, onProgress);
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
    <div className="flex h-full w-full items-center justify-center bg-bg">
      <div className="w-full max-w-lg space-y-5 rounded-lg border border-line bg-raised p-8 shadow-sm">
        <div>
          <h1 className="font-serif text-xl font-semibold text-ink">Set up your models</h1>
          <p className="mt-1 text-sm text-ink-soft">
            Sediment needs the local embedding model that powers note search. It runs entirely on
            your machine — Sediment downloads it once.
          </p>
        </div>

        {blockedOnOllama && (
          <p className="rounded-md border border-line px-3 py-2 text-xs text-gold bg-gold-tint">
            Ollama isn't installed — install it from{" "}
            <span className="font-mono">ollama.com/download</span> and relaunch to download the
            embedding model.
          </p>
        )}

        <ul className="space-y-2">
          {readiness.requirements.map((req) => (
            <RequirementRow key={req.id} req={req} progress={progress[req.id]} />
          ))}
        </ul>

        {error && <p className="text-xs text-danger">{error}</p>}

        <div className="flex items-start justify-between gap-3">
          <div className="flex flex-col gap-1.5">
            <button
              type="button"
              onClick={() => void useOnDevice()}
              disabled={running || preparing}
              className="text-left text-xs font-semibold text-accent-ink hover:underline disabled:opacity-40"
            >
              {preparing ? "Preparing on-device model…" : "Use on-device search (no Ollama)"}
            </button>
            <button
              type="button"
              onClick={() => {
                tauri
                  .setEmbeddingProvider("none")
                  .catch(() => {})
                  .finally(onComplete);
              }}
              disabled={running || preparing}
              className="text-left text-xs font-medium text-muted hover:text-ink-soft disabled:opacity-40"
            >
              Or use keyword search
            </button>
            <p className="max-w-xs text-[10px] leading-snug text-faint">
              On-device runs the embedding model inside Sediment (one ~80 MB download, no Ollama).
              Keyword search needs no model at all. Change anytime in Settings.
            </p>
          </div>
          {missing.length === 0 ? (
            <button
              type="button"
              onClick={onComplete}
              className="rounded-md bg-accent px-4 py-2 text-sm font-medium text-white hover:bg-accent-ink"
            >
              Continue
            </button>
          ) : (
            <button
              type="button"
              onClick={() => void downloadAll()}
              disabled={running || preparing || blockedOnOllama}
              className="rounded-md bg-accent px-4 py-2 text-sm font-medium text-white hover:bg-accent-ink disabled:opacity-40"
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
    <li className="rounded-md border border-line px-3 py-2">
      <div className="flex items-center justify-between text-sm">
        <span className="flex items-center gap-2 text-ink">
          {installed ? (
            <Icon.Check
              aria-hidden
              className={`h-3.5 w-3.5 shrink-0 ${installed ? "text-sage" : "text-faint"}`}
            />
          ) : (
            <span
              aria-hidden
              className="flex h-3.5 w-3.5 shrink-0 items-center justify-center rounded-full border border-line-strong"
            />
          )}
          {req.label}
        </span>
        <span className="text-xs text-muted">{installed ? "ready" : req.size_hint}</span>
      </div>
      {progress && !progress.done && (
        <div className="mt-1.5">
          <div className="h-1 overflow-hidden rounded-full bg-bg-sunk">
            <div
              className="h-full bg-accent transition-all"
              style={{ width: percent !== undefined ? `${percent}%` : "33%" }}
            />
          </div>
          <p className="mt-1 truncate text-[10px] text-muted">
            {progress.phase}
            {percent !== undefined ? ` · ${percent}%` : ""}
          </p>
        </div>
      )}
    </li>
  );
}
