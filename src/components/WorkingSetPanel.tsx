import { useFormationStore } from "@/lib/store";
import { useWorkingSetStore } from "@/lib/store";
import type { ActiveEntity, OpenLoop, OpenTask } from "@/lib/tauri";
import { tauri } from "@/lib/tauri";
import { useState } from "react";

/**
 * "What's in play" — the Working Set panel (ADR-0011 §3).
 * Collapsible, compact, mounted above ChatPane in the right column.
 */
export function WorkingSetPanel() {
  const workingSet = useWorkingSetStore((s) => s.workingSet);
  const [open, setOpen] = useState(true);

  // Nothing to show until the backend populates the set.
  const hasContent =
    workingSet &&
    (workingSet.activeEntities.length > 0 ||
      workingSet.recentNotes.length > 0 ||
      workingSet.openTasks.length > 0 ||
      workingSet.openLoops.length > 0);

  if (!workingSet) return null;

  return (
    <div className="border-b border-zinc-200 dark:border-zinc-800">
      {/* Collapsible header */}
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        className="flex w-full items-center justify-between px-4 py-2 text-left hover:bg-zinc-100 dark:hover:bg-zinc-800/50"
      >
        <span className="text-xs font-medium text-zinc-500 dark:text-zinc-400">In play</span>
        <span className="text-[9px] text-zinc-400 dark:text-zinc-500">{open ? "▾" : "▸"}</span>
      </button>

      {open && (
        <div className="space-y-0 pb-2">
          {!hasContent && (
            <p className="px-4 py-1 text-[11px] text-zinc-400 dark:text-zinc-600">Nothing yet.</p>
          )}

          {/* Active entities */}
          {workingSet.activeEntities.length > 0 && (
            <Section label="Entities">
              {workingSet.activeEntities.map((e) => (
                <EntityRow key={`${e.entityType}:${e.name}`} entity={e} />
              ))}
            </Section>
          )}

          {/* Recent notes */}
          {workingSet.recentNotes.length > 0 && (
            <Section label="Recent notes">
              {workingSet.recentNotes.map((path) => (
                <NoteRow key={path} notePath={path} />
              ))}
            </Section>
          )}

          {/* Open tasks */}
          {workingSet.openTasks.length > 0 && (
            <Section label="Tasks">
              {workingSet.openTasks.map((t) => (
                <TaskRow key={t.title} task={t} />
              ))}
            </Section>
          )}

          {/* Open loops */}
          {workingSet.openLoops.length > 0 && (
            <Section label="Open loops">
              {workingSet.openLoops.map((l) => (
                <LoopRow key={l.id} loop={l} />
              ))}
            </Section>
          )}
        </div>
      )}
    </div>
  );
}

function Section({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="mt-1">
      <div className="px-4 pt-1 pb-0.5 text-[9px] font-semibold uppercase tracking-wide text-zinc-400 dark:text-zinc-600">
        {label}
      </div>
      {children}
    </div>
  );
}

function EntityRow({ entity }: { entity: ActiveEntity }) {
  const openNote = useFormationStore((s) => s.openNote);
  const canOpen = entity.notePath !== null;
  return (
    <button
      type="button"
      disabled={!canOpen}
      onClick={() => {
        if (entity.notePath) {
          openNote(entity.notePath).catch(() => {});
        }
      }}
      className={`block w-full truncate px-4 py-0.5 text-left text-[11px] ${
        canOpen
          ? "text-zinc-700 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800/50"
          : "cursor-default text-zinc-500 dark:text-zinc-500"
      }`}
      title={entity.notePath ?? undefined}
    >
      <span className="truncate">{entity.name}</span>
      <span className="ml-1 text-[10px] text-zinc-400 dark:text-zinc-600">{entity.entityType}</span>
    </button>
  );
}

function NoteRow({ notePath }: { notePath: string }) {
  const openNote = useFormationStore((s) => s.openNote);
  const displayName = notePath.endsWith(".md") ? notePath.slice(0, -3) : notePath;
  return (
    <button
      type="button"
      onClick={() => openNote(notePath).catch(() => {})}
      className="block w-full truncate px-4 py-0.5 text-left text-[11px] text-zinc-700 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800/50"
      title={notePath}
    >
      {displayName}
    </button>
  );
}

function TaskRow({ task }: { task: OpenTask }) {
  return (
    <div className="flex items-baseline gap-2 px-4 py-0.5">
      <span className="min-w-0 flex-1 truncate text-[11px] text-zinc-700 dark:text-zinc-300">
        {task.title}
      </span>
      {task.due && (
        <span className="shrink-0 text-[10px] text-zinc-400 dark:text-zinc-500">{task.due}</span>
      )}
    </div>
  );
}

function LoopRow({ loop }: { loop: OpenLoop }) {
  const removeOpenLoop = useWorkingSetStore((s) => s.removeOpenLoop);

  function handleDismiss() {
    removeOpenLoop(loop.id);
    tauri.dismissOpenLoop(loop.id).catch(() => {
      // Optimistic removal already happened — a failure here is silent so the
      // UX stays clean. The loop will reappear on the next Working Set refresh
      // if the backend didn't persist the dismissal.
    });
  }

  return (
    <div className="flex items-start gap-1 px-4 py-0.5">
      <div className="min-w-0 flex-1">
        <div className="truncate text-[11px] text-zinc-700 dark:text-zinc-300">{loop.title}</div>
        {loop.context && (
          <div className="truncate text-[10px] text-zinc-400 dark:text-zinc-500">
            {loop.context}
          </div>
        )}
      </div>
      <button
        type="button"
        onClick={handleDismiss}
        aria-label={`Dismiss: ${loop.title}`}
        className="mt-0.5 shrink-0 rounded px-1 text-[10px] text-zinc-400 hover:bg-zinc-100 hover:text-zinc-600 dark:text-zinc-600 dark:hover:bg-zinc-800 dark:hover:text-zinc-400"
      >
        ×
      </button>
    </div>
  );
}
