import { AuditLog } from "@/components/AuditLog";
import { ChatPane } from "@/components/ChatPane";
import { CommandPalette } from "@/components/CommandPalette";
import { FileTree } from "@/components/FileTree";
import { FormationPicker } from "@/components/FormationPicker";
import { InFocusBar } from "@/components/InFocusBar";
import { MeetingSessionBar } from "@/components/MeetingSessionBar";
import { ModelSetup } from "@/components/ModelSetup";
import { NoteViewer } from "@/components/NoteViewer";
import { Onboarding } from "@/components/Onboarding";
import { ReminderToast } from "@/components/ReminderToast";
import { RemindersView } from "@/components/RemindersView";
import { SettingsModal } from "@/components/SettingsModal";
import { TitleBar } from "@/components/TitleBar";
import { UndoToast } from "@/components/UndoToast";
import {
  useAuditStore,
  useFormationStore,
  useRemindersStore,
  useWorkingSetStore,
} from "@/lib/store";
import { type ModelReadiness, type Task, tauri } from "@/lib/tauri";
import { useUiStore } from "@/lib/ui";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useRef, useState } from "react";

interface FormationChangeEvent {
  kind: string;
  paths: string[];
}

const LEFT_MIN = 200;
const LEFT_MAX = 360;
const RIGHT_MIN = 300;
const RIGHT_MAX = 560;

function clamp(v: number, min: number, max: number) {
  return Math.min(max, Math.max(min, v));
}

function readWidth(key: string, fallback: number, min: number, max: number) {
  if (typeof window === "undefined") return fallback;
  const v = Number(window.localStorage.getItem(key));
  return Number.isFinite(v) && v > 0 ? clamp(v, min, max) : fallback;
}

export default function App() {
  const [onboardingComplete, setOnboardingComplete] = useState<boolean | null>(null);
  const restore = useFormationStore((s) => s.restore);
  const handleExternalChange = useFormationStore((s) => s.handleExternalChange);
  const formationPath = useFormationStore((s) => s.formationPath);
  const refreshAudit = useAuditStore((s) => s.refresh);
  const setupAudit = useAuditStore((s) => s.setup);
  const setWorkingSet = useWorkingSetStore((s) => s.setWorkingSet);
  const setSelfSummary = useWorkingSetStore((s) => s.setSelfSummary);
  const [modelReadiness, setModelReadiness] = useState<ModelReadiness | null>(null);
  const [modelsChecked, setModelsChecked] = useState(false);
  const refreshReminders = useRemindersStore((s) => s.refresh);
  const showDueToast = useRemindersStore((s) => s.showDueToast);
  const openTaskCount = useRemindersStore((s) => s.tasks.filter((t) => t.status === "open").length);

  const settingsOpen = useUiStore((s) => s.settingsOpen);
  const closeSettings = useUiStore((s) => s.closeSettings);
  const openSettings = useUiStore((s) => s.openSettings);
  const togglePalette = useUiStore((s) => s.togglePalette);
  const toggleNotePane = useUiStore((s) => s.toggleNotePane);
  const notePaneCollapsed = useUiStore((s) => s.notePaneCollapsed);
  const closeAllOverlays = useUiStore((s) => s.closeAllOverlays);

  useEffect(() => {
    restore().catch((e) => console.error("restore formation failed:", e));
    // Ollama backs only the `ollama` embedding provider now (the agent runs on
    // a CLI, ADR-0009). Start the daemon ahead of the first search only when
    // that provider is selected — bundled/keyword users need no daemon. Errors
    // are non-fatal.
    tauri
      .getEmbeddingProvider()
      .then((p) => {
        if (p === "ollama") {
          tauri.ollamaEnsureRunning().catch((e) => console.warn("ollama ensure failed:", e));
        }
      })
      .catch(() => {});
    tauri
      .getOnboardingState()
      .then((s) => setOnboardingComplete(s.complete))
      .catch(() => setOnboardingComplete(false));
  }, [restore]);

  useEffect(() => {
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
    if (formationPath) {
      refreshAudit().catch(() => {});
    }
  }, [formationPath, refreshAudit]);

  useEffect(() => {
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
    if (!formationPath) return;
    const unlistenP = setupAudit();
    return () => {
      unlistenP.then((unlisten) => unlisten()).catch(() => {});
    };
  }, [formationPath, setupAudit]);

  useEffect(() => {
    if (!formationPath) return;
    tauri
      .getWorkingSet()
      .then(setWorkingSet)
      .catch(() => {});
    tauri
      .getSelfSummary()
      .then(setSelfSummary)
      .catch(() => {});
  }, [formationPath, setWorkingSet, setSelfSummary]);

  // Global shortcuts: ⌘K command palette, ⌘\ toggle note pane, ⌘, Settings, Esc closes overlays.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if ((e.metaKey || e.ctrlKey) && e.key === "k") {
        e.preventDefault();
        togglePalette();
      } else if ((e.metaKey || e.ctrlKey) && e.key === "\\") {
        e.preventDefault();
        toggleNotePane();
      } else if ((e.metaKey || e.ctrlKey) && e.key === ",") {
        e.preventDefault();
        openSettings();
      } else if (e.key === "Escape") {
        closeAllOverlays();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [togglePalette, toggleNotePane, openSettings, closeAllOverlays]);

  const runModelCheck = useCallback(() => {
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

  if (onboardingComplete === false) {
    return <Onboarding onComplete={() => setOnboardingComplete(true)} />;
  }
  if (onboardingComplete === true && formationPath && !modelsChecked) {
    return <CheckingModels />;
  }
  if (modelReadiness && !modelReadiness.all_present) {
    return <ModelSetup readiness={modelReadiness} onComplete={() => setModelReadiness(null)} />;
  }

  return (
    <div className="relative flex h-full w-full flex-col bg-bg text-ink">
      <TitleBar openTaskCount={openTaskCount} />

      <main className="flex min-h-0 flex-1">
        {formationPath ? <Workspace collapsedNote={notePaneCollapsed} /> : <FormationPicker />}
      </main>

      <AuditLog />
      <CommandPalette />
      <UndoToast />
      <ReminderToast />
      {settingsOpen && (
        <SettingsModal onClose={closeSettings} onModelConfigChanged={runModelCheck} />
      )}
    </div>
  );
}

/** The three-column resizable workspace: Formation · Conversation (hero) · Note. */
function Workspace({ collapsedNote }: { collapsedNote: boolean }) {
  const view = useUiStore((s) => s.view);
  const [leftWidth, setLeftWidth] = useState(() =>
    readWidth("sediment.leftWidth", 240, LEFT_MIN, LEFT_MAX),
  );
  const [rightWidth, setRightWidth] = useState(() =>
    readWidth("sediment.rightWidth", 392, RIGHT_MIN, RIGHT_MAX),
  );

  const persist = useCallback((key: string, value: number) => {
    window.localStorage.setItem(key, String(Math.round(value)));
  }, []);

  return (
    <div className="flex min-h-0 w-full flex-1">
      <aside className="flex min-h-0 flex-none flex-col" style={{ width: leftWidth }}>
        <FileTree />
      </aside>
      <ResizeDivider
        onDelta={(dx) => setLeftWidth((w) => clamp(w + dx, LEFT_MIN, LEFT_MAX))}
        onCommit={() => persist("sediment.leftWidth", leftWidth)}
      />

      <section className="flex min-h-0 min-w-0 flex-1 flex-col bg-bg">
        {view === "reminders" ? (
          <RemindersView />
        ) : (
          <>
            <MeetingSessionBar />
            <InFocusBar />
            <div className="min-h-0 flex-1">
              <ChatPane />
            </div>
          </>
        )}
      </section>

      {/* The note pane belongs to the Conversation view; Reminders takes the
          full canvas beside the sidebar so it reads as its own section. */}
      {view === "chat" && !collapsedNote && (
        <>
          <ResizeDivider
            onDelta={(dx) => setRightWidth((w) => clamp(w - dx, RIGHT_MIN, RIGHT_MAX))}
            onCommit={() => persist("sediment.rightWidth", rightWidth)}
          />
          <aside className="flex min-h-0 flex-none flex-col" style={{ width: rightWidth }}>
            <NoteViewer />
          </aside>
        </>
      )}
    </div>
  );
}

/** A draggable column divider with a hairline that lights up on hover. */
function ResizeDivider({
  onDelta,
  onCommit,
}: {
  onDelta: (dx: number) => void;
  onCommit: () => void;
}) {
  const last = useRef(0);
  const dragging = useRef(false);

  // Keyboard resize: arrow keys nudge the boundary in 24px steps.
  function onKeyDown(e: React.KeyboardEvent) {
    const step = e.shiftKey ? 64 : 24;
    if (e.key === "ArrowLeft") {
      e.preventDefault();
      onDelta(-step);
      onCommit();
    } else if (e.key === "ArrowRight") {
      e.preventDefault();
      onDelta(step);
      onCommit();
    }
  }

  return (
    // biome-ignore lint/a11y/useSemanticElements: a draggable column splitter has no native element; role="separator" is the ARIA-correct choice
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize panel — arrow keys to adjust"
      title="Drag, or focus and use arrow keys, to resize"
      tabIndex={0}
      className="group relative z-10 w-[7px] flex-none cursor-col-resize focus:outline-none"
      onKeyDown={onKeyDown}
      onPointerDown={(e) => {
        dragging.current = true;
        last.current = e.clientX;
        e.currentTarget.setPointerCapture(e.pointerId);
      }}
      onPointerMove={(e) => {
        if (!dragging.current) return;
        const dx = e.clientX - last.current;
        last.current = e.clientX;
        if (dx !== 0) onDelta(dx);
      }}
      onPointerUp={(e) => {
        if (!dragging.current) return;
        dragging.current = false;
        e.currentTarget.releasePointerCapture(e.pointerId);
        onCommit();
      }}
    >
      <span className="pointer-events-none absolute inset-y-0 left-1/2 w-px -translate-x-1/2 bg-line transition-colors group-hover:bg-accent group-focus:bg-accent" />
      <span className="pointer-events-none absolute top-1/2 left-1/2 h-8 w-[3px] -translate-x-1/2 -translate-y-1/2 rounded-full bg-accent opacity-0 transition-opacity group-hover:opacity-90 group-focus:opacity-90" />
    </div>
  );
}

function CheckingModels() {
  return (
    <div className="flex h-full w-full items-center justify-center bg-bg text-sm text-muted">
      Checking models…
    </div>
  );
}
