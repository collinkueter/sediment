import { IndexProgress } from "@/components/IndexProgress";
import { Segmented } from "@/components/Segmented";
import { Icon } from "@/components/icons";
import { useFormationStore } from "@/lib/store";
import { type Theme, useThemeStore } from "@/lib/theme";
import { useUiStore } from "@/lib/ui";
import type { ReactNode } from "react";

/**
 * The custom window title bar (macOS overlay style). Draggable, with a
 * traffic-light safe area on the left, the wordmark + a strata mark, a
 * breadcrumb of where you are, and the right-hand controls: theme toggle,
 * search (⌘K), tasks, settings.
 */
export function TitleBar({ openTaskCount }: { openTaskCount: number }) {
  const formationPath = useFormationStore((s) => s.formationPath);
  const currentNotePath = useFormationStore((s) => s.currentNotePath);
  const togglePalette = useUiStore((s) => s.togglePalette);
  const toggleReminders = useUiStore((s) => s.toggleReminders);
  const openSettings = useUiStore((s) => s.openSettings);

  return (
    <header
      data-tauri-drag-region
      className="flex h-11 flex-none items-center gap-3 border-b border-line bg-surface pr-3 pl-[78px] select-none"
    >
      <div className="flex items-baseline gap-2" data-tauri-drag-region>
        <StrataMark />
        <span className="font-serif text-[16px] font-semibold tracking-tight text-ink">
          Sediment
        </span>
      </div>

      {formationPath && (
        <div
          data-tauri-drag-region
          className="flex min-w-0 items-center gap-1.5 text-[12.5px] text-muted"
        >
          <span className="font-medium text-ink-soft">{basename(formationPath)}</span>
          {currentNotePath && (
            <>
              <Icon.ChevronRight className="h-3 w-3 shrink-0 opacity-50" />
              <span className="truncate font-medium text-ink-soft">
                {noteTitle(currentNotePath)}
              </span>
            </>
          )}
        </div>
      )}

      <IndexProgress />

      <div className="ml-auto flex items-center gap-1">
        <ThemeToggle />
        <IconButton label="Search (⌘K)" onClick={togglePalette}>
          <Icon.Search className="h-[17px] w-[17px]" />
        </IconButton>
        <IconButton label="Tasks & reminders" onClick={toggleReminders}>
          <Icon.Bell className="h-[17px] w-[17px]" />
          {openTaskCount > 0 && (
            <span className="absolute -top-0.5 -right-0.5 grid h-[15px] min-w-[15px] place-items-center rounded-full border-[1.5px] border-surface bg-accent px-1 text-[9.5px] font-bold text-white">
              {openTaskCount}
            </span>
          )}
        </IconButton>
        <IconButton label="Settings" onClick={openSettings}>
          <Icon.Settings className="h-[17px] w-[17px]" />
        </IconButton>
      </div>
    </header>
  );
}

function ThemeToggle() {
  const theme = useThemeStore((s) => s.theme);
  const setTheme = useThemeStore((s) => s.setTheme);
  return (
    <div className="mr-1.5">
      <Segmented<Theme>
        value={theme}
        onChange={setTheme}
        ariaLabel="Theme"
        options={[
          { value: "paper", label: "Paper" },
          { value: "strata", label: "Strata" },
        ]}
      />
    </div>
  );
}

function IconButton({
  label,
  onClick,
  children,
}: {
  label: string;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      title={label}
      className="relative grid h-[30px] w-[30px] place-items-center rounded-lg text-muted transition-colors hover:bg-bg-sunk hover:text-ink"
    >
      {children}
    </button>
  );
}

/** Three stacked strata bars — the brand mark. */
function StrataMark() {
  return (
    <span className="flex flex-col gap-[1.5px]" aria-hidden>
      <span className="block h-[2px] w-[13px] rounded-sm bg-accent opacity-45" />
      <span className="block h-[2px] w-[16px] rounded-sm bg-accent opacity-70" />
      <span className="block h-[2px] w-[11px] rounded-sm bg-accent" />
    </span>
  );
}

function basename(p: string): string {
  const parts = p.split(/[/\\]/);
  return parts[parts.length - 1] || p;
}

function noteTitle(p: string): string {
  const name = p.split(/[/\\]/).pop() ?? p;
  return name.endsWith(".md") ? name.slice(0, -3) : name;
}
