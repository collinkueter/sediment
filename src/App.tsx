import { ChatPane } from "@/components/ChatPane";
import { FileTree } from "@/components/FileTree";
import { FormationPicker } from "@/components/FormationPicker";
import { IndexProgress } from "@/components/IndexProgress";
import { ModelSetup } from "@/components/ModelSetup";
import { NoteViewer } from "@/components/NoteViewer";
import { Onboarding } from "@/components/Onboarding";
import { SettingsModal } from "@/components/SettingsModal";
import { StagingTray } from "@/components/StagingTray";
import { UndoToast } from "@/components/UndoToast";
import { useFormationStore, useStagingStore, useUiStore } from "@/lib/store";
import { type ModelReadiness, tauri } from "@/lib/tauri";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";

interface FormationChangeEvent {
  kind: string;
  paths: string[];
}

export default function App() {
  const [version, setVersion] = useState<string>("");
  const [onboardingComplete, setOnboardingComplete] = useState<boolean | null>(null);
  const restore = useFormationStore((s) => s.restore);
  const handleExternalChange = useFormationStore((s) => s.handleExternalChange);
  const formationPath = useFormationStore((s) => s.formationPath);
  const refreshStaging = useStagingStore((s) => s.refresh);
  const [modelReadiness, setModelReadiness] = useState<ModelReadiness | null>(null);
  const [modelsChecked, setModelsChecked] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);

  useEffect(() => {
    tauri
      .appVersion()
      .then(setVersion)
      .catch(() => setVersion("?"));
    restore().catch((e) => console.error("restore formation failed:", e));
    // Kick the Ollama daemon awake in the background so the first chat
    // message doesn't pay the cold-start latency. Errors are non-fatal —
    // the chat pane surfaces them with bootstrap guidance.
    tauri.ollamaEnsureRunning().catch((e) => console.warn("ollama ensure failed:", e));
    tauri
      .getOnboardingState()
      .then((s) => setOnboardingComplete(s.complete))
      .catch(() => setOnboardingComplete(false));
  }, [restore]);

  useEffect(() => {
    // Subscribe to the Rust core's debounced file watcher.
    const unlistenP = listen<FormationChangeEvent>("formation-change", (event) => {
      handleExternalChange(event.payload.paths).catch((e) =>
        console.error("external change handler failed:", e),
      );
    });
    return () => {
      unlistenP.then((unlisten) => unlisten()).catch(() => {});
    };
  }, [handleExternalChange]);

  useEffect(() => {
    // Load any pending staging entries for the open formation, and refresh +
    // surface the tray whenever a new Write-mode batch is staged.
    if (formationPath) {
      refreshStaging().catch(() => {});
    }
    const unlistenP = listen("staging-created", () => {
      refreshStaging().catch(() => {});
      useUiStore.getState().setStagingTrayOpen(true);
    });
    return () => {
      unlistenP.then((unlisten) => unlisten()).catch(() => {});
    };
  }, [formationPath, refreshStaging]);

  useEffect(() => {
    // On launch (and on formation switch) check the active tier has its
    // models. The GLiNER model is per-formation, so this needs a formation.
    if (onboardingComplete !== true || !formationPath) return;
    setModelsChecked(false);
    tauri
      .checkModelReadiness()
      .then(setModelReadiness)
      .catch((e) => {
        console.warn("model readiness check failed:", e);
        setModelReadiness(null);
      })
      .finally(() => setModelsChecked(true));
  }, [onboardingComplete, formationPath]);

  // Show onboarding until the user finishes it. `null` = still loading state from disk.
  if (onboardingComplete === false) {
    return <Onboarding onComplete={() => setOnboardingComplete(true)} />;
  }

  // With a formation open, hold the app behind the model check.
  if (onboardingComplete === true && formationPath && !modelsChecked) {
    return <CheckingModels />;
  }
  if (modelReadiness && !modelReadiness.all_present) {
    return <ModelSetup readiness={modelReadiness} onComplete={() => setModelReadiness(null)} />;
  }

  return (
    <div className="flex h-full w-full flex-col bg-zinc-50 text-zinc-900 dark:bg-zinc-950 dark:text-zinc-100">
      <TitleBar version={version} onOpenSettings={() => setSettingsOpen(true)} />
      <main className="flex min-h-0 flex-1">
        <section className="flex basis-3/5 border-r border-zinc-200 dark:border-zinc-800">
          {formationPath ? (
            <>
              <FileTree />
              <div className="min-h-0 flex-1">
                <NoteViewer />
              </div>
            </>
          ) : (
            <FormationPicker />
          )}
        </section>
        <section className="basis-2/5">
          <ChatPane />
        </section>
      </main>
      <StagingTray />
      <UndoToast />
      {settingsOpen && <SettingsModal onClose={() => setSettingsOpen(false)} />}
    </div>
  );
}

function CheckingModels() {
  return (
    <div className="flex h-full w-full items-center justify-center bg-zinc-50 text-sm text-zinc-400 dark:bg-zinc-950 dark:text-zinc-500">
      Checking models…
    </div>
  );
}

function TitleBar({ version, onOpenSettings }: { version: string; onOpenSettings: () => void }) {
  const formationPath = useFormationStore((s) => s.formationPath);
  return (
    <header
      data-tauri-drag-region
      className="flex h-9 items-center justify-center border-b border-zinc-200 text-xs text-zinc-500 dark:border-zinc-800 dark:text-zinc-400"
    >
      <span data-tauri-drag-region>Sediment</span>
      <span data-tauri-drag-region className="ml-2 text-zinc-300 dark:text-zinc-600">
        v{version || "…"}
      </span>
      {formationPath && (
        <span data-tauri-drag-region className="ml-3 truncate text-zinc-400 dark:text-zinc-500">
          · {formationPath}
        </span>
      )}
      <IndexProgress />
      <button
        type="button"
        onClick={onOpenSettings}
        aria-label="Settings"
        className="ml-auto mr-2 rounded px-1.5 text-zinc-400 hover:bg-zinc-100 hover:text-zinc-700 dark:hover:bg-zinc-800 dark:hover:text-zinc-200"
      >
        ⚙
      </button>
    </header>
  );
}
