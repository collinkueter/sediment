import { useFormationStore } from "@/lib/store";
import { type HardwareInfo, type Tier, tauri } from "@/lib/tauri";
import { useEffect, useMemo, useState } from "react";

type Step = "welcome" | "formation" | "tier" | "done";

const TIERS: { id: Tier; label: string; blurb: string }[] = [
  { id: "Lite", label: "Lite", blurb: "16 GB RAM · 3B model · basic extraction" },
  { id: "Standard", label: "Standard", blurb: "32 GB · 8-14B model · solid extraction + Q&A" },
  { id: "Pro", label: "Pro", blurb: "64 GB+ · 32-70B model · frontier-quality" },
  { id: "Byok", label: "BYOK Cloud", blurb: "Use your own Anthropic/OpenAI API key" },
];

export function Onboarding({ onComplete }: { onComplete: () => void }) {
  const [step, setStep] = useState<Step>("welcome");
  const [hardware, setHardware] = useState<HardwareInfo | null>(null);
  const [tier, setTier] = useState<Tier | null>(null);
  const [busy, setBusy] = useState(false);
  const formationPath = useFormationStore((s) => s.formationPath);
  const pick = useFormationStore((s) => s.pick);

  useEffect(() => {
    tauri
      .detectHardware()
      .then((hw) => {
        setHardware(hw);
        setTier(hw.recommended_tier);
      })
      .catch(() => {
        // Detection failed — let user pick manually.
        setTier("Standard");
      });
  }, []);

  // When the user has a formation open, advance past the formation step automatically.
  useEffect(() => {
    if (step === "formation" && formationPath) {
      setStep("tier");
    }
  }, [step, formationPath]);

  async function finish() {
    if (!tier) return;
    setBusy(true);
    try {
      await tauri.completeOnboarding(tier);
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
        {step === "tier" && (
          <TierStep
            hardware={hardware}
            selected={tier}
            onSelect={setTier}
            onNext={() => setStep("done")}
          />
        )}
        {step === "done" && <DoneStep tier={tier} busy={busy} onFinish={() => void finish()} />}
      </div>
    </div>
  );
}

function Stepper({ current }: { current: Step }) {
  const steps: { id: Step; label: string }[] = useMemo(
    () => [
      { id: "welcome", label: "Welcome" },
      { id: "formation", label: "Formation" },
      { id: "tier", label: "Hardware" },
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
        Sediment is a desktop note-taking app where the primary input is conversation. You chat;
        Sediment extracts facts, files them into your notes, and stages every change for review
        before it touches your disk. Nothing leaves your machine.
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
        new folder or an existing vault. We'll create a{" "}
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

function TierStep({
  hardware,
  selected,
  onSelect,
  onNext,
}: {
  hardware: HardwareInfo | null;
  selected: Tier | null;
  onSelect: (t: Tier) => void;
  onNext: () => void;
}) {
  return (
    <div className="space-y-4">
      <div>
        <h2 className="text-lg font-semibold text-zinc-900 dark:text-zinc-100">
          Detected hardware
        </h2>
        <p className="mt-1 text-xs text-zinc-500 dark:text-zinc-400">
          {hardware
            ? `${hardware.chip} · ${hardware.total_ram_gb} GB RAM — recommended tier: ${hardware.recommended_tier}`
            : "Detecting…"}
        </p>
      </div>
      <div className="space-y-2">
        {TIERS.map((t) => {
          const isSelected = selected === t.id;
          const isRecommended = hardware?.recommended_tier === t.id;
          return (
            <button
              key={t.id}
              type="button"
              onClick={() => onSelect(t.id)}
              className={`block w-full rounded-md border px-3 py-2 text-left ${
                isSelected
                  ? "border-zinc-900 bg-zinc-100 dark:border-zinc-100 dark:bg-zinc-800"
                  : "border-zinc-200 hover:bg-zinc-50 dark:border-zinc-800 dark:hover:bg-zinc-800"
              }`}
            >
              <div className="flex items-center justify-between text-sm font-medium text-zinc-900 dark:text-zinc-100">
                <span>{t.label}</span>
                {isRecommended && (
                  <span className="text-[10px] uppercase text-zinc-500 dark:text-zinc-400">
                    recommended
                  </span>
                )}
              </div>
              <div className="text-xs text-zinc-500 dark:text-zinc-400">{t.blurb}</div>
            </button>
          );
        })}
      </div>
      <button
        type="button"
        onClick={onNext}
        disabled={!selected}
        className="w-full rounded-md bg-zinc-900 px-3 py-2 text-sm font-medium text-white disabled:opacity-50 dark:bg-zinc-100 dark:text-zinc-900"
      >
        Continue
      </button>
    </div>
  );
}

function DoneStep({
  tier,
  busy,
  onFinish,
}: {
  tier: Tier | null;
  busy: boolean;
  onFinish: () => void;
}) {
  return (
    <div className="space-y-4">
      <h2 className="text-lg font-semibold text-zinc-900 dark:text-zinc-100">You're set</h2>
      <p className="text-sm leading-relaxed text-zinc-600 dark:text-zinc-400">
        Sediment will run with the <strong>{tier}</strong> tier defaults. Next, it checks that the
        models this tier needs are installed and downloads anything missing — no manual setup
        required.
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
