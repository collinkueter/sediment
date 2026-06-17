import { Icon, initials } from "@/components/icons";
import { useFormationStore, useWorkingSetStore } from "@/lib/store";
import { tauri } from "@/lib/tauri";
import type { ActiveEntity, OpenLoop, OpenTask } from "@/lib/tauri";
import { useUiStore } from "@/lib/ui";

// ── Avatar style by entity type ────────────────────────────────────────────

interface AvatarStyle {
  gradient: string;
  rounded: boolean;
}

function avatarStyleFor(entityType: string): AvatarStyle {
  switch (entityType.toLowerCase()) {
    case "person":
      return {
        gradient: "bg-[linear-gradient(150deg,var(--accent),var(--accent-ink))]",
        rounded: true,
      };
    case "organization":
    case "org":
    case "company":
      return {
        gradient: "bg-[linear-gradient(150deg,var(--sage),#3f5249)]",
        rounded: false,
      };
    case "project":
      return {
        gradient: "bg-[linear-gradient(150deg,var(--gold),#7c5d22)]",
        rounded: false,
      };
    case "meeting":
      return {
        gradient: "bg-[linear-gradient(150deg,var(--sage),#3f5249)]",
        rounded: false,
      };
    default:
      return {
        gradient: "bg-[linear-gradient(150deg,var(--accent),var(--accent-ink))]",
        rounded: true,
      };
  }
}

// ── Short due-date label ────────────────────────────────────────────────────

const DAY_NAMES = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTH_NAMES = [
  "Jan",
  "Feb",
  "Mar",
  "Apr",
  "May",
  "Jun",
  "Jul",
  "Aug",
  "Sep",
  "Oct",
  "Nov",
  "Dec",
];

function formatDue(due: string, today: Date): string {
  // due is YYYY-MM-DD
  const [y, m, d] = due.split("-").map(Number);
  if (y === undefined || m === undefined || d === undefined) return due;
  const dueDate = new Date(y, m - 1, d);
  const todayMidnight = new Date(today.getFullYear(), today.getMonth(), today.getDate());
  const diffMs = dueDate.getTime() - todayMidnight.getTime();
  const diffDays = Math.round(diffMs / 86_400_000);
  if (diffDays >= 0 && diffDays <= 6) {
    // Within a week — show day name
    return DAY_NAMES[dueDate.getDay()] ?? due;
  }
  // Further out — show "Mon DD"
  return `${MONTH_NAMES[dueDate.getMonth()] ?? ""} ${d}`;
}

// ── Visible cap ─────────────────────────────────────────────────────────────

const MAX_ENTITIES = 3;
const MAX_PILLS = 3;

// ── Sub-components ──────────────────────────────────────────────────────────

function EntityChip({ entity }: { entity: ActiveEntity }) {
  const openNote = useFormationStore((s) => s.openNote);
  const { gradient, rounded } = avatarStyleFor(entity.entityType);
  const canOpen = entity.notePath !== null;

  function handleClick() {
    if (entity.notePath) {
      openNote(entity.notePath).catch(() => {});
    }
  }

  return (
    <button
      type="button"
      onClick={handleClick}
      disabled={!canOpen}
      aria-label={`Open note for ${entity.name}`}
      title={entity.notePath ?? entity.name}
      className={[
        "inline-flex items-center gap-1.5 rounded-full border border-line bg-raised px-2.5 py-1",
        "text-[12.5px] text-ink shadow-sm transition-[border-color,transform] duration-150",
        canOpen
          ? "cursor-pointer hover:-translate-y-px hover:border-accent"
          : "cursor-default opacity-60",
      ].join(" ")}
    >
      {/* Avatar */}
      <span
        className={[
          "inline-grid h-[18px] w-[18px] flex-none place-items-center text-[10px] font-bold text-white",
          gradient,
          rounded ? "rounded-full" : "rounded-[5px]",
        ].join(" ")}
        aria-hidden="true"
      >
        {initials(entity.name)}
      </span>
      <span>{entity.name}</span>
    </button>
  );
}

function TaskPill({ task, today }: { task: OpenTask; today: Date }) {
  const toggleReminders = useUiStore((s) => s.toggleReminders);

  return (
    <button
      type="button"
      onClick={toggleReminders}
      aria-label={`Task: ${task.title}${task.due ? `, due ${task.due}` : ""}`}
      className="inline-flex items-center gap-1.5 rounded-lg border border-line bg-raised px-2.5 py-1 text-[12px] text-ink shadow-sm transition-[border-color] hover:border-line-strong"
    >
      {/* Gold lead dot */}
      <span
        className="inline-block h-[7px] w-[7px] flex-none rounded-full bg-gold"
        aria-hidden="true"
      />
      <span className="truncate">{task.title}</span>
      {task.due !== null && task.due !== undefined && (
        <span className="font-mono text-[10.5px] font-medium text-gold">
          {formatDue(task.due, today)}
        </span>
      )}
    </button>
  );
}

function LoopPill({
  loop,
  onDismiss,
}: {
  loop: OpenLoop;
  onDismiss: (id: string) => void;
}) {
  return (
    <span className="group inline-flex items-center gap-1.5 rounded-lg border border-line bg-raised px-2.5 py-1 text-[12px] text-ink shadow-sm">
      {/* Sage lead dot */}
      <span
        className="inline-block h-[7px] w-[7px] flex-none rounded-full bg-sage"
        aria-hidden="true"
      />
      <span className="truncate">{loop.title}</span>
      {/* Dismiss × appears on group-hover */}
      <button
        type="button"
        onClick={() => onDismiss(loop.id)}
        aria-label={`Dismiss open loop: ${loop.title}`}
        className="ml-0.5 hidden rounded p-0.5 text-muted hover:bg-bg-sunk hover:text-ink group-hover:inline-flex"
      >
        <Icon.X className="h-3 w-3" />
      </button>
    </span>
  );
}

// The Self chip — the agent's durable model of the user (ADR-0015 §5). Opens
// `Self.md` so you can see and edit exactly what it knows about you. Distinct from
// the derived entity chips: the Self is authored, always you.
function SelfChip({ summary }: { summary: string }) {
  const openNote = useFormationStore((s) => s.openNote);

  return (
    <button
      type="button"
      onClick={() => {
        openNote("Self.md").catch(() => {});
      }}
      aria-label="Open your Self note — what the agent knows about you"
      title={summary}
      className={[
        "inline-flex items-center gap-1.5 rounded-full border border-line bg-raised px-2.5 py-1",
        "text-[12.5px] font-medium text-ink shadow-sm transition-[border-color,transform] duration-150",
        "cursor-pointer hover:-translate-y-px hover:border-accent",
      ].join(" ")}
    >
      <span
        className="inline-block h-[7px] w-[7px] flex-none rounded-full bg-[var(--accent)]"
        aria-hidden="true"
      />
      <span>You</span>
    </button>
  );
}

// ── Main component ───────────────────────────────────────────────────────────

export function InFocusBar() {
  const workingSet = useWorkingSetStore((s) => s.workingSet);
  const selfSummary = useWorkingSetStore((s) => s.selfSummary);
  const removeOpenLoop = useWorkingSetStore((s) => s.removeOpenLoop);
  const toggleReminders = useUiStore((s) => s.toggleReminders);

  const today = new Date();

  const activeEntities = workingSet?.activeEntities ?? [];
  const openTasks = workingSet?.openTasks ?? [];
  const openLoops = workingSet?.openLoops ?? [];
  const hasContent = activeEntities.length > 0 || openTasks.length > 0 || openLoops.length > 0;
  // The bar shows if the agent knows you (Self) OR there's recent activity.
  if (!selfSummary && !hasContent) return null;

  // Visible entity chips (capped)
  const visibleEntities = activeEntities.slice(0, MAX_ENTITIES);

  // Visible pills (tasks first, then loops, capped combined)
  const allPills: Array<{ kind: "task"; item: OpenTask } | { kind: "loop"; item: OpenLoop }> = [
    ...openTasks.map((t) => ({ kind: "task" as const, item: t })),
    ...openLoops.map((l) => ({ kind: "loop" as const, item: l })),
  ];
  const visiblePills = allPills.slice(0, MAX_PILLS);
  const hiddenCount =
    activeEntities.length - visibleEntities.length + (allPills.length - visiblePills.length);

  function handleDismissLoop(id: string) {
    removeOpenLoop(id);
    tauri.dismissOpenLoop(id).catch(() => {
      // Optimistic removal — silent failure. Loop reappears on next refresh
      // if the backend didn't persist the dismissal.
    });
  }

  const hasPills = visiblePills.length > 0;

  return (
    <div className="flex flex-wrap items-center gap-2 border-b border-line bg-surface px-5 py-2.5">
      {/* "In focus" label with live dot */}
      <span className="inline-flex items-center gap-1.5 text-[10px] font-bold uppercase tracking-[.08em] text-ink-soft">
        <span
          className="inline-block h-[6px] w-[6px] rounded-full bg-sage"
          style={{ animation: "infocus-pulse 2.4s ease-in-out infinite" }}
          aria-hidden="true"
        />
        In focus
      </span>

      {/* Self chip — what the agent knows about you (ADR-0015 §5) */}
      {selfSummary && <SelfChip summary={selfSummary} />}
      {selfSummary && hasContent && (
        <span className="h-[18px] w-px flex-none bg-line-strong" aria-hidden="true" />
      )}

      {/* Entity chips */}
      {visibleEntities.map((e) => (
        <EntityChip key={`${e.entityType}:${e.name}`} entity={e} />
      ))}

      {/* Vertical separator between chips and pills */}
      {hasPills && activeEntities.length > 0 && (
        <span className="h-[18px] w-px flex-none bg-line-strong" aria-hidden="true" />
      )}

      {/* Task / loop pills */}
      {visiblePills.map((entry) =>
        entry.kind === "task" ? (
          <TaskPill key={`task:${entry.item.title}`} task={entry.item} today={today} />
        ) : (
          <LoopPill key={`loop:${entry.item.id}`} loop={entry.item} onDismiss={handleDismissLoop} />
        ),
      )}

      {/* "N more" affordance */}
      {hiddenCount > 0 && (
        <button
          type="button"
          onClick={toggleReminders}
          aria-label={`Show ${hiddenCount} more items`}
          className="ml-auto inline-flex items-center gap-1 text-[12px] text-muted hover:text-ink-soft"
        >
          {hiddenCount} more
          <Icon.ChevronDown className="h-3.5 w-3.5" />
        </button>
      )}
    </div>
  );
}
