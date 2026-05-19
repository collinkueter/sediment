import { useFormationStore } from "@/lib/store";

export function FileTree() {
  const notes = useFormationStore((s) => s.notes);
  const currentPath = useFormationStore((s) => s.currentNotePath);
  const openNote = useFormationStore((s) => s.openNote);
  const formationPath = useFormationStore((s) => s.formationPath);

  return (
    <aside className="flex h-full w-64 flex-col border-r border-zinc-200 bg-zinc-50/50 dark:border-zinc-800 dark:bg-zinc-900/50">
      <header className="border-b border-zinc-200 px-3 py-2 dark:border-zinc-800">
        <div className="truncate text-xs font-medium text-zinc-500 dark:text-zinc-400">
          {formationPath ? basename(formationPath) : "no formation"}
        </div>
        <div className="text-[10px] text-zinc-400 dark:text-zinc-600">
          {notes.length} note{notes.length === 1 ? "" : "s"}
        </div>
      </header>
      <div className="min-h-0 flex-1 overflow-auto py-1">
        {notes.length === 0 ? (
          <div className="px-3 py-4 text-xs text-zinc-400 dark:text-zinc-500">
            No markdown files in this folder yet.
          </div>
        ) : (
          <ul>
            {notes.map((n) => {
              const isActive = n.relative_path === currentPath;
              return (
                <li key={n.relative_path}>
                  <button
                    type="button"
                    onClick={() => {
                      openNote(n.relative_path).catch((e) => console.error("open note failed:", e));
                    }}
                    className={`block w-full truncate px-3 py-1 text-left text-xs ${
                      isActive
                        ? "bg-zinc-200 font-medium text-zinc-900 dark:bg-zinc-800 dark:text-zinc-100"
                        : "text-zinc-600 hover:bg-zinc-100 dark:text-zinc-400 dark:hover:bg-zinc-800/50"
                    }`}
                    title={n.relative_path}
                  >
                    {n.relative_path}
                  </button>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </aside>
  );
}

function basename(p: string): string {
  const parts = p.split(/[/\\]/);
  return parts[parts.length - 1] || p;
}
