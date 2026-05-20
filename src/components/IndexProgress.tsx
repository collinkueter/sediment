import type { IndexProgress as IndexProgressPayload } from "@/lib/tauri";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";

/// Compact background-indexing indicator for the title bar. Subscribes to the
/// Rust core's `index-progress` events and hides itself once done === total.
export function IndexProgress() {
  const [progress, setProgress] = useState<IndexProgressPayload | null>(null);

  useEffect(() => {
    const unlistenP = listen<IndexProgressPayload>("index-progress", (event) => {
      const p = event.payload;
      if (p.total === 0 || p.done >= p.total) {
        // Completed (or nothing to do) — clear after a short beat.
        setTimeout(() => setProgress(null), 800);
        setProgress(p.total === 0 ? null : p);
      } else {
        setProgress(p);
      }
    });
    return () => {
      unlistenP.then((unlisten) => unlisten()).catch(() => {});
    };
  }, []);

  if (!progress || progress.total === 0) return null;

  const pct = Math.round((progress.done / progress.total) * 100);
  const done = progress.done >= progress.total;

  return (
    <span
      data-tauri-drag-region
      className="ml-3 flex items-center gap-1.5 text-zinc-400 dark:text-zinc-500"
      title={progress.current_path || "indexing"}
    >
      <span
        aria-hidden
        className="h-1.5 w-16 overflow-hidden rounded-full bg-zinc-200 dark:bg-zinc-800"
      >
        <span
          className="block h-full rounded-full bg-zinc-500 transition-[width] duration-200 dark:bg-zinc-400"
          style={{ width: `${pct}%` }}
        />
      </span>
      <span className="tabular-nums">
        {done ? "indexed" : `indexing ${progress.done}/${progress.total}`}
      </span>
    </span>
  );
}
