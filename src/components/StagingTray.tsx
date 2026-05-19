import { useUiStore } from "@/lib/store";

export function StagingTray() {
  const open = useUiStore((s) => s.stagingTrayOpen);
  const toggle = useUiStore((s) => s.toggleStagingTray);

  return (
    <div className="border-t border-zinc-200 bg-zinc-50 dark:border-zinc-800 dark:bg-zinc-900">
      <button
        type="button"
        onClick={toggle}
        className="flex w-full items-center justify-between px-4 py-1.5 text-xs text-zinc-500 hover:bg-zinc-100 dark:text-zinc-400 dark:hover:bg-zinc-800"
      >
        <span>
          Staged changes <span className="ml-1 text-zinc-400 dark:text-zinc-600">— none yet</span>
        </span>
        <span aria-hidden>{open ? "▾" : "▸"}</span>
      </button>
      {open && (
        <div className="px-4 py-3 text-xs text-zinc-400 dark:text-zinc-500">
          When Sediment extracts facts from your chat, they appear here for review before they hit
          your formation. Empty until M3 wires up extraction.
        </div>
      )}
    </div>
  );
}
