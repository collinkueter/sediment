import { Icon } from "@/components/icons";
import { useFormationStore, useWorkingSetStore } from "@/lib/store";
import { useThemeStore } from "@/lib/theme";
import { useUiStore } from "@/lib/ui";
import { useEffect, useMemo, useRef, useState } from "react";

/**
 * ⌘K command palette over notes & entities, plus quick actions. A centered
 * panel over a dim backdrop with a search input, a grouped "Jump to" list, and
 * an "Actions" group. Arrow keys move the selection, Enter activates, Escape
 * closes.
 */

const MAX_RESULTS = 8;

/** The basename of a note path without its `.md` extension. */
function basename(path: string): string {
  const file = path.split("/").pop() ?? path;
  return file.replace(/\.md$/i, "");
}

/** True when the note path looks like a dated daily note (YYYY-MM-DD). */
function isDaily(path: string): boolean {
  return /\d{4}-\d{2}-\d{2}/.test(basename(path));
}

type Item =
  | { kind: "note"; key: string; title: string; subtitle: string; icon: "file" | "calendar" }
  | {
      kind: "entity";
      key: string;
      title: string;
      subtitle: string;
      icon: "person" | "building";
      notePath: string | null;
    }
  | { kind: "action"; key: string; title: string; subtitle: string; keycap?: string };

export function CommandPalette() {
  const open = useUiStore((s) => s.paletteOpen);
  const close = useUiStore((s) => s.closePalette);
  const openSettings = useUiStore((s) => s.openSettings);
  const notes = useFormationStore((s) => s.notes);
  const openNote = useFormationStore((s) => s.openNote);
  const entities = useWorkingSetStore((s) => s.workingSet)?.activeEntities;

  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  // Reset the query/selection and focus the input each time the palette opens.
  useEffect(() => {
    if (open) {
      setQuery("");
      setSelected(0);
      inputRef.current?.focus();
    }
  }, [open]);

  const noteItems = useMemo<Item[]>(() => {
    const q = query.trim().toLowerCase();
    return notes
      .filter((n) => {
        if (!q) return true;
        return (
          n.relative_path.toLowerCase().includes(q) ||
          basename(n.relative_path).toLowerCase().includes(q)
        );
      })
      .map<Item>((n) => ({
        kind: "note",
        key: n.relative_path,
        title: basename(n.relative_path),
        subtitle: n.relative_path,
        icon: isDaily(n.relative_path) ? "calendar" : "file",
      }));
  }, [notes, query]);

  const entityItems = useMemo<Item[]>(() => {
    const q = query.trim().toLowerCase();
    return (entities ?? [])
      .filter((e) => (q ? e.name.toLowerCase().includes(q) : true))
      .map<Item>((e) => {
        const isOrg = /org|company|building|institution/i.test(e.entityType);
        return {
          kind: "entity",
          key: `entity:${e.name}`,
          title: e.name,
          subtitle: `${e.entityType} · ${e.notePath ?? "no note yet"}`,
          icon: isOrg ? "building" : "person",
          notePath: e.notePath,
        };
      });
  }, [entities, query]);

  const jumpItems = useMemo(
    () => [...entityItems, ...noteItems].slice(0, MAX_RESULTS),
    [entityItems, noteItems],
  );

  const actionItems = useMemo<Item[]>(() => {
    const q = query.trim().toLowerCase();
    const all: Item[] = [
      {
        kind: "action",
        key: "action:settings",
        title: "Open Settings",
        subtitle: "Engine, appearance, formation",
        keycap: "⌘,",
      },
      {
        kind: "action",
        key: "action:theme",
        title: "Toggle theme",
        subtitle: "Paper for daylight, Strata for night",
      },
    ];
    return all.filter((a) => (q ? a.title.toLowerCase().includes(q) : true));
  }, [query]);

  // Flattened, navigable list across both visible groups.
  const flat = useMemo(() => [...jumpItems, ...actionItems], [jumpItems, actionItems]);

  // Keep the selected index within bounds as results change.
  useEffect(() => {
    setSelected((s) => (flat.length === 0 ? 0 : Math.min(s, flat.length - 1)));
  }, [flat.length]);

  if (!open) return null;

  function activate(item: Item) {
    if (item.kind === "note") {
      void openNote(item.key);
      close();
    } else if (item.kind === "entity") {
      if (item.notePath) {
        void openNote(item.notePath);
        close();
      }
    } else if (item.key === "action:settings") {
      openSettings();
    } else if (item.key === "action:theme") {
      useThemeStore.getState().toggle();
      close();
    }
  }

  function onKeyDown(e: React.KeyboardEvent) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setSelected((s) => (flat.length === 0 ? 0 : (s + 1) % flat.length));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSelected((s) => (flat.length === 0 ? 0 : (s - 1 + flat.length) % flat.length));
    } else if (e.key === "Enter") {
      e.preventDefault();
      const item = flat[selected];
      if (item) activate(item);
    } else if (e.key === "Escape") {
      e.preventDefault();
      close();
    }
  }

  return (
    <>
      <button
        type="button"
        aria-label="Close"
        className="fixed inset-0 z-[300] cursor-default bg-black/30"
        onClick={close}
      />
      <div className="fixed top-20 left-1/2 z-[310] w-[min(620px,92vw)] -translate-x-1/2 overflow-hidden rounded-2xl border border-line-strong bg-raised shadow-2xl">
        <div className="flex items-center gap-3 border-line border-b px-[18px] py-[15px]">
          <Icon.Search className="h-[18px] w-[18px] shrink-0 text-muted" />
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={onKeyDown}
            placeholder="Search notes, entities, or type a command…"
            className="w-full bg-transparent text-[16px] text-ink outline-none placeholder:text-faint"
          />
          <span className="rounded-[5px] border border-line-strong px-1.5 py-0.5 font-mono text-[10.5px] text-faint">
            esc
          </span>
        </div>

        <div className="max-h-[344px] overflow-y-auto p-[7px]">
          {jumpItems.length > 0 && (
            <>
              <div className="px-3 pt-[11px] pb-[5px] font-bold text-[10px] text-faint uppercase tracking-[0.07em]">
                Jump to
              </div>
              {jumpItems.map((item, i) => (
                <ResultRow
                  key={item.key}
                  item={item}
                  selected={selected === i}
                  onActivate={() => activate(item)}
                  onHover={() => setSelected(i)}
                />
              ))}
            </>
          )}

          {actionItems.length > 0 && (
            <>
              <div className="px-3 pt-[11px] pb-[5px] font-bold text-[10px] text-faint uppercase tracking-[0.07em]">
                Actions
              </div>
              {actionItems.map((item, i) => (
                <ResultRow
                  key={item.key}
                  item={item}
                  selected={selected === jumpItems.length + i}
                  onActivate={() => activate(item)}
                  onHover={() => setSelected(jumpItems.length + i)}
                />
              ))}
            </>
          )}

          {flat.length === 0 && (
            <p className="px-3 py-6 text-center text-muted text-sm">No matches.</p>
          )}
        </div>
      </div>
    </>
  );
}

function ResultRow({
  item,
  selected,
  onActivate,
  onHover,
}: {
  item: Item;
  selected: boolean;
  onActivate: () => void;
  onHover: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onActivate}
      onMouseEnter={onHover}
      className={`flex w-full items-center gap-3 rounded-[9px] px-3 py-[9px] text-left ${
        selected ? "bg-accent-tint" : "hover:bg-accent-tint"
      }`}
    >
      <span className="grid h-7 w-7 shrink-0 place-items-center rounded-lg bg-accent text-white">
        <RowIcon item={item} />
      </span>
      <span className="min-w-0 flex-1">
        <span className="block font-medium text-[13.5px] text-ink">{item.title}</span>
        <span className="block truncate font-mono text-[11.5px] text-muted">{item.subtitle}</span>
      </span>
      {selected && item.kind !== "action" && (
        <span className="font-mono text-[10.5px] text-faint">↵</span>
      )}
      {item.kind === "action" && item.keycap && (
        <span className="font-mono text-[10.5px] text-faint">{item.keycap}</span>
      )}
    </button>
  );
}

function RowIcon({ item }: { item: Item }) {
  if (item.kind === "note") {
    return item.icon === "calendar" ? (
      <Icon.Calendar className="h-[15px] w-[15px]" />
    ) : (
      <Icon.File className="h-[15px] w-[15px]" />
    );
  }
  if (item.kind === "entity") {
    return item.icon === "building" ? (
      <Icon.Building className="h-[15px] w-[15px]" />
    ) : (
      <Icon.Person className="h-[15px] w-[15px]" />
    );
  }
  return <Icon.Settings className="h-[15px] w-[15px]" />;
}
