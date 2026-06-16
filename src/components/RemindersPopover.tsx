import { Icon } from "@/components/icons";
import { useFormationStore, useRemindersStore, useWorkingSetStore } from "@/lib/store";
import type { OpenLoop, Task } from "@/lib/tauri";
import { useUiStore } from "@/lib/ui";

/// Dropdown panel listing tasks & reminders, opened from the title-bar bell.
/// Open tasks come first (overdue, then soonest-due, then undated) under "Due
/// soon"; each can be completed or snoozed a day. Below them, an "Open loops"
/// section surfaces unresolved threads the agent noticed.
export function RemindersPopover({ onClose }: { onClose: () => void }) {
  const tasks = useRemindersStore((s) => s.tasks);
  const openLoops = useWorkingSetStore((s) => s.workingSet)?.openLoops ?? [];
  const notes = useFormationStore((s) => s.notes);
  const openNote = useFormationStore((s) => s.openNote);
  const open = tasks.filter((t) => t.status === "open").sort(byDue);
  const done = tasks.filter((t) => t.status === "done");

  // "View all" opens the Tasks note, the canonical home of every task.
  const tasksNote = notes.find((n) => n.relative_path.replace(/^.*\//, "") === "Tasks.md");
  function viewAll() {
    if (tasksNote) {
      openNote(tasksNote.relative_path).catch(() => {});
      onClose();
    }
  }

  return (
    <div className="absolute top-12 right-3 z-50 w-[344px] overflow-hidden rounded-2xl border border-line-strong bg-raised shadow-2xl">
      <header className="flex items-center justify-between border-line border-b px-4 py-[13px]">
        <b className="font-semibold text-[13px] text-ink">Tasks &amp; reminders</b>
        <div className="flex items-center gap-1">
          {tasksNote && (
            <button
              type="button"
              onClick={viewAll}
              className="cursor-pointer font-semibold text-[11.5px] text-accent-ink hover:underline"
            >
              View all
            </button>
          )}
          <button
            type="button"
            aria-label="Close reminders"
            onClick={onClose}
            className="grid h-6 w-6 place-items-center rounded-md text-muted hover:bg-bg-sunk hover:text-ink"
          >
            <Icon.X className="h-4 w-4" />
          </button>
        </div>
      </header>

      <div className="max-h-[420px] overflow-y-auto px-2 pt-1.5 pb-2.5">
        {open.length === 0 && done.length === 0 && openLoops.length === 0 ? (
          <p className="px-3 py-4 text-muted text-xs">
            No reminders yet. When you mention a reminder in conversation, Sediment records it here.
          </p>
        ) : (
          <>
            {(open.length > 0 || done.length > 0) && <SectionLabel>Due soon</SectionLabel>}
            {open.map((task) => (
              <ReminderRow key={task.id} task={task} />
            ))}
            {done.map((task) => (
              <ReminderRow key={task.id} task={task} />
            ))}

            {openLoops.length > 0 && (
              <>
                <SectionLabel>Open loops</SectionLabel>
                {openLoops.map((loop) => (
                  <OpenLoopRow key={loop.id} loop={loop} />
                ))}
              </>
            )}
          </>
        )}
      </div>
    </div>
  );
}

function SectionLabel({ children }: { children: React.ReactNode }) {
  return (
    <div className="px-3.5 pt-[11px] pb-1 font-bold text-[10px] text-faint uppercase tracking-[0.06em]">
      {children}
    </div>
  );
}

function ReminderRow({ task }: { task: Task }) {
  const complete = useRemindersStore((s) => s.complete);
  const snooze = useRemindersStore((s) => s.snooze);
  const due = dueInfo(task.due);
  const isDone = task.status === "done";
  const soon = due ? due.overdue || due.soon : false;

  return (
    <div className="group flex gap-2.5 rounded-[9px] px-2.5 py-2 hover:bg-bg-sunk">
      <button
        type="button"
        aria-label={isDone ? "Completed" : "Complete task"}
        onClick={() => !isDone && void complete(task.id)}
        disabled={isDone}
        className={`mt-0.5 grid h-[17px] w-[17px] shrink-0 place-items-center rounded-md border ${
          isDone
            ? "border-sage bg-sage text-white"
            : "border-line-strong bg-raised text-transparent"
        }`}
      >
        <Icon.Check className="h-[11px] w-[11px]" strokeWidth={3} />
      </button>

      <div className="min-w-0 flex-1">
        <p className={`text-[13px] ${isDone ? "text-faint line-through" : "text-ink"}`}>
          {task.title}
        </p>
        {!isDone && (
          <button
            type="button"
            onClick={() => void snooze(task.id, tomorrow())}
            className="mt-0.5 text-[11.5px] text-muted opacity-0 transition-opacity hover:text-accent-ink group-hover:opacity-100"
          >
            Snooze 1d
          </button>
        )}
      </div>

      {due && !isDone && (
        <span
          className={`self-start whitespace-nowrap rounded-md px-2 py-0.5 font-mono text-[10.5px] font-medium ${
            soon ? "bg-accent-tint text-accent-ink" : "bg-gold-tint text-gold"
          }`}
        >
          {due.overdue ? `Overdue · ${due.label}` : due.label}
        </span>
      )}
    </div>
  );
}

function OpenLoopRow({ loop }: { loop: OpenLoop }) {
  const openSettings = useUiStore((s) => s.openSettings);
  // The engine-selection loop deep-links to Settings; other loops are inert.
  const isEngineLoop = /engine/i.test(loop.title);

  const inner = (
    <>
      <span className="mt-0.5 grid h-[17px] w-[17px] shrink-0 place-items-center rounded-full border border-sage border-dashed" />
      <div className="min-w-0 flex-1">
        <p className="text-[13px] text-ink">{loop.title}</p>
        {loop.context && <p className="mt-px text-[11.5px] text-muted">{loop.context}</p>}
      </div>
    </>
  );

  if (isEngineLoop) {
    return (
      <button
        type="button"
        onClick={openSettings}
        className="flex w-full gap-2.5 rounded-[9px] px-2.5 py-2 text-left hover:bg-bg-sunk"
      >
        {inner}
      </button>
    );
  }
  return <div className="flex gap-2.5 rounded-[9px] px-2.5 py-2 hover:bg-bg-sunk">{inner}</div>;
}

/// Sort key for open tasks: dated tasks ascending (overdue → soonest), then
/// undated tasks by creation order. RFC3339 strings sort chronologically.
function byDue(a: Task, b: Task): number {
  if (a.due && b.due) return a.due.localeCompare(b.due);
  if (a.due) return -1;
  if (b.due) return 1;
  return a.created.localeCompare(b.created);
}

function dueInfo(due: string | null): { label: string; overdue: boolean; soon: boolean } | null {
  if (!due) return null;
  const date = new Date(due);
  const ms = date.getTime() - Date.now();
  return {
    label: date.toLocaleDateString(undefined, { month: "short", day: "numeric" }),
    overdue: ms < 0,
    // "Soon" = due within the next two days; tinted with the accent.
    soon: ms >= 0 && ms < 2 * 24 * 60 * 60 * 1000,
  };
}

/// An RFC3339 timestamp 24 hours from now — the "Snooze 1d" target.
function tomorrow(): string {
  return new Date(Date.now() + 24 * 60 * 60 * 1000).toISOString();
}
