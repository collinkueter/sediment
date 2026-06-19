import { Icon } from "@/components/icons";
import { useFormationStore } from "@/lib/store";
import { type ClaudeCodeStatus, type CopilotStatus, tauri } from "@/lib/tauri";
import { useEffect, useMemo, useState } from "react";

type Step = "welcome" | "formation" | "engine" | "search" | "done";
type SearchMode = "bundled" | "ollama" | "none";

export function Onboarding({ onComplete }: { onComplete: () => void }) {
  const [step, setStep] = useState<Step>("welcome");
  const [busy, setBusy] = useState(false);
  // The note-search backend the user picks in the Search step. Carried up here so
  // the Done step can tailor its copy to the choice. On-device is the friendly
  // default for a fresh install — a one-time download, no daemon to run.
  const [searchMode, setSearchMode] = useState<SearchMode>("bundled");
  const formationPath = useFormationStore((s) => s.formationPath);
  const pick = useFormationStore((s) => s.pick);

  // When the user has a formation open, advance past the formation step automatically.
  useEffect(() => {
    if (step === "formation" && formationPath) {
      setStep("engine");
    }
  }, [step, formationPath]);

  async function finish() {
    setBusy(true);
    try {
      await tauri.completeOnboarding();
      onComplete();
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex h-full w-full items-center justify-center bg-bg">
      <div className="w-full max-w-lg space-y-6 rounded-lg border border-line bg-raised p-8 shadow-sm">
        <Stepper current={step} />
        {step === "welcome" && <Welcome onNext={() => setStep("formation")} />}
        {step === "formation" && <FormationStep onPick={pick} />}
        {step === "engine" && <EngineStep onNext={() => setStep("search")} />}
        {step === "search" && (
          <SearchStep
            mode={searchMode}
            onModeChange={setSearchMode}
            onNext={() => setStep("done")}
          />
        )}
        {step === "done" && (
          <DoneStep mode={searchMode} busy={busy} onFinish={() => void finish()} />
        )}
      </div>
    </div>
  );
}

function Stepper({ current }: { current: Step }) {
  const steps: { id: Step; label: string }[] = useMemo(
    () => [
      { id: "welcome", label: "Welcome" },
      { id: "formation", label: "Formation" },
      { id: "engine", label: "Engine" },
      { id: "search", label: "Search" },
      { id: "done", label: "Done" },
    ],
    [],
  );
  const currentIdx = steps.findIndex((s) => s.id === current);
  return (
    <ol className="flex items-center justify-between text-[10px] uppercase tracking-wider text-faint">
      {steps.map((s, i) => {
        const reached = i <= currentIdx;
        return (
          <li key={s.id} className={`flex items-center gap-1 ${reached ? "text-ink-soft" : ""}`}>
            <span
              className={`flex h-5 w-5 items-center justify-center rounded-full text-[10px] ${
                reached ? "bg-accent text-white" : "border border-line-strong text-faint"
              }`}
            >
              {i + 1}
            </span>
            {s.label}
          </li>
        );
      })}
    </ol>
  );
}

function Welcome({ onNext }: { onNext: () => void }) {
  return (
    <div className="space-y-4">
      <h1 className="font-serif text-2xl font-semibold text-ink">Welcome to Sediment</h1>
      <p className="text-sm leading-relaxed text-ink-soft">
        Sediment is a desktop note-taking app where the primary input is conversation. You chat with
        an AI agent; it grounds itself in your notes, records what it learns, and questions you when
        something is unclear or contradicts what it already knows. The conversation runs on your
        installed Claude Code or GitHub Copilot CLI — pick the engine in Settings.
      </p>
      <button
        type="button"
        onClick={onNext}
        className="w-full rounded-md bg-accent px-3 py-2 text-sm font-medium text-white hover:bg-accent-ink"
      >
        Get started
      </button>
    </div>
  );
}

function FormationStep({ onPick }: { onPick: () => Promise<void> }) {
  return (
    <div className="space-y-4">
      <h2 className="font-serif text-lg font-semibold text-ink">Pick a formation</h2>
      <p className="text-sm leading-relaxed text-ink-soft">
        A formation is a folder of markdown notes. Sediment is Obsidian-compatible — point it at a
        new folder or one that already holds your notes. We'll create a{" "}
        <code className="font-mono">.chat-notes/</code> subdirectory for app state.
      </p>
      <button
        type="button"
        onClick={() =>
          onPick().catch((e: unknown) => {
            // pick errors are surfaced by the native dialog — no UI needed
            void e;
          })
        }
        className="w-full rounded-md bg-accent px-3 py-2 text-sm font-medium text-white hover:bg-accent-ink"
      >
        Choose folder…
      </button>
    </div>
  );
}

function EngineStep({ onNext }: { onNext: () => void }) {
  const [claudeCode, setClaudeCode] = useState<ClaudeCodeStatus | null>(null);
  const [copilot, setCopilot] = useState<CopilotStatus | null>(null);
  const [checked, setChecked] = useState(false);

  useEffect(() => {
    // Detect both CLIs in parallel — each call is ~1s.
    Promise.allSettled([tauri.detectClaudeCode(), tauri.detectCopilot()])
      .then(([cc, cp]) => {
        setClaudeCode(
          cc.status === "fulfilled"
            ? cc.value
            : {
                installed: false,
                binary_path: null,
                logged_in: false,
                auth_method: null,
                subscription_type: null,
                email: null,
              },
        );
        setCopilot(cp.status === "fulfilled" ? cp.value : { installed: false, binary_path: null });
      })
      .finally(() => setChecked(true));
  }, []);

  const claudeReady = !!claudeCode?.installed && !!claudeCode?.logged_in;
  const copilotReady = !!copilot?.installed;
  const anyReady = claudeReady || copilotReady;

  return (
    <div className="space-y-4">
      <h2 className="font-serif text-lg font-semibold text-ink">Set up a conversation engine</h2>
      <p className="text-sm leading-relaxed text-ink-soft">
        Sediment runs the agent on an agentic CLI you've installed yourself. You don't need both —
        either one works. You can change this later in Settings.
      </p>

      {!checked ? (
        <p className="text-xs text-faint">Checking your machine…</p>
      ) : (
        <ul className="space-y-2">
          <li className="rounded-md border border-line px-3 py-2">
            <div className="flex items-center justify-between gap-2">
              <span className="text-sm font-medium text-ink">Claude Code</span>
              <EngineBadge ready={claudeReady} />
            </div>
            <p className={`mt-1 text-[11px] ${claudeReady ? "text-sage" : "text-muted"}`}>
              {!claudeCode?.installed
                ? "Install Claude Code from claude.com/claude-code."
                : !claudeCode.logged_in
                  ? "Installed — run `claude` in a terminal and sign in."
                  : `Connected as ${claudeCode.email} · ${claudeCode.subscription_type} subscription.`}
            </p>
          </li>
          <li className="rounded-md border border-line px-3 py-2">
            <div className="flex items-center justify-between gap-2">
              <span className="text-sm font-medium text-ink">GitHub Copilot</span>
              <EngineBadge ready={copilotReady} />
            </div>
            <p className={`mt-1 text-[11px] ${copilotReady ? "text-sage" : "text-muted"}`}>
              {!copilot?.installed
                ? "Install with `npm install -g @github/copilot`."
                : "Installed — run `copilot login` if turns fail."}
            </p>
          </li>
        </ul>
      )}

      {checked && anyReady && (
        <p className="flex items-center gap-1.5 rounded-md border border-line px-3 py-2 text-[11px] text-sage bg-sage-tint">
          <Icon.Check className="h-3.5 w-3.5 shrink-0" aria-hidden />
          You're ready to chat.
        </p>
      )}
      {checked && !anyReady && (
        <p className="rounded-md border border-line px-3 py-2 text-[11px] text-gold bg-gold-tint">
          No engine is ready yet. You can finish onboarding and set one up later in Settings — turns
          will fail until you do.
        </p>
      )}

      <button
        type="button"
        onClick={onNext}
        disabled={!checked}
        className="w-full rounded-md bg-accent px-3 py-2 text-sm font-medium text-white hover:bg-accent-ink disabled:opacity-50"
      >
        {anyReady ? "Continue" : "Set up later in Settings"}
      </button>
    </div>
  );
}

function EngineBadge({ ready }: { ready: boolean }) {
  return ready ? (
    <span
      aria-label="Ready"
      className="flex items-center gap-1 rounded-full bg-sage-tint px-1.5 py-0.5 text-[10px] font-medium text-sage"
    >
      <Icon.Check className="h-2.5 w-2.5" aria-hidden />
      Ready
    </span>
  ) : (
    <span
      aria-label="Not ready"
      className="rounded-full border border-line px-1.5 py-0.5 text-[10px] font-medium text-muted"
    >
      Not ready
    </span>
  );
}

const SEARCH_OPTIONS: {
  mode: SearchMode;
  title: string;
  badge?: string;
  body: string;
}[] = [
  {
    mode: "bundled",
    title: "On-device model",
    badge: "Recommended",
    body: "Sediment downloads a small embedding model (~80 MB, once) and runs it inside the app. Best search quality, and fully offline after setup — no separate server.",
  },
  {
    mode: "ollama",
    title: "Ollama server",
    body: "Use an embedding model served by Ollama — a local daemon, a Docker/Podman container, or a remote host. Point Sediment at the endpoint and it pulls the model there.",
  },
  {
    mode: "none",
    title: "Keyword search",
    body: "A rudimentary keyword match that needs no model at all. Nothing to download — pick this to skip model setup entirely. You can switch to a model later in Settings.",
  },
];

function SearchStep({
  mode,
  onModeChange,
  onNext,
}: {
  mode: SearchMode;
  onModeChange: (m: SearchMode) => void;
  onNext: () => void;
}) {
  const [ollamaUrl, setOllamaUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [importing, setImporting] = useState(false);
  const [imported, setImported] = useState(false);

  // Read any provider already configured so a returning user sees their choice
  // preselected; a fresh install keeps the on-device default.
  // biome-ignore lint/correctness/useExhaustiveDependencies: mount-only preload; onModeChange is a stable setter.
  useEffect(() => {
    Promise.all([tauri.getEmbeddingProvider(), tauri.getOllamaUrl()])
      .then(([provider, url]) => {
        if (provider === "ollama" || provider === "bundled" || provider === "none") {
          onModeChange(provider);
        }
        if (url) setOllamaUrl(url);
      })
      .catch(() => {});
    // Run once on mount; onModeChange is stable enough for this purpose.
  }, []);

  async function persistAndContinue() {
    setBusy(true);
    setError(null);
    try {
      await tauri.setEmbeddingProvider(mode);
      if (mode === "ollama") {
        const trimmed = ollamaUrl.trim();
        await tauri.setOllamaUrl(trimmed === "" ? null : trimmed);
      }
      onNext();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  }

  // BYO model: the user downloaded the files themselves and points at the folder.
  // The app validates and installs them into its own model dir, then uses them —
  // no network needed. Selects the on-device provider so the import is what runs.
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
    setImporting(true);
    try {
      await tauri.setEmbeddingProvider("bundled");
      await tauri.importBundledModel(dir);
      onModeChange("bundled");
      setImported(true);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setImporting(false);
    }
  }

  return (
    <div className="space-y-4">
      <h2 className="font-serif text-lg font-semibold text-ink">Choose how notes are searched</h2>
      <p className="text-sm leading-relaxed text-ink-soft">
        The agent searches your notes to ground every reply. Pick how that runs — you can change it
        anytime in Settings.
      </p>

      <div className="space-y-2">
        {SEARCH_OPTIONS.map((opt) => (
          <SearchOption
            key={opt.mode}
            selected={mode === opt.mode}
            title={opt.title}
            badge={opt.badge}
            body={opt.body}
            onSelect={() => onModeChange(opt.mode)}
          />
        ))}
      </div>

      {mode === "ollama" && (
        <label className="block text-[11px] text-ink-soft">
          Ollama endpoint
          <input
            type="text"
            value={ollamaUrl}
            onChange={(e) => setOllamaUrl(e.target.value)}
            placeholder="http://localhost:11434"
            spellCheck={false}
            autoCapitalize="off"
            autoCorrect="off"
            className="mt-1 block w-full rounded-md border border-line bg-surface px-2.5 py-1.5 font-mono text-[12px] text-ink placeholder:text-faint focus:border-accent focus:outline-none"
          />
          <span className="mt-1 block text-[11px] text-muted">
            Leave blank to auto-manage a local daemon. Set a URL to use Docker/Podman or a remote
            host.
          </span>
        </label>
      )}

      {mode === "bundled" && (
        <div className="rounded-md border border-line px-3 py-2">
          {imported ? (
            <p className="flex items-center gap-1.5 text-[11.5px] text-sage">
              <Icon.Check className="h-3.5 w-3.5 shrink-0" aria-hidden />
              Model imported — Continue to finish.
            </p>
          ) : (
            <>
              <p className="text-[11.5px] leading-relaxed text-muted">
                Already downloaded the model files yourself? Point Sediment at the folder and it
                installs and uses them — no download needed. Otherwise, leave this and Sediment
                downloads the model on the next screen.
              </p>
              <button
                type="button"
                onClick={() => void importFolder()}
                disabled={importing}
                className="mt-1.5 text-[12px] font-semibold text-accent-ink hover:underline disabled:opacity-40"
              >
                {importing ? "Importing…" : "Import model folder…"}
              </button>
            </>
          )}
        </div>
      )}

      {error && <p className="text-xs text-danger">{error}</p>}

      <button
        type="button"
        onClick={() => void persistAndContinue()}
        disabled={busy}
        className="w-full rounded-md bg-accent px-3 py-2 text-sm font-medium text-white hover:bg-accent-ink disabled:opacity-50"
      >
        {busy ? "Saving…" : "Continue"}
      </button>
    </div>
  );
}

function SearchOption({
  selected,
  title,
  badge,
  body,
  onSelect,
}: {
  selected: boolean;
  title: string;
  badge?: string;
  body: string;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onSelect}
      className={[
        "relative w-full rounded-md border p-3 text-left transition-colors",
        selected ? "border-accent bg-accent-tint" : "border-line hover:border-line-strong",
      ].join(" ")}
    >
      {selected && (
        <span className="absolute right-2.5 top-2.5 grid h-4 w-4 place-items-center rounded-full bg-accent text-white">
          <Icon.Check className="h-2.5 w-2.5" strokeWidth={3} />
        </span>
      )}
      <div className="flex items-center gap-2 pr-6">
        <span className="text-sm font-semibold text-ink">{title}</span>
        {badge && (
          <span className="rounded-full bg-sage-tint px-1.5 py-0.5 text-[10px] font-medium text-sage">
            {badge}
          </span>
        )}
      </div>
      <p className="mt-1 text-[11.5px] leading-relaxed text-muted">{body}</p>
    </button>
  );
}

function DoneStep({
  mode,
  busy,
  onFinish,
}: {
  mode: SearchMode;
  busy: boolean;
  onFinish: () => void;
}) {
  const note =
    mode === "none"
      ? "Note search will use keyword matching — no model needed, so you're ready to go."
      : mode === "ollama"
        ? "Next, Sediment makes sure the Ollama embedding model is available and pulls it if missing — it powers note search."
        : "Next, Sediment downloads the on-device embedding model if it isn't installed yet — it powers note search.";
  return (
    <div className="space-y-4">
      <h2 className="font-serif text-lg font-semibold text-ink">You're set</h2>
      <p className="text-sm leading-relaxed text-ink-soft">{note}</p>
      <button
        type="button"
        onClick={onFinish}
        disabled={busy}
        className="w-full rounded-md bg-accent px-3 py-2 text-sm font-medium text-white hover:bg-accent-ink disabled:opacity-50"
      >
        {busy ? "Saving…" : "Open Sediment"}
      </button>
    </div>
  );
}
