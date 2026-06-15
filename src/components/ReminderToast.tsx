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
    <div className="-translate-x-1/2 fixed bottom-24 left-1/2 z-50 flex items-center gap-3 rounded-lg bg-rose-600 px-4 py-2 text-sm text-white shadow-lg">
      <span>
        <span aria-hidden>🔔</span> Reminder: <span className="font-medium">{task.title}</span>
      </span>
      <button
        type="button"
        onClick={() => void complete(task.id)}
        className="rounded bg-white/15 px-2 py-0.5 text-xs font-medium hover:bg-white/25"
      >
        Done
      </button>
      <button
        type="button"
        aria-label="Dismiss"
        onClick={dismiss}
        className="text-white/70 hover:text-white"
      >
        ✕
      </button>
    </div>
  );
}
