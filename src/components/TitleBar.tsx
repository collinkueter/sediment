import { IndexProgress } from "@/components/IndexProgress";
import { Segmented } from "@/components/Segmented";
import { Icon } from "@/components/icons";
import { isWindows } from "@/lib/platform";
import { useFormationStore } from "@/lib/store";
import { type Theme, useThemeStore } from "@/lib/theme";
import { useUiStore } from "@/lib/ui";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { type ReactNode, useEffect, useState } from "react";

/**
 * The custom window title bar. Draggable, with the wordmark + a strata mark, a
 * breadcrumb of where you are, and the right-hand controls: theme toggle,
 * search (⌘K), tasks, settings.
 *
 * The window is frameless on both desktop platforms, so the chrome adapts:
 * macOS overlays the native traffic lights, so the bar reserves a 78px safe
 * area on the left; Windows is fully undecorated, so the bar draws its own
 * minimize / maximize / close controls flush to the right edge (see
 * `lib/platform.ts` and `tauri.windows.conf.json`).
 */
export function TitleBar({ openTaskCount }: { openTaskCount: number }) {
  const formationPath = useFormationStore((s) => s.formationPath);
  const currentNotePath = useFormationStore((s) => s.currentNotePath);
  const togglePalette = useUiStore((s) => s.togglePalette);
  const toggleReminders = useUiStore((s) => s.toggleReminders);
  const remindersActive = useUiStore((s) => s.view === "reminders");
  const openSettings = useUiStore((s) => s.openSettings);

  // macOS reserves the traffic-light safe area on the left; Windows draws its
  // own controls on the right, so the bar runs flush to the right edge there.
  const edgePadding = isWindows ? "pr-0 pl-3" : "pr-3 pl-[78px]";

  return (
    <header
      data-tauri-drag-region
      className={`flex h-11 flex-none items-center gap-3 border-b border-line bg-surface select-none ${edgePadding}`}
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
        <IconButton label="Reminders" onClick={toggleReminders} active={remindersActive}>
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

      {isWindows && <WindowControls />}
    </header>
  );
}

/**
 * Windows window controls — minimize, maximize/restore, close. Rendered only on
 * Windows, where the window is undecorated (`tauri.windows.conf.json`) and there
 * is no native caption. Buttons are full-height and flush to the top-right
 * corner, matching the platform convention; the close button reddens on hover.
 *
 * The maximize/restore glyph tracks the real window state via `onResized`, so
 * snapping or double-click-to-maximize keeps the icon honest.
 */
function WindowControls() {
  const appWindow = getCurrentWindow();
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    appWindow
      .isMaximized()
      .then(setMaximized)
      .catch(() => {});
    appWindow
      .onResized(() => {
        appWindow
          .isMaximized()
          .then(setMaximized)
          .catch(() => {});
      })
      .then((u) => {
        unlisten = u;
      })
      .catch(() => {});
    return () => unlisten?.();
  }, [appWindow]);

  return (
    <div className="flex h-11 items-stretch self-stretch">
      <WindowButton label="Minimize" onClick={() => appWindow.minimize()}>
        <svg viewBox="0 0 12 12" className="h-3 w-3" aria-hidden="true">
          <path d="M2 6h8" stroke="currentColor" strokeWidth="1" />
        </svg>
      </WindowButton>
      <WindowButton
        label={maximized ? "Restore" : "Maximize"}
        onClick={() => appWindow.toggleMaximize()}
      >
        {maximized ? (
          <svg viewBox="0 0 12 12" className="h-3 w-3" fill="none" aria-hidden="true">
            <path d="M3.5 3.5V2.5h6v6h-1" stroke="currentColor" strokeWidth="1" />
            <rect x="2.5" y="3.5" width="6" height="6" stroke="currentColor" strokeWidth="1" />
          </svg>
        ) : (
          <svg viewBox="0 0 12 12" className="h-3 w-3" fill="none" aria-hidden="true">
            <rect x="2.5" y="2.5" width="7" height="7" stroke="currentColor" strokeWidth="1" />
          </svg>
        )}
      </WindowButton>
      <WindowButton label="Close" onClick={() => appWindow.close()} danger>
        <svg viewBox="0 0 12 12" className="h-3 w-3" aria-hidden="true">
          <path d="M2.5 2.5l7 7M9.5 2.5l-7 7" stroke="currentColor" strokeWidth="1" />
        </svg>
      </WindowButton>
    </div>
  );
}

function WindowButton({
  label,
  onClick,
  danger,
  children,
}: {
  label: string;
  onClick: () => void;
  danger?: boolean;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      title={label}
      className={`grid w-[46px] place-items-center text-muted transition-colors ${
        danger ? "hover:bg-red-600 hover:text-white" : "hover:bg-bg-sunk hover:text-ink"
      }`}
    >
      {children}
    </button>
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
  active = false,
  children,
}: {
  label: string;
  onClick: () => void;
  active?: boolean;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      aria-pressed={active}
      title={label}
      className={`relative grid h-[30px] w-[30px] place-items-center rounded-lg transition-colors ${
        active ? "bg-accent-tint text-accent-ink" : "text-muted hover:bg-bg-sunk hover:text-ink"
      }`}
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
