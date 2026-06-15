import { AuditLog } from "@/components/AuditLog";
import { ChatPane } from "@/components/ChatPane";
import { FileTree } from "@/components/FileTree";
import { FormationPicker } from "@/components/FormationPicker";
import { IndexProgress } from "@/components/IndexProgress";
import { ModelSetup } from "@/components/ModelSetup";
import { NoteViewer } from "@/components/NoteViewer";
import { Onboarding } from "@/components/Onboarding";
import { ReminderToast } from "@/components/ReminderToast";
import { RemindersPopover } from "@/components/RemindersPopover";
import { SettingsModal } from "@/components/SettingsModal";
import { UndoToast } from "@/components/UndoToast";
import { WorkingSetPanel } from "@/components/WorkingSetPanel";
import {
  useAuditStore,
  useFormationStore,
  useRemindersStore,
  useWorkingSetStore,
} from "@/lib/store";
import { type ModelReadiness, type Task, tauri } from "@/lib/tauri";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useState } from "react";

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
  const refreshAudit = useAuditStore((s) => s.refresh);
  const setupAudit = useAuditStore((s) => s.setup);
  const setWorkingSet = useWorkingSetStore((s) => s.setWorkingSet);
  const [modelReadiness, setModelReadiness] = useState<ModelReadiness | null>(null);
  const [modelsChecked, setModelsChecked] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [remindersOpen, setRemindersOpen] = useState(false);
  const refreshReminders = useRemindersStore((s) => s.refresh);
  const showDueToast = useRemindersStore((s) => s.showDueToast);
  const openTaskCount = useRemindersStore((s) => s.tasks.filter((t) => t.status === "open").length);

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
    // Load the audit log for the open formation. It is refreshed after each
    // conversational turn and after any undo by the audit store itself.
    if (formationPath) {
      refreshAudit().catch(() => {});
    }
  }, [formationPath, refreshAudit]);

  useEffect(() => {
    // Load reminders for the open formation, and surface a toast + refresh
    // whenever the scheduler fires one. (A turn that records a task refreshes
    // the list directly — see ChatPane.) The listener is only active while a
    // formation is open; without one there's nothing the scheduler could fire.
    if (!formationPath) return;
    refreshReminders().catch(() => {});
    const dueP = listen<Task>("reminder-due", (event) => {
      showDueToast(event.payload);
      refreshReminders().catch(() => {});
    });
    return () => {
      dueP.then((unlisten) => unlisten()).catch(() => {});
    };
  }, [formationPath, refreshReminders, showDueToast]);

  useEffect(() => {
    // Subscribe to daily-note-appended events from the indexer. The audit
    // store's setup action wires the listener and arms the quiet undo toast.
    // Only active while a formation is open — same pattern as reminder-due.
    if (!formationPath) return;
    const unlistenP = setupAudit();
    return () => {
      unlistenP.then((unlisten) => unlisten()).catch(() => {});
    };
  }, [formationPath, setupAudit]);

  useEffect(() => {
    // Populate the Working Set panel as soon as a formation is open.
    // Refreshed after every chat turn by ChatPane. This initial load
    // covers the launch/restore path.
    if (!formationPath) return;
    tauri
      .getWorkingSet()
      .then(setWorkingSet)
      .catch(() => {});
  }, [formationPath, setWorkingSet]);

  const runModelCheck = useCallback(() => {
    // Check the local embedding model is installed. Re-run on launch,
    // formation switch, and a models-directory change in Settings.
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

  useEffect(() => {
    runModelCheck();
  }, [runModelCheck]);

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
    <div className="relative flex h-full w-full flex-col bg-zinc-50 text-zinc-900 dark:bg-zinc-950 dark:text-zinc-100">
      <TitleBar
        version={version}
        reminderCount={openTaskCount}
        onToggleReminders={() => setRemindersOpen((o) => !o)}
        onOpenSettings={() => setSettingsOpen(true)}
      />
      {remindersOpen && (
        <>
          <button
            type="button"
            aria-label="Close reminders"
            className="fixed inset-0 z-40 cursor-default"
            onClick={() => setRemindersOpen(false)}
          />
          <RemindersPopover onClose={() => setRemindersOpen(false)} />
        </>
      )}
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
        <section className="flex basis-2/5 flex-col">
          <WorkingSetPanel />
          <div className="min-h-0 flex-1">
            <ChatPane />
          </div>
        </section>
      </main>
      <AuditLog />
      <UndoToast />
      <ReminderToast />
      {settingsOpen && (
        <SettingsModal
          onClose={() => setSettingsOpen(false)}
          onModelConfigChanged={runModelCheck}
        />
      )}
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

function TitleBar({
  version,
  reminderCount,
  onToggleReminders,
  onOpenSettings,
}: {
  version: string;
  reminderCount: number;
  onToggleReminders: () => void;
  onOpenSettings: () => void;
}) {
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
        onClick={onToggleReminders}
        aria-label="Reminders"
        className="relative ml-auto rounded px-1.5 text-zinc-400 hover:bg-zinc-100 hover:text-zinc-700 dark:hover:bg-zinc-800 dark:hover:text-zinc-200"
      >
        <span aria-hidden>🔔</span>
        {reminderCount > 0 && (
          <span className="absolute -right-0.5 -top-0.5 flex h-3.5 min-w-[14px] items-center justify-center rounded-full bg-rose-500 px-1 text-[9px] font-medium text-white">
            {reminderCount}
          </span>
        )}
      </button>
      <button
        type="button"
        onClick={onOpenSettings}
        aria-label="Settings"
        className="mr-2 ml-1 rounded px-1.5 text-zinc-400 hover:bg-zinc-100 hover:text-zinc-700 dark:hover:bg-zinc-800 dark:hover:text-zinc-200"
      >
        ⚙
      </button>
    </header>
  );
}
