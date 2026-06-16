import { Icon } from "@/components/icons";
import { useFormationStore } from "@/lib/store";
import type { FormationNote } from "@/lib/tauri";
import { useUiStore } from "@/lib/ui";
import { useMemo, useState } from "react";
import type { ComponentType, SVGProps } from "react";

interface FolderNode {
  type: "folder";
  name: string;
  /** Formation-relative folder path, e.g. "People" or "Projects/Acme". */
  path: string;
  children: TreeNode[];
}

interface FileNode {
  type: "file";
  name: string;
  note: FormationNote;
}

type TreeNode = FolderNode | FileNode;

/** Group a flat note list into a nested folder tree by splitting `relative_path`. */
function buildTree(notes: FormationNote[]): TreeNode[] {
  const root: FolderNode = { type: "folder", name: "", path: "", children: [] };
  for (const note of notes) {
    const segments = note.relative_path.split(/[/\\]/).filter((s) => s.length > 0);
    const fileName = segments.pop();
    if (fileName === undefined) continue;
    let cursor = root;
    let prefix = "";
    for (const name of segments) {
      prefix = prefix ? `${prefix}/${name}` : name;
      let next = cursor.children.find(
        (c): c is FolderNode => c.type === "folder" && c.name === name,
      );
      if (!next) {
        next = { type: "folder", name, path: prefix, children: [] };
        cursor.children.push(next);
      }
      cursor = next;
    }
    cursor.children.push({ type: "file", name: fileName, note });
  }
  sortFolder(root);
  return root.children;
}

/** Sort each folder's children: folders first, then files, alphabetical within each. */
function sortFolder(folder: FolderNode): void {
  folder.children.sort((a, b) => {
    if (a.type !== b.type) return a.type === "folder" ? -1 : 1;
    return a.name.localeCompare(b.name);
  });
  for (const child of folder.children) {
    if (child.type === "folder") sortFolder(child);
  }
}

function nodeKey(node: TreeNode): string {
  return node.type === "folder" ? `dir:${node.path}` : node.note.relative_path;
}

/** Intentional section order — the conventional formation folders lead. */
const SECTION_ORDER = [
  "People",
  "Projects",
  "Organizations",
  "Daily Notes",
  "Weekly Notes",
  "Templates",
];
function sectionRank(name: string): number {
  const i = SECTION_ORDER.indexOf(name);
  return i === -1 ? SECTION_ORDER.length : i;
}

/** Total files nested anywhere under a folder. */
function countFiles(folder: FolderNode): number {
  let total = 0;
  for (const child of folder.children) {
    total += child.type === "folder" ? countFiles(child) : 1;
  }
  return total;
}

/** Drop a trailing `.md` so the tree reads like note titles, not filenames. */
function displayName(name: string): string {
  return name.endsWith(".md") ? name.slice(0, -3) : name;
}

function basename(p: string): string {
  const parts = p.split(/[/\\]/);
  return parts[parts.length - 1] || p;
}

/**
 * Derive the best icon for a top-level folder by its conventional name.
 * Nested folders and files fall back to Folder / File respectively.
 */
function folderIcon(name: string, depth: number): ComponentType<SVGProps<SVGSVGElement>> {
  if (depth > 0) return Icon.Folder;
  const lower = name.toLowerCase();
  if (lower === "people" || lower === "persons") return Icon.Person;
  if (lower === "organizations" || lower === "orgs") return Icon.Building;
  if (lower === "daily notes" || lower === "daily" || lower === "journal") return Icon.Calendar;
  return Icon.Folder;
}

export function FileTree() {
  const notes = useFormationStore((s) => s.notes);
  const formationPath = useFormationStore((s) => s.formationPath);
  const pick = useFormationStore((s) => s.pick);
  const openPalette = useUiStore((s) => s.openPalette);
  const tree = useMemo(() => buildTree(notes), [notes]);

  // Split top-level folders (the named sections) from loose root files; order
  // the sections intentionally rather than alphabetically.
  const { folders, rootFiles } = useMemo(() => {
    const folders = tree
      .filter((n): n is FolderNode => n.type === "folder")
      .sort((a, b) => sectionRank(a.name) - sectionRank(b.name) || a.name.localeCompare(b.name));
    const rootFiles = tree.filter((n): n is FileNode => n.type === "file");
    return { folders, rootFiles };
  }, [tree]);

  const formationName = formationPath ? basename(formationPath) : "No formation";

  return (
    <aside className="flex h-full w-full flex-col bg-surface">
      {/* Formation-switch header button */}
      <div className="px-3.5 pt-3.5 pb-2.5">
        <button
          type="button"
          aria-label="Switch formation"
          title="Open a different formation"
          onClick={() => void pick()}
          className="flex w-full items-center gap-2.5 rounded-[9px] border border-line bg-raised px-2.5 py-2
            shadow-sm transition-colors hover:border-line-strong"
        >
          {/* Gradient layers glyph */}
          <span
            className="flex h-[26px] w-[26px] shrink-0 items-center justify-center rounded-[7px] text-white"
            style={{ background: "linear-gradient(150deg, var(--accent), var(--accent-ink))" }}
            aria-hidden="true"
          >
            <Icon.Layers className="h-[15px] w-[15px]" />
          </span>

          {/* Name + note count */}
          <span className="min-w-0 flex-1 text-left">
            <span className="block truncate text-[13px] font-semibold leading-tight text-ink">
              {formationName}
            </span>
            <span className="block text-[11px] leading-tight text-muted">
              {notes.length} {notes.length === 1 ? "note" : "notes"}
            </span>
          </span>

          {/* Up/down chevron */}
          <Icon.ChevronUpDown className="h-[14px] w-[14px] shrink-0 text-faint" />
        </button>
      </div>

      {/* Search / ⌘K trigger */}
      <div className="px-3.5 pb-2">
        <button
          type="button"
          aria-label="Search notes and entities (⌘K)"
          onClick={openPalette}
          className="flex w-full items-center gap-2 rounded-[8px] bg-bg-sunk px-2.5 py-[6px] text-muted
            transition-colors hover:text-ink-soft"
        >
          <Icon.Search className="h-[14px] w-[14px] shrink-0" />
          <span className="min-w-0 flex-1 text-left text-[12.5px]">
            Search notes &amp; entities
          </span>
          <kbd
            className="shrink-0 rounded border border-line-strong font-mono text-[10px] text-faint"
            style={{ padding: "1px 4px" }}
          >
            ⌘K
          </kbd>
        </button>
      </div>

      {/* Tree */}
      <nav
        className="min-h-0 flex-1 overflow-y-auto px-2 pb-3.5 pt-0.5"
        aria-label="Formation notes"
      >
        {notes.length === 0 ? (
          <p className="px-3 py-4 text-[12.5px] text-muted">
            No markdown files in this folder yet.
          </p>
        ) : (
          <ul>
            {rootFiles.length > 0 && (
              <li>
                <div className="flex items-center gap-1.5 px-2 pt-[9px] pb-1">
                  <span className="min-w-0 flex-1 truncate text-[10px] font-bold uppercase tracking-[0.08em] text-faint">
                    Pinned
                  </span>
                  <span className="ml-auto shrink-0 text-[10px] font-semibold text-faint">
                    {rootFiles.length}
                  </span>
                </div>
                <ul>
                  {rootFiles.map((node) => (
                    <FileRow key={nodeKey(node)} node={node} depth={1} />
                  ))}
                </ul>
              </li>
            )}
            {folders.map((node) => (
              <TreeNodeView key={nodeKey(node)} node={node} depth={0} />
            ))}
          </ul>
        )}
      </nav>
    </aside>
  );
}

function TreeNodeView({ node, depth }: { node: TreeNode; depth: number }) {
  return node.type === "folder" ? (
    <FolderRow node={node} depth={depth} />
  ) : (
    <FileRow node={node} depth={depth} />
  );
}

function FolderRow({ node, depth }: { node: FolderNode; depth: number }) {
  const [expanded, setExpanded] = useState(true);
  const fileCount = countFiles(node);
  const isTopLevel = depth === 0;

  if (isTopLevel) {
    // Render as a section label (UPPERCASE, text-faint, tracking-wide)
    return (
      <li>
        <button
          type="button"
          onClick={() => setExpanded((e) => !e)}
          aria-expanded={expanded}
          aria-label={`${node.name} section, ${fileCount} notes`}
          className="flex w-full items-center gap-1.5 px-2 pb-1 pt-[9px] text-left"
        >
          <span className="min-w-0 flex-1 truncate text-[10px] font-bold uppercase tracking-[0.08em] text-faint">
            {node.name}
          </span>
          <span className="ml-auto shrink-0 text-[10px] font-semibold text-faint">{fileCount}</span>
        </button>
        {expanded && (
          <ul>
            {node.children.map((child) => (
              <TreeNodeView key={nodeKey(child)} node={child} depth={depth + 1} />
            ))}
          </ul>
        )}
      </li>
    );
  }

  // Nested folder: chevron disclosure row
  const FolderIconComp = folderIcon(node.name, depth);
  return (
    <li>
      <button
        type="button"
        onClick={() => setExpanded((e) => !e)}
        aria-expanded={expanded}
        aria-label={`${node.name} folder`}
        className="relative flex w-full items-center gap-2 rounded-[7px] px-2 py-[5px] text-left
          text-[13px] font-medium text-muted transition-colors hover:bg-bg-sunk"
        style={{ paddingLeft: `${(depth - 1) * 16 + 8}px` }}
      >
        <span className="flex shrink-0 items-center gap-1 text-faint">
          {expanded ? (
            <Icon.ChevronDown className="h-[13px] w-[13px]" />
          ) : (
            <Icon.ChevronRight className="h-[13px] w-[13px]" />
          )}
        </span>
        <FolderIconComp className="h-[15px] w-[15px] shrink-0 opacity-70" />
        <span className="min-w-0 flex-1 truncate">{node.name}</span>
        <span className="ml-auto shrink-0 text-[10px] text-faint">{fileCount}</span>
      </button>
      {expanded && (
        <ul>
          {node.children.map((child) => (
            <TreeNodeView key={nodeKey(child)} node={child} depth={depth + 1} />
          ))}
        </ul>
      )}
    </li>
  );
}

function FileRow({ node, depth }: { node: FileNode; depth: number }) {
  const currentPath = useFormationStore((s) => s.currentNotePath);
  const openNote = useFormationStore((s) => s.openNote);
  const isActive = node.note.relative_path === currentPath;

  // Derive icon from parent folder name (depth 1 = top-level folder child)
  const FileIconComp = Icon.File;

  return (
    <li>
      <button
        type="button"
        onClick={() => {
          void openNote(node.note.relative_path);
        }}
        title={node.note.relative_path}
        aria-current={isActive ? "page" : undefined}
        className={[
          "relative flex w-full items-center gap-2 rounded-[7px] px-2 py-[5px] text-left",
          "text-[13px] transition-colors",
          isActive
            ? "bg-accent-tint font-semibold text-accent-ink"
            : "text-ink-soft hover:bg-bg-sunk",
        ].join(" ")}
        style={{ paddingLeft: `${Math.max(depth - 1, 0) * 16 + 24}px` }}
      >
        {/* Active left bar */}
        {isActive && (
          <span
            className="absolute bottom-[6px] left-0 top-[6px] w-[3px] rounded-[3px] bg-accent"
            aria-hidden="true"
          />
        )}
        <FileIconComp
          className={[
            "h-[15px] w-[15px] shrink-0",
            isActive ? "text-accent opacity-100" : "opacity-70",
          ].join(" ")}
        />
        <span className="min-w-0 flex-1 truncate">{displayName(node.name)}</span>
      </button>
    </li>
  );
}
