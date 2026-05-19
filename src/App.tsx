import { ChatPane } from "@/components/ChatPane";
import { FileTree } from "@/components/FileTree";
import { FormationPicker } from "@/components/FormationPicker";
import { NoteViewer } from "@/components/NoteViewer";
import { Onboarding } from "@/components/Onboarding";
import { StagingTray } from "@/components/StagingTray";
import { useFormationStore } from "@/lib/store";
import { tauri } from "@/lib/tauri";
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

  // Show onboarding until the user finishes it. `null` = still loading state from disk.
  if (onboardingComplete === false) {
    return <Onboarding onComplete={() => setOnboardingComplete(true)} />;
  }

  return (
    <div className="flex h-full w-full flex-col bg-zinc-50 text-zinc-900 dark:bg-zinc-950 dark:text-zinc-100">
      <TitleBar version={version} />
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
    </div>
  );
}

function TitleBar({ version }: { version: string }) {
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
    </header>
  );
}
