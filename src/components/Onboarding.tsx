import { useFormationStore } from "@/lib/store";
import { type ClaudeCodeStatus, type CopilotStatus, tauri } from "@/lib/tauri";
import { useEffect, useMemo, useState } from "react";

type Step = "welcome" | "formation" | "engine" | "done";

export function Onboarding({ onComplete }: { onComplete: () => void }) {
  const [step, setStep] = useState<Step>("welcome");
  const [busy, setBusy] = useState(false);
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
    <div className="flex h-full w-full items-center justify-center bg-zinc-50 dark:bg-zinc-950">
      <div className="w-full max-w-lg space-y-6 rounded-lg border border-zinc-200 bg-white p-8 shadow-sm dark:border-zinc-800 dark:bg-zinc-900">
        <Stepper current={step} />
        {step === "welcome" && <Welcome onNext={() => setStep("formation")} />}
        {step === "formation" && <FormationStep onPick={pick} />}
        {step === "engine" && <EngineStep onNext={() => setStep("done")} />}
        {step === "done" && <DoneStep busy={busy} onFinish={() => void finish()} />}
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
      { id: "done", label: "Done" },
    ],
    [],
  );
  const currentIdx = steps.findIndex((s) => s.id === current);
  return (
    <ol className="flex items-center justify-between text-[10px] uppercase tracking-wider text-zinc-400 dark:text-zinc-500">
      {steps.map((s, i) => {
        const reached = i <= currentIdx;
        return (
          <li
            key={s.id}
            className={`flex items-center gap-1 ${reached ? "text-zinc-700 dark:text-zinc-300" : ""}`}
          >
            <span
              className={`flex h-5 w-5 items-center justify-center rounded-full text-[10px] ${
                reached
                  ? "bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900"
                  : "border border-zinc-300 dark:border-zinc-700"
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
      <h1 className="text-2xl font-semibold text-zinc-900 dark:text-zinc-100">
        Welcome to Sediment
      </h1>
      <p className="text-sm leading-relaxed text-zinc-600 dark:text-zinc-400">
        Sediment is a desktop note-taking app where the primary input is conversation. You chat with
        an AI agent; it grounds itself in your notes, records what it learns, and questions you when
        something is unclear or contradicts what it already knows. The conversation runs on your
        installed Claude Code or GitHub Copilot CLI — pick the engine in Settings.
      </p>
      <button
        type="button"
        onClick={onNext}
        className="w-full rounded-md bg-zinc-900 px-3 py-2 text-sm font-medium text-white dark:bg-zinc-100 dark:text-zinc-900"
      >
        Get started
      </button>
    </div>
  );
}

function FormationStep({ onPick }: { onPick: () => Promise<void> }) {
  return (
    <div className="space-y-4">
      <h2 className="text-lg font-semibold text-zinc-900 dark:text-zinc-100">Pick a formation</h2>
      <p className="text-sm leading-relaxed text-zinc-600 dark:text-zinc-400">
        A formation is a folder of markdown notes. Sediment is Obsidian-compatible — point it at a
        new folder or one that already holds your notes. We'll create a{" "}
        <code className="font-mono">.chat-notes/</code> subdirectory for app state.
      </p>
      <button
        type="button"
        onClick={() => onPick().catch((e) => console.error("pick failed:", e))}
        className="w-full rounded-md bg-zinc-900 px-3 py-2 text-sm font-medium text-white dark:bg-zinc-100 dark:text-zinc-900"
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
      <h2 className="text-lg font-semibold text-zinc-900 dark:text-zinc-100">
        Set up a conversation engine
      </h2>
      <p className="text-sm leading-relaxed text-zinc-600 dark:text-zinc-400">
        Sediment runs the agent on an agentic CLI you've installed yourself. You don't need both —
        either one works. You can change this later in Settings.
      </p>

      {!checked ? (
        <p className="text-xs text-zinc-400 dark:text-zinc-500">Checking your machine…</p>
      ) : (
        <ul className="space-y-2">
          <li className="rounded-md border border-zinc-200 px-3 py-2 dark:border-zinc-800">
            <div className="flex items-center justify-between gap-2">
              <span className="text-sm font-medium text-zinc-900 dark:text-zinc-100">
                Claude Code
              </span>
              <EngineBadge ready={claudeReady} />
            </div>
            <p
              className={`mt-1 text-[11px] ${
                claudeReady
                  ? "text-emerald-600 dark:text-emerald-400"
                  : "text-zinc-500 dark:text-zinc-400"
              }`}
            >
              {!claudeCode?.installed
                ? "Install Claude Code from claude.com/claude-code."
                : !claudeCode.logged_in
                  ? "Installed — run `claude` in a terminal and sign in."
                  : `Connected as ${claudeCode.email} · ${claudeCode.subscription_type} subscription.`}
            </p>
          </li>
          <li className="rounded-md border border-zinc-200 px-3 py-2 dark:border-zinc-800">
            <div className="flex items-center justify-between gap-2">
              <span className="text-sm font-medium text-zinc-900 dark:text-zinc-100">
                GitHub Copilot
              </span>
              <EngineBadge ready={copilotReady} />
            </div>
            <p
              className={`mt-1 text-[11px] ${
                copilotReady
                  ? "text-emerald-600 dark:text-emerald-400"
                  : "text-zinc-500 dark:text-zinc-400"
              }`}
            >
              {!copilot?.installed
                ? "Install with `npm install -g @github/copilot`."
                : "Installed — run `copilot login` if turns fail."}
            </p>
          </li>
        </ul>
      )}

      {checked && anyReady && (
        <p className="rounded-md border border-emerald-300 bg-emerald-50 px-3 py-2 text-[11px] text-emerald-800 dark:border-emerald-800/60 dark:bg-emerald-950/40 dark:text-emerald-300">
          ✓ You're ready to chat.
        </p>
      )}
      {checked && !anyReady && (
        <p className="rounded-md border border-amber-300 bg-amber-50 px-3 py-2 text-[11px] text-amber-800 dark:border-amber-800/60 dark:bg-amber-950/40 dark:text-amber-300">
          No engine is ready yet. You can finish onboarding and set one up later in Settings — turns
          will fail until you do.
        </p>
      )}

      <button
        type="button"
        onClick={onNext}
        disabled={!checked}
        className="w-full rounded-md bg-zinc-900 px-3 py-2 text-sm font-medium text-white disabled:opacity-50 dark:bg-zinc-100 dark:text-zinc-900"
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
      className="rounded-full bg-emerald-100 px-1.5 py-0.5 text-[10px] font-medium text-emerald-700 dark:bg-emerald-900/40 dark:text-emerald-300"
    >
      ✓ Ready
    </span>
  ) : (
    <span
      aria-label="Not ready"
      className="rounded-full bg-zinc-100 px-1.5 py-0.5 text-[10px] font-medium text-zinc-500 dark:bg-zinc-800 dark:text-zinc-400"
    >
      Not ready
    </span>
  );
}

function DoneStep({ busy, onFinish }: { busy: boolean; onFinish: () => void }) {
  return (
    <div className="space-y-4">
      <h2 className="text-lg font-semibold text-zinc-900 dark:text-zinc-100">You're set</h2>
      <p className="text-sm leading-relaxed text-zinc-600 dark:text-zinc-400">
        Next, Sediment checks that the local embedding model is installed and downloads it if
        missing — it powers note search.
      </p>
      <button
        type="button"
        onClick={onFinish}
        disabled={busy}
        className="w-full rounded-md bg-zinc-900 px-3 py-2 text-sm font-medium text-white disabled:opacity-50 dark:bg-zinc-100 dark:text-zinc-900"
      >
        {busy ? "Saving…" : "Open Sediment"}
      </button>
    </div>
  );
}
