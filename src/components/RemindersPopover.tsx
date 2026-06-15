import { useRemindersStore } from "@/lib/store";
import type { Task } from "@/lib/tauri";

/// Dropdown panel listing every reminder, opened from the title-bar bell.
/// Open tasks come first (overdue, then soonest-due, then undated); completed
/// tasks follow, dimmed. Each open task can be completed or snoozed a day.
export function RemindersPopover({ onClose }: { onClose: () => void }) {
  const tasks = useRemindersStore((s) => s.tasks);
  const open = tasks.filter((t) => t.status === "open").sort(byDue);
  const done = tasks.filter((t) => t.status === "done");

  return (
    <div className="absolute top-9 right-2 z-50 w-80 overflow-hidden rounded-lg border border-zinc-200 bg-white shadow-xl dark:border-zinc-700 dark:bg-zinc-900">
      <header className="flex items-center justify-between border-b border-zinc-200 px-3 py-2 dark:border-zinc-800">
        <span className="text-xs font-medium text-zinc-600 dark:text-zinc-300">Reminders</span>
        <button
          type="button"
          aria-label="Close reminders"
          onClick={onClose}
          className="rounded px-1 text-zinc-400 hover:bg-zinc-100 hover:text-zinc-700 dark:hover:bg-zinc-800 dark:hover:text-zinc-200"
        >
          ✕
        </button>
      </header>
      <div className="max-h-96 overflow-auto py-1">
        {open.length === 0 && done.length === 0 ? (
          <p className="px-3 py-4 text-xs text-zinc-400 dark:text-zinc-500">
            No reminders yet. When you mention a reminder in conversation, Sediment records it here.
          </p>
        ) : (
          <>
            {open.map((task) => (
              <ReminderRow key={task.id} task={task} />
            ))}
            {done.length > 0 && (
              <div className="mt-1 border-t border-zinc-100 pt-1 dark:border-zinc-800">
                <p className="px-3 py-1 text-[10px] uppercase tracking-wide text-zinc-400 dark:text-zinc-600">
                  Completed
                </p>
                {done.map((task) => (
                  <ReminderRow key={task.id} task={task} />
                ))}
              </div>
            )}
          </>
        )}
      </div>
    </div>
  );
}

function ReminderRow({ task }: { task: Task }) {
  const complete = useRemindersStore((s) => s.complete);
  const snooze = useRemindersStore((s) => s.snooze);
  const due = dueInfo(task.due);
  const isDone = task.status === "done";

  return (
    <div className="flex items-start gap-2 px-3 py-1.5 hover:bg-zinc-50 dark:hover:bg-zinc-800/40">
      <span aria-hidden className="mt-0.5 text-xs text-zinc-400 dark:text-zinc-500">
        {isDone ? "☑" : "☐"}
      </span>
      <div className="min-w-0 flex-1">
        <p
          className={`truncate text-xs ${
            isDone
              ? "text-zinc-400 line-through dark:text-zinc-600"
              : "text-zinc-700 dark:text-zinc-200"
          }`}
        >
          {task.title}
        </p>
        {due && (
          <p
            className={`text-[10px] ${
              due.overdue && !isDone
                ? "text-rose-600 dark:text-rose-400"
                : "text-zinc-400 dark:text-zinc-500"
            }`}
          >
            {due.overdue && !isDone ? `Overdue · ${due.label}` : `Due ${due.label}`}
          </p>
        )}
      </div>
      {!isDone && (
        <div className="flex shrink-0 gap-1">
          <button
            type="button"
            onClick={() => void complete(task.id)}
            className="rounded px-1.5 py-0.5 text-[10px] text-zinc-500 hover:bg-zinc-200 dark:text-zinc-400 dark:hover:bg-zinc-700"
          >
            Done
          </button>
          <button
            type="button"
            onClick={() => void snooze(task.id, tomorrow())}
            className="rounded px-1.5 py-0.5 text-[10px] text-zinc-500 hover:bg-zinc-200 dark:text-zinc-400 dark:hover:bg-zinc-700"
          >
            Snooze 1d
          </button>
        </div>
      )}
    </div>
  );
}

/// Sort key for open tasks: dated tasks ascending (overdue → soonest), then
/// undated tasks by creation order. RFC3339 strings sort chronologically.
function byDue(a: Task, b: Task): number {
  if (a.due && b.due) return a.due.localeCompare(b.due);
  if (a.due) return -1;
  if (b.due) return 1;
  return a.created.localeCompare(b.created);
}

function dueInfo(due: string | null): { label: string; overdue: boolean } | null {
  if (!due) return null;
  const date = new Date(due);
  return {
    label: date.toLocaleDateString(undefined, { month: "short", day: "numeric" }),
    overdue: date.getTime() < Date.now(),
  };
}

/// An RFC3339 timestamp 24 hours from now — the "Snooze 1d" target.
function tomorrow(): string {
  return new Date(Date.now() + 24 * 60 * 60 * 1000).toISOString();
}
