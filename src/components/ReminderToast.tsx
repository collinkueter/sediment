import { Icon } from "@/components/icons";
import { useRemindersStore } from "@/lib/store";

/// Floating toast shown when the scheduler fires a reminder (ADR-0007). Sits
/// above the undo toast so the two never collide. Dismiss leaves the task on
/// the list; Done completes it outright.
export function ReminderToast() {
  const task = useRemindersStore((s) => s.dueToast);
  const complete = useRemindersStore((s) => s.complete);
  const dismiss = useRemindersStore((s) => s.dismissToast);

  if (!task) return null;

  return (
    <div className="-translate-x-1/2 fixed bottom-20 left-1/2 z-50 flex items-center gap-3 rounded-xl bg-accent px-4 py-2.5 text-sm text-white shadow-2xl">
      <Icon.Bell className="h-4 w-4 shrink-0" />
      <span>
        Reminder: <span className="font-semibold">{task.title}</span>
      </span>
      <button
        type="button"
        onClick={() => void complete(task.id)}
        className="rounded-md bg-white/15 px-2.5 py-1 font-semibold text-xs hover:bg-white/25"
      >
        Done
      </button>
      <button
        type="button"
        aria-label="Dismiss"
        onClick={dismiss}
        className="grid h-6 w-6 place-items-center rounded-md text-white/70 hover:bg-white/15 hover:text-white"
      >
        <Icon.X className="h-4 w-4" />
      </button>
    </div>
  );
}
