import { useFormationStore } from "@/lib/store";
import type { FormationNote } from "@/lib/tauri";
import { useMemo, useState } from "react";

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

/** Indentation in px for a row at a given tree depth. */
const INDENT = 12;
const BASE_PAD = 8;
/** Width of the folder chevron column — files pad past it so names align. */
const CHEVRON_COL = 16;

export function FileTree() {
  const notes = useFormationStore((s) => s.notes);
  const formationPath = useFormationStore((s) => s.formationPath);
  const tree = useMemo(() => buildTree(notes), [notes]);

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
            {tree.map((node) => (
              <TreeNodeView key={nodeKey(node)} node={node} depth={0} />
            ))}
          </ul>
        )}
      </div>
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
  return (
    <li>
      <button
        type="button"
        onClick={() => setExpanded((e) => !e)}
        aria-expanded={expanded}
        className="flex w-full items-center gap-1 py-1 pr-2 text-left text-xs text-zinc-500 hover:bg-zinc-100 dark:text-zinc-400 dark:hover:bg-zinc-800/50"
        style={{ paddingLeft: depth * INDENT + BASE_PAD }}
        title={node.path}
      >
        <span className="w-3 shrink-0 text-[9px] text-zinc-400 dark:text-zinc-600">
          {expanded ? "▾" : "▸"}
        </span>
        <span className="truncate font-medium">{node.name}</span>
        <span className="ml-auto pl-2 text-[10px] text-zinc-300 dark:text-zinc-600">
          {fileCount}
        </span>
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
  return (
    <li>
      <button
        type="button"
        onClick={() => {
          openNote(node.note.relative_path).catch((e) => console.error("open note failed:", e));
        }}
        className={`block w-full truncate py-1 pr-3 text-left text-xs ${
          isActive
            ? "bg-zinc-200 font-medium text-zinc-900 dark:bg-zinc-800 dark:text-zinc-100"
            : "text-zinc-600 hover:bg-zinc-100 dark:text-zinc-400 dark:hover:bg-zinc-800/50"
        }`}
        style={{ paddingLeft: depth * INDENT + BASE_PAD + CHEVRON_COL }}
        title={node.note.relative_path}
      >
        {displayName(node.name)}
      </button>
    </li>
  );
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
