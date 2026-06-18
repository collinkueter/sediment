import { Icon } from "@/components/icons";
import { type ModelProgress, type ModelReadiness, type ModelRequirement, tauri } from "@/lib/tauri";
import { useState } from "react";

// After a semantic provider's model becomes ready, re-embed existing notes so
// search actually returns results. Switching providers (or installing the
// model) invalidates the prior vectors, and the background index on open skips
// unchanged files by mtime — so a forced pass is the only thing that re-embeds.
// Backgrounded; progress surfaces through the usual `index-progress` events.
function reindexNotes() {
  tauri.indexFormation(true).catch((e) => console.warn("re-index after model setup failed:", e));
}

/// Launch-time setup screen, shown when the selected note-search provider's
/// model isn't installed. The acquisition flow depends on the provider:
///   - "ollama": pull the embedding model through the Ollama daemon.
///   - "bundled": download the on-device model, or import it from a folder
///     (the offline path). After install it runs in-process with no network.
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

  async function refresh() {
    const fresh = await tauri.checkModelReadiness();
    setReadiness(fresh);
    return fresh;
  }

  // From the Ollama screen: switch to the in-process provider, then re-check so
  // the on-device acquisition flow (download/import) takes over.
  async function switchToBundled() {
    setPreparing(true);
    setError(null);
    try {
      await tauri.setEmbeddingProvider("bundled");
      const fresh = await refresh();
      if (fresh.all_present) {
        reindexNotes();
        onComplete();
      }
    } catch (e) {
      setError(`On-device model — ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setPreparing(false);
    }
  }

  // From the on-device screen: fall back to Ollama's pull flow.
  async function switchToOllama() {
    setPreparing(true);
    setError(null);
    try {
      await tauri.setEmbeddingProvider("ollama");
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setPreparing(false);
    }
  }

  function chooseKeyword() {
    tauri
      .setEmbeddingProvider("none")
      .catch(() => {})
      .finally(onComplete);
  }

  if (readiness.provider === "bundled") {
    return (
      <SetupCard>
        <BundledSetup
          requirement={readiness.requirements[0]}
          progress={progress}
          setProgress={setProgress}
          running={running}
          setRunning={setRunning}
          preparing={preparing}
          error={error}
          setError={setError}
          refresh={refresh}
          onComplete={onComplete}
          onUseKeyword={chooseKeyword}
          onUseOllama={() => void switchToOllama()}
        />
      </SetupCard>
    );
  }

  return (
    <SetupCard>
      <OllamaSetup
        readiness={readiness}
        setReadiness={setReadiness}
        progress={progress}
        setProgress={setProgress}
        running={running}
        setRunning={setRunning}
        preparing={preparing}
        error={error}
        setError={setError}
        onComplete={onComplete}
        onUseOnDevice={() => void switchToBundled()}
        onUseKeyword={chooseKeyword}
      />
    </SetupCard>
  );
}

function SetupCard({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex h-full w-full items-center justify-center bg-bg">
      <div className="w-full max-w-lg space-y-5 rounded-lg border border-line bg-raised p-8 shadow-sm">
        {children}
      </div>
    </div>
  );
}

// ---- On-device (bundled) provider -----------------------------------------

function BundledSetup({
  requirement,
  progress,
  setProgress,
  running,
  setRunning,
  preparing,
  error,
  setError,
  refresh,
  onComplete,
  onUseKeyword,
  onUseOllama,
}: {
  requirement: ModelRequirement | undefined;
  progress: Record<string, ModelProgress>;
  setProgress: React.Dispatch<React.SetStateAction<Record<string, ModelProgress>>>;
  running: boolean;
  setRunning: (v: boolean) => void;
  preparing: boolean;
  error: string | null;
  setError: (v: string | null) => void;
  refresh: () => Promise<ModelReadiness>;
  onComplete: () => void;
  onUseKeyword: () => void;
  onUseOllama: () => void;
}) {
  const present = requirement?.present ?? false;
  // While downloading, the per-file ticks are keyed by their relative path.
  const active = Object.values(progress).find((p) => !p.done);

  async function download() {
    setRunning(true);
    setError(null);
    setProgress({});
    try {
      await tauri.downloadBundledModel((p) => setProgress((s) => ({ ...s, [p.model]: p })));
      const fresh = await refresh();
      if (fresh.all_present) {
        reindexNotes();
        onComplete();
      }
    } catch (e) {
      setError(`Download failed — ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setRunning(false);
    }
  }

  async function importFolder() {
    setError(null);
    let dir: string | null = null;
    try {
      dir = await tauri.pickDirectory();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      return;
    }
    if (!dir) return;
    setRunning(true);
    try {
      await tauri.importBundledModel(dir);
      const fresh = await refresh();
      if (fresh.all_present) {
        reindexNotes();
        onComplete();
      }
    } catch (e) {
      setError(`Import failed — ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setRunning(false);
    }
  }

  const percent =
    active && active.total > 0
      ? Math.min(100, Math.round((active.completed / active.total) * 100))
      : undefined;

  return (
    <>
      <div>
        <h1 className="font-serif text-xl font-semibold text-ink">Set up on-device search</h1>
        <p className="mt-1 text-sm text-ink-soft">
          On-device search runs the embedding model inside Sediment — no Ollama daemon, and once set
          up it works without a network connection. Download it once, or import a model folder you
          already have.
        </p>
      </div>

      <div className="rounded-md border border-line px-3 py-2">
        <div className="flex items-center justify-between text-sm">
          <span className="flex items-center gap-2 text-ink">
            {present ? (
              <Icon.Check aria-hidden className="h-3.5 w-3.5 shrink-0 text-sage" />
            ) : (
              <span
                aria-hidden
                className="flex h-3.5 w-3.5 shrink-0 items-center justify-center rounded-full border border-line-strong"
              />
            )}
            {requirement?.label ?? "On-device embedding model"}
          </span>
          <span className="text-xs text-muted">
            {present ? "ready" : (requirement?.size_hint ?? "~0.5 GB")}
          </span>
        </div>
        {active && (
          <div className="mt-1.5">
            <div className="h-1 overflow-hidden rounded-full bg-bg-sunk">
              <div
                className="h-full bg-accent transition-all"
                style={{ width: percent !== undefined ? `${percent}%` : "33%" }}
              />
            </div>
            <p className="mt-1 truncate text-[10px] text-muted">
              {active.phase}
              {percent !== undefined ? ` · ${percent}%` : ""}
            </p>
          </div>
        )}
      </div>

      {error && <p className="text-xs text-danger">{error}</p>}

      <div className="flex items-start justify-between gap-3">
        <div className="flex flex-col gap-1.5">
          <button
            type="button"
            onClick={() => void importFolder()}
            disabled={running || preparing}
            className="text-left text-xs font-semibold text-accent-ink hover:underline disabled:opacity-40"
          >
            Import model folder…
          </button>
          <button
            type="button"
            onClick={onUseKeyword}
            disabled={running || preparing}
            className="text-left text-xs font-medium text-muted hover:text-ink-soft disabled:opacity-40"
          >
            Use keyword search instead
          </button>
          <button
            type="button"
            onClick={onUseOllama}
            disabled={running || preparing}
            className="text-left text-[10px] font-medium text-faint hover:text-ink-soft disabled:opacity-40"
          >
            Use Ollama instead
          </button>
          <p className="max-w-xs text-[10px] leading-snug text-faint">
            The folder must contain the model files (onnx/model.onnx plus the tokenizer JSON files).
            Change anytime in Settings.
          </p>
        </div>
        {present ? (
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
            onClick={() => void download()}
            disabled={running || preparing}
            className="rounded-md bg-accent px-4 py-2 text-sm font-medium text-white hover:bg-accent-ink disabled:opacity-40"
          >
            {running ? "Downloading…" : "Download model"}
          </button>
        )}
      </div>
    </>
  );
}

// ---- Ollama provider -------------------------------------------------------

function OllamaSetup({
  readiness,
  setReadiness,
  progress,
  setProgress,
  running,
  setRunning,
  preparing,
  error,
  setError,
  onComplete,
  onUseOnDevice,
  onUseKeyword,
}: {
  readiness: ModelReadiness;
  setReadiness: (r: ModelReadiness) => void;
  progress: Record<string, ModelProgress>;
  setProgress: React.Dispatch<React.SetStateAction<Record<string, ModelProgress>>>;
  running: boolean;
  setRunning: (v: boolean) => void;
  preparing: boolean;
  error: string | null;
  setError: (v: string | null) => void;
  onComplete: () => void;
  onUseOnDevice: () => void;
  onUseKeyword: () => void;
}) {
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
      if (fresh.all_present) {
        reindexNotes();
        onComplete();
      }
    } catch {
      // Leave the screen up so the user can retry.
    }
  }

  return (
    <>
      <div>
        <h1 className="font-serif text-xl font-semibold text-ink">Set up your models</h1>
        <p className="mt-1 text-sm text-ink-soft">
          Sediment needs the local embedding model that powers note search. It runs entirely on your
          machine — Sediment downloads it once.
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
            onClick={onUseOnDevice}
            disabled={running || preparing}
            className="text-left text-xs font-semibold text-accent-ink hover:underline disabled:opacity-40"
          >
            {preparing ? "Switching to on-device…" : "Use on-device search (no Ollama)"}
          </button>
          <button
            type="button"
            onClick={onUseKeyword}
            disabled={running || preparing}
            className="text-left text-xs font-medium text-muted hover:text-ink-soft disabled:opacity-40"
          >
            Or use keyword search
          </button>
          <p className="max-w-xs text-[10px] leading-snug text-faint">
            On-device runs the embedding model inside Sediment (no Ollama). Keyword search needs no
            model at all. Change anytime in Settings.
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
    </>
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
