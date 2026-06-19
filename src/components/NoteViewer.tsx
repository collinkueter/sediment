import { EmptyState } from "@/components/EmptyState";
import { MeetingSpeakers } from "@/components/MeetingSpeakers";
import { NotePreview } from "@/components/NotePreview";
import { Segmented } from "@/components/Segmented";
import { Icon } from "@/components/icons";
import { markdownExtensions, oneDark, paperTheme } from "@/lib/codemirror/setup";
import { useFormationStore } from "@/lib/store";
import { useThemeStore } from "@/lib/theme";
import { useUiStore } from "@/lib/ui";
import CodeMirror from "@uiw/react-codemirror";
import { useEffect, useMemo, useState } from "react";

type Mode = "preview" | "source";

/**
 * The note pane. The conversational agent edits notes directly on disk
 * (ADR-0009 §5) — there is no staged-diff review in the editor; the audit log
 * is the backstop.
 *
 * Two modes, Obsidian-style:
 * - **Read** (default) — rendered markdown with clickable `[[wiki-links]]`
 *   that navigate to other notes, headings/lists/code/tables styled, external
 *   links open in the OS browser.
 * - **Source** — the raw CodeMirror editor, for direct hand edits.
 */
export function NoteViewer() {
  const currentNotePath = useFormationStore((s) => s.currentNotePath);

  if (!currentNotePath) {
    return <EmptyEditor />;
  }
  return <Editor />;
}

/** Basename without the `.md` extension — the human-facing note title. */
function noteTitle(path: string): string {
  const base = path.replace(/^.*\//, "").replace(/\.md$/i, "");
  return base || path;
}

/** Map the top-level folder to a human note-type label for the sub line. */
function noteTypeLabel(path: string): string {
  const top = path.includes("/") ? path.slice(0, path.indexOf("/")) : "";
  switch (top) {
    case "People":
      return "Person";
    case "Projects":
      return "Project";
    case "Organizations":
      return "Organization";
    case "Daily Notes":
      return "Daily note";
    case "Meetings":
      return "Meeting";
    default:
      return "Note";
  }
}

/** Coarse relative time from a unix-seconds timestamp ("just now", "5m ago"…). */
function relativeEdited(modifiedSecs: number): string {
  const now = Date.now() / 1000;
  const diff = Math.max(0, now - modifiedSecs);
  if (diff < 45) return "edited just now";
  const mins = Math.round(diff / 60);
  if (mins < 60) return `edited ${mins}m ago`;
  const hours = Math.round(diff / 3600);
  if (hours < 24) return `edited ${hours}h ago`;
  const d = new Date(modifiedSecs * 1000);
  return `edited ${d.toLocaleDateString(undefined, { month: "short", day: "numeric" })}`;
}

function Editor() {
  const currentNotePath = useFormationStore((s) => s.currentNotePath);
  const content = useFormationStore((s) => s.currentNoteContent);
  const isDirty = useFormationStore((s) => s.isDirty);
  const setContent = useFormationStore((s) => s.setContent);
  const save = useFormationStore((s) => s.save);
  const notes = useFormationStore((s) => s.notes);
  const theme = useThemeStore((s) => s.theme);
  const toggleNotePane = useUiStore((s) => s.toggleNotePane);
  const [mode, setMode] = useState<Mode>("preview");

  const path = currentNotePath ?? "";
  const title = useMemo(() => noteTitle(path), [path]);
  const typeLabel = useMemo(() => noteTypeLabel(path), [path]);
  const note = useMemo(() => notes.find((n) => n.relative_path === path), [notes, path]);
  const editedLabel = note ? relativeEdited(note.modified_secs) : "edited just now";
  const openNote = useFormationStore((s) => s.openNote);
  // A Meeting note gets a speaker-reconciliation band (ADR-0017 §6).
  const isMeeting = path.startsWith("Meetings/");

  // Cmd+S / Ctrl+S to save the current note (Source mode only — Read has
  // nothing to save). Cmd+E toggles between modes, matching Obsidian.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if ((e.metaKey || e.ctrlKey) && e.key === "s") {
        e.preventDefault();
        save().catch((err) => console.error("save failed:", err));
      } else if ((e.metaKey || e.ctrlKey) && e.key === "e") {
        e.preventDefault();
        setMode((m) => (m === "preview" ? "source" : "preview"));
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [save]);

  // When the active note changes, default back to Read. Editing is the
  // exception, not the default — most viewing is reading.
  // biome-ignore lint/correctness/useExhaustiveDependencies: currentNotePath is the trigger, not a value read in the effect
  useEffect(() => {
    setMode("preview");
  }, [currentNotePath]);

  return (
    <div className="flex h-full w-full flex-col">
      <header className="flex items-center gap-2 border-b border-line px-4 py-3">
        <button
          type="button"
          aria-label="Hide note"
          title="Hide note (focus the conversation)"
          onClick={toggleNotePane}
          className="-ml-1 flex h-7 w-7 shrink-0 items-center justify-center rounded-md text-muted transition-colors hover:bg-bg-sunk hover:text-ink"
        >
          <Icon.ChevronRight className="h-4 w-4" />
        </button>
        <div className="min-w-0 flex-1">
          <div className="truncate font-serif text-[15.5px] font-semibold text-ink">{title}</div>
          <div className="flex items-center gap-1 text-[11px] text-muted">
            <span className="h-1.5 w-1.5 rounded-full bg-sage" aria-hidden />
            <span className="truncate">
              {typeLabel} · {editedLabel}
            </span>
            {isDirty && (
              <span
                className="ml-0.5 h-1.5 w-1.5 rounded-full bg-gold"
                aria-label="Unsaved changes"
              />
            )}
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-1.5">
          <ModeToggle mode={mode} setMode={setMode} />
          {mode === "source" && (
            <button
              type="button"
              onClick={() => save().catch((e) => console.error("save failed:", e))}
              disabled={!isDirty}
              title="Save (⌘S)"
              className="rounded-md bg-accent px-2.5 py-1 text-xs font-medium text-white transition-opacity hover:opacity-90 disabled:opacity-30"
            >
              Save
            </button>
          )}
        </div>
      </header>
      {isMeeting && mode === "preview" && (
        <MeetingSpeakers notePath={path} onReload={() => openNote(path)} />
      )}
      <div className="min-h-0 flex-1 overflow-auto">
        {mode === "preview" ? (
          <NotePreview source={content} />
        ) : (
          <CodeMirror
            value={content}
            onChange={setContent}
            extensions={markdownExtensions}
            theme={theme === "strata" ? oneDark : paperTheme}
            height="100%"
            basicSetup={{
              lineNumbers: false,
              foldGutter: false,
              highlightActiveLine: false,
              highlightActiveLineGutter: false,
            }}
          />
        )}
      </div>
    </div>
  );
}

function ModeToggle({ mode, setMode }: { mode: Mode; setMode: (m: Mode) => void }) {
  return (
    <div title="⌘E to toggle">
      <Segmented<Mode>
        value={mode}
        onChange={setMode}
        ariaLabel="View mode"
        options={[
          {
            value: "preview",
            label: (
              <>
                <Icon.Eye className="h-3.5 w-3.5" />
                Read
              </>
            ),
          },
          {
            value: "source",
            label: (
              <>
                <Icon.Pencil className="h-3.5 w-3.5" />
                Source
              </>
            ),
          },
        ]}
      />
    </div>
  );
}

function EmptyEditor() {
  return (
    <EmptyState
      icon={Icon.File}
      title="No note open"
      description="Choose a note from the left, follow a [[link]] in the conversation, or press ⌘K to search."
    />
  );
}
