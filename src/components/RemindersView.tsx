import { Segmented } from "@/components/Segmented";
import { Icon } from "@/components/icons";
import { useFormationStore, useRemindersStore } from "@/lib/store";
import type { Task } from "@/lib/tauri";
import { useUiStore } from "@/lib/ui";
import { useState } from "react";

/// The Reminders section — a full-column home for every task the agent has
/// filed (ADR-0007). Reminders are grouped by when they're due (Overdue, Today,
/// Upcoming, Someday) so the most pressing sit at the top; each can be completed
/// or snoozed. A filter flips between active reminders, completed ones, or all.
/// This is the canonical surface; the title-bar bell and left-nav both open it.

type Filter = "active" | "done" | "all";

interface Group {
  key: string;
  label: string;
  /** Lead-dot / count tint for the group header. */
  tone: "danger" | "accent" | "gold" | "sage" | "muted";
  tasks: Task[];
}

export function RemindersView() {
  const tasks = useRemindersStore((s) => s.tasks);
  const showChat = useUiStore((s) => s.showChat);
  const [filter, setFilter] = useState<Filter>("active");

  const now = Date.now();
  const open = tasks.filter((t) => t.status === "open");
  const done = tasks.filter((t) => t.status === "done");
  const overdueCount = open.filter((t) => isOverdue(t.due, now)).length;

  const groups = buildGroups(open, done, filter, now);
  const isEmpty = groups.every((g) => g.tasks.length === 0);

  return (
    <div className="flex h-full min-h-0 flex-col bg-bg">
      {/* Header */}
      <header className="flex flex-none items-start gap-3 border-b border-line bg-surface px-6 py-3.5">
        <span
          className="mt-px grid h-8 w-8 flex-none place-items-center rounded-[9px] text-white"
          style={{ background: "linear-gradient(150deg, var(--accent), var(--accent-ink))" }}
          aria-hidden="true"
        >
          <Icon.Bell className="h-[17px] w-[17px]" />
        </span>
        <div className="min-w-0 flex-1">
          <h1 className="font-serif text-[19px] font-semibold leading-tight tracking-tight text-ink">
            Reminders
          </h1>
          <p className="mt-0.5 text-[12px] text-muted">
            <Summary open={open.length} overdue={overdueCount} done={done.length} />
          </p>
        </div>
        <div className="flex flex-none items-center gap-2">
          <Segmented<Filter>
            value={filter}
            onChange={setFilter}
            ariaLabel="Filter reminders"
            options={[
              { value: "active", label: "Active" },
              { value: "done", label: "Completed" },
              { value: "all", label: "All" },
            ]}
          />
          <button
            type="button"
            onClick={showChat}
            aria-label="Back to conversation"
            title="Back to conversation"
            className="grid h-[30px] w-[30px] place-items-center rounded-lg text-muted transition-colors hover:bg-bg-sunk hover:text-ink"
          >
            <Icon.Chat className="h-[17px] w-[17px]" />
          </button>
        </div>
      </header>

      {/* Body */}
      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto w-full max-w-[680px] px-6 py-6">
          {isEmpty ? (
            <EmptyState filter={filter} onOpenChat={showChat} />
          ) : (
            <div className="flex flex-col gap-7">
              {groups.map(
                (group) => group.tasks.length > 0 && <GroupBlock key={group.key} group={group} />,
              )}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function Summary({ open, overdue, done }: { open: number; overdue: number; done: number }) {
  if (open === 0 && done === 0) return <>Nothing scheduled — all clear.</>;
  const parts: React.ReactNode[] = [];
  parts.push(
    <span key="active">
      <b className="font-semibold text-ink-soft">{open}</b> active
    </span>,
  );
  if (overdue > 0) {
    parts.push(
      <span key="overdue" className="text-danger">
        <b className="font-semibold">{overdue}</b> overdue
      </span>,
    );
  }
  if (done > 0) {
    parts.push(
      <span key="done">
        <b className="font-semibold text-ink-soft">{done}</b> completed
      </span>,
    );
  }
  return (
    <>
      {parts.map((p, i) => (
        // biome-ignore lint/suspicious/noArrayIndexKey: static, order-stable summary segments
        <span key={i}>
          {i > 0 && <span className="px-1.5 text-faint">·</span>}
          {p}
        </span>
      ))}
    </>
  );
}

function GroupBlock({ group }: { group: Group }) {
  const toneText = {
    danger: "text-danger",
    accent: "text-accent-ink",
    gold: "text-gold",
    sage: "text-sage",
    muted: "text-faint",
  }[group.tone];

  return (
    <section>
      <div className="mb-1.5 flex items-center gap-2 px-1">
        <span
          className={`inline-block h-[6px] w-[6px] rounded-full ${
            {
              danger: "bg-danger",
              accent: "bg-accent",
              gold: "bg-gold",
              sage: "bg-sage",
              muted: "bg-faint",
            }[group.tone]
          }`}
          aria-hidden="true"
        />
        <h2 className={`text-[10.5px] font-bold uppercase tracking-[0.07em] ${toneText}`}>
          {group.label}
        </h2>
        <span className="text-[10.5px] font-semibold text-faint">{group.tasks.length}</span>
        <span className="ml-1 h-px flex-1 bg-line" aria-hidden="true" />
      </div>
      <ul className="flex flex-col">
        {group.tasks.map((task) => (
          <ReminderRow key={task.id} task={task} />
        ))}
      </ul>
    </section>
  );
}

function ReminderRow({ task }: { task: Task }) {
  const complete = useRemindersStore((s) => s.complete);
  const snooze = useRemindersStore((s) => s.snooze);
  const reschedule = useRemindersStore((s) => s.reschedule);
  const openNote = useFormationStore((s) => s.openNote);
  const notes = useFormationStore((s) => s.notes);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const isDone = task.status === "done";
  const due = describeDue(isDone ? task.completed_at : task.due, Date.now(), isDone);

  const startEditing = () => {
    setDraft(toLocalInputValue(task.due));
    setEditing(true);
  };
  const save = () => {
    if (!draft) return;
    void reschedule(task.id, new Date(draft).toISOString());
    setEditing(false);
  };
  const clearDate = () => {
    void reschedule(task.id, null);
    setEditing(false);
  };

  // Reminders trace back to Tasks.md — let a row deep-link to it for full context.
  const tasksNote = notes.find((n) => n.relative_path.replace(/^.*\//, "") === "Tasks.md");

  return (
    <li className="group flex items-start gap-3 rounded-[10px] px-2.5 py-2.5 transition-colors hover:bg-surface">
      <button
        type="button"
        aria-label={isDone ? `${task.title} — completed` : `Complete ${task.title}`}
        onClick={() => !isDone && void complete(task.id)}
        disabled={isDone}
        className={`mt-px grid h-[19px] w-[19px] shrink-0 place-items-center rounded-md border transition-colors ${
          isDone
            ? "border-sage bg-sage text-white"
            : "border-line-strong bg-raised text-transparent hover:border-sage hover:text-sage"
        }`}
      >
        <Icon.Check className="h-[12px] w-[12px]" strokeWidth={3} />
      </button>

      <div className="min-w-0 flex-1">
        <p
          className={`text-[14px] leading-snug ${
            isDone ? "text-faint line-through decoration-line-strong" : "text-ink"
          }`}
        >
          {task.title}
        </p>
        <div className="mt-1 flex items-center gap-2.5">
          {due && (
            <span
              className={`inline-flex items-center gap-1 font-mono text-[10.5px] font-medium ${dueToneText(due.tone)}`}
            >
              {!isDone && due.tone !== "none" && <Icon.Clock className="h-[11px] w-[11px]" />}
              {due.label}
            </span>
          )}
          {!isDone && (
            <button
              type="button"
              onClick={() => (editing ? setEditing(false) : startEditing())}
              aria-expanded={editing}
              className={`text-[11px] transition-opacity hover:text-accent-ink group-hover:opacity-100 ${
                editing ? "text-accent-ink opacity-100" : "text-muted opacity-0"
              }`}
            >
              Reschedule
            </button>
          )}
          {!isDone && (
            <button
              type="button"
              onClick={() => void snooze(task.id, tomorrow())}
              className="text-[11px] text-muted opacity-0 transition-opacity hover:text-accent-ink group-hover:opacity-100"
            >
              Snooze 1 day
            </button>
          )}
          {!isDone && tasksNote && (
            <button
              type="button"
              onClick={() => void openNote(tasksNote.relative_path)}
              className="text-[11px] text-muted opacity-0 transition-opacity hover:text-accent-ink group-hover:opacity-100"
            >
              Open in note
            </button>
          )}
        </div>

        {editing && !isDone && (
          <div className="mt-2 flex flex-wrap items-center gap-2">
            <input
              type="datetime-local"
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              aria-label={`Due date and time for ${task.title}`}
              className="rounded-md border border-line bg-raised px-2 py-1 font-mono text-[11px] text-ink-soft outline-none focus:border-accent"
            />
            <button
              type="button"
              onClick={save}
              disabled={!draft}
              className="rounded-md bg-accent px-2.5 py-1 text-[11px] font-semibold text-white transition-opacity hover:opacity-90 disabled:opacity-40"
            >
              Save
            </button>
            {task.due && (
              <button
                type="button"
                onClick={clearDate}
                className="rounded-md px-2 py-1 text-[11px] font-medium text-muted transition-colors hover:text-danger"
              >
                Clear date
              </button>
            )}
            <button
              type="button"
              onClick={() => setEditing(false)}
              className="rounded-md px-2 py-1 text-[11px] font-medium text-muted transition-colors hover:text-ink"
            >
              Cancel
            </button>
          </div>
        )}
      </div>
    </li>
  );
}

function EmptyState({ filter, onOpenChat }: { filter: Filter; onOpenChat: () => void }) {
  if (filter === "done") {
    return (
      <Centered>
        <p className="font-serif text-[16px] text-ink-soft">Nothing completed yet</p>
        <p className="mt-1.5 max-w-[34ch] text-[13px] text-muted">
          Reminders you check off will gather here.
        </p>
      </Centered>
    );
  }
  return (
    <Centered>
      <span
        className="mb-4 grid h-12 w-12 place-items-center rounded-2xl border border-line bg-surface text-muted"
        aria-hidden="true"
      >
        <Icon.Bell className="h-6 w-6" />
      </span>
      <p className="font-serif text-[16px] text-ink-soft">No reminders yet</p>
      <p className="mt-1.5 max-w-[40ch] text-[13px] leading-relaxed text-muted">
        Mention something you need to do in conversation — “remind me to send the rubric Friday” —
        and the agent files it here.
      </p>
      <button
        type="button"
        onClick={onOpenChat}
        className="mt-4 inline-flex items-center gap-1.5 rounded-lg border border-line bg-raised px-3 py-1.5 text-[12.5px] font-medium text-ink-soft shadow-sm transition-colors hover:border-line-strong hover:text-ink"
      >
        <Icon.Chat className="h-4 w-4" />
        Start a conversation
      </button>
    </Centered>
  );
}

function Centered({ children }: { children: React.ReactNode }) {
  return <div className="flex flex-col items-center px-6 pt-16 pb-10 text-center">{children}</div>;
}

// ── Grouping & date helpers ──────────────────────────────────────────────────

/// Build the ordered, filtered groups. Active reminders bucket by due date
/// (Overdue → Today → Upcoming → Someday); completed ones get a single group.
function buildGroups(open: Task[], done: Task[], filter: Filter, now: number): Group[] {
  const groups: Group[] = [];

  if (filter !== "done") {
    const overdue: Task[] = [];
    const today: Task[] = [];
    const upcoming: Task[] = [];
    const someday: Task[] = [];
    for (const t of [...open].sort(byDue)) {
      if (!t.due) someday.push(t);
      else if (isOverdue(t.due, now)) overdue.push(t);
      else if (dayDiff(t.due, now) === 0) today.push(t);
      else upcoming.push(t);
    }
    groups.push(
      { key: "overdue", label: "Overdue", tone: "danger", tasks: overdue },
      { key: "today", label: "Today", tone: "accent", tasks: today },
      { key: "upcoming", label: "Upcoming", tone: "gold", tasks: upcoming },
      { key: "someday", label: "Someday", tone: "sage", tasks: someday },
    );
  }

  if (filter !== "active") {
    const doneSorted = [...done].sort((a, b) =>
      (b.completed_at ?? b.created).localeCompare(a.completed_at ?? a.created),
    );
    groups.push({ key: "done", label: "Completed", tone: "muted", tasks: doneSorted });
  }

  return groups;
}

/// Sort key for open tasks: dated ascending (soonest first), then undated by
/// creation order. RFC3339 strings sort chronologically.
function byDue(a: Task, b: Task): number {
  if (a.due && b.due) return a.due.localeCompare(b.due);
  if (a.due) return -1;
  if (b.due) return 1;
  return a.created.localeCompare(b.created);
}

function isOverdue(due: string | null, now: number): boolean {
  if (!due) return false;
  return dayDiff(due, now) < 0;
}

/// Whole-day difference between a due date and now, at local midnight: negative
/// is in the past, 0 is today, positive is in the future.
function dayDiff(due: string, now: number): number {
  const d = new Date(due);
  const dueMid = new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const n = new Date(now);
  const nowMid = new Date(n.getFullYear(), n.getMonth(), n.getDate()).getTime();
  return Math.round((dueMid - nowMid) / 86_400_000);
}

type DueTone = "danger" | "accent" | "gold" | "none";

/// A human label + urgency tone for a due/completed timestamp.
function describeDue(
  ts: string | null,
  now: number,
  isDone: boolean,
): { label: string; tone: DueTone } | null {
  if (!ts) return isDone ? null : { label: "No date", tone: "none" };
  const diff = dayDiff(ts, now);
  if (isDone) {
    if (diff === 0) return { label: "Completed today", tone: "none" };
    if (diff === -1) return { label: "Completed yesterday", tone: "none" };
    return { label: `Completed ${shortDate(ts)}`, tone: "none" };
  }
  if (diff < 0) {
    const ago = -diff;
    return { label: `Overdue · ${ago === 1 ? "yesterday" : `${ago} days ago`}`, tone: "danger" };
  }
  if (diff === 0) return { label: "Today", tone: "accent" };
  if (diff === 1) return { label: "Tomorrow", tone: "accent" };
  if (diff <= 6) return { label: weekday(ts), tone: "gold" };
  return { label: shortDate(ts), tone: "gold" };
}

function dueToneText(tone: DueTone): string {
  switch (tone) {
    case "danger":
      return "text-danger";
    case "accent":
      return "text-accent-ink";
    case "gold":
      return "text-gold";
    default:
      return "text-faint";
  }
}

const WEEKDAYS = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

function weekday(ts: string): string {
  return WEEKDAYS[new Date(ts).getDay()] ?? shortDate(ts);
}

function shortDate(ts: string): string {
  return new Date(ts).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

/// An RFC3339 timestamp 24 hours from now — the "Snooze 1 day" target.
function tomorrow(): string {
  return new Date(Date.now() + 24 * 60 * 60 * 1000).toISOString();
}

/// Convert an RFC3339 due (or null) to the `YYYY-MM-DDTHH:mm` shape a
/// `<input type="datetime-local">` expects, in the browser's local time. A task
/// with no due seeds the picker at today 09:00 — the hour the scheduler uses for
/// date-only reminders (`due_at`).
function toLocalInputValue(iso: string | null): string {
  const d = iso ? new Date(iso) : defaultDue();
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/// Seed value for a task with no due date: today at 09:00 local.
function defaultDue(): Date {
  const d = new Date();
  d.setHours(9, 0, 0, 0);
  return d;
}
