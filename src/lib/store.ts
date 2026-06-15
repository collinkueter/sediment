import {
  type AuditEntry,
  type DailyNoteAppendedPayload,
  type FormationNote,
  type Task,
  type UndoTaskCompletionResult,
  type WorkingSet,
  tauri,
} from "@/lib/tauri";
import { listen } from "@tauri-apps/api/event";
import { create } from "zustand";

/** One line in a turn's inline tool-activity trail (ADR-0009 §5). */
export interface ToolActivity {
  /** The tool name (e.g. `Edit`, `mcp__sediment__record_fact`). */
  tool: string;
  /** A short human phrase describing the call. */
  summary: string;
}

/** Why a turn failed, plus the message needed to retry it. */
export interface ChatFailure {
  /** Human-readable error from the failed `chat_turn` command. */
  error: string;
  /** The user message to re-send on retry. */
  body: string;
}

/**
 * One conversational turn (ADR-0009): the user's message, the agent's streamed
 * reply, and the inline trail of tools the agent used to produce it.
 */
export interface ChatTurn {
  id: string;
  createdAt: number;
  /** The user's message that opened the turn. */
  userMessage: string;
  /** The agent's reply, accumulated from streamed `textDelta` events. */
  reply: string;
  /** The agent's tool calls, in order, surfaced as a subtle activity trail. */
  activity: ToolActivity[];
  /** True while the turn is still streaming. */
  pending: boolean;
  /** The audit-entry id once the turn completes — drives the quiet undo. */
  turnId?: string;
  /** Set when the turn failed; drives the inline retry affordance. */
  failure?: ChatFailure;
}

interface ChatState {
  /** Stable id for this app-launch chat session; provenance for stored facts. */
  sessionId: string;
  turns: ChatTurn[];
  /** Start a turn from a user message; returns the new turn's local id. */
  startTurn: (userMessage: string) => string;
  /** Append a streamed reply chunk to a turn. */
  appendReply: (id: string, text: string) => void;
  /** Append a tool-activity line to a turn's trail. */
  appendActivity: (id: string, activity: ToolActivity) => void;
  /** Mark a turn complete: set its authoritative reply and audit turn id. */
  completeTurn: (id: string, reply: string, turnId: string) => void;
  /** Mark a turn as failed so it can be retried. */
  failTurn: (id: string, failure: ChatFailure) => void;
  /** Clear a turn's failure + partial state so it can be re-run in place. */
  resetTurn: (id: string) => void;
  clear: () => void;
}

export const useChatStore = create<ChatState>((set) => ({
  sessionId: crypto.randomUUID(),
  turns: [],
  startTurn: (userMessage) => {
    const id = crypto.randomUUID();
    set((state) => ({
      turns: [
        ...state.turns,
        { id, createdAt: Date.now(), userMessage, reply: "", activity: [], pending: true },
      ],
    }));
    return id;
  },
  appendReply: (id, text) =>
    set((state) => ({
      turns: state.turns.map((t) => (t.id === id ? { ...t, reply: t.reply + text } : t)),
    })),
  appendActivity: (id, activity) =>
    set((state) => ({
      turns: state.turns.map((t) =>
        t.id === id ? { ...t, activity: [...t.activity, activity] } : t,
      ),
    })),
  completeTurn: (id, reply, turnId) =>
    set((state) => ({
      turns: state.turns.map((t) => (t.id === id ? { ...t, reply, turnId, pending: false } : t)),
    })),
  failTurn: (id, failure) =>
    set((state) => ({
      turns: state.turns.map((t) => (t.id === id ? { ...t, pending: false, failure } : t)),
    })),
  resetTurn: (id) =>
    set((state) => ({
      turns: state.turns.map((t) =>
        t.id === id
          ? { ...t, reply: "", activity: [], pending: true, failure: undefined, turnId: undefined }
          : t,
      ),
    })),
  clear: () => set({ turns: [] }),
}));

interface FormationState {
  formationPath: string | null;
  noteCount: number;
  notes: FormationNote[];
  currentNotePath: string | null;
  currentNoteContent: string;
  /** The on-disk content as of the last open/save — anchors `isDirty`. */
  originalContent: string;
  isDirty: boolean;
  loading: boolean;

  /** Try to restore the last formation on launch. Safe to call multiple times. */
  restore: () => Promise<void>;
  /** Show native picker, then open the chosen folder. */
  pick: () => Promise<void>;
  /** Open a specific folder as the formation. */
  open: (path: string) => Promise<void>;
  refreshNotes: () => Promise<void>;
  openNote: (relativePath: string) => Promise<void>;
  setContent: (content: string) => void;
  save: () => Promise<void>;
  closeFormation: () => void;

  /** Handle a debounced file-watcher event from the Rust core. */
  handleExternalChange: (paths: string[]) => Promise<void>;
}

export const useFormationStore = create<FormationState>((set, get) => ({
  formationPath: null,
  noteCount: 0,
  notes: [],
  currentNotePath: null,
  currentNoteContent: "",
  originalContent: "",
  isDirty: false,
  loading: false,

  async restore() {
    set({ loading: true });
    try {
      const summary = await tauri.restoreLastFormation();
      if (summary) {
        set({ formationPath: summary.path, noteCount: summary.note_count });
        await get().refreshNotes();
        // Background re-index (skips unchanged files). Beyond keeping search
        // fresh this re-runs Tasks.md reconciliation, so a reminder checked
        // off in Obsidian while the app was closed is caught on next launch.
        tauri.indexFormation(false).catch((e) => console.warn("restore re-index failed:", e));
      }
    } finally {
      set({ loading: false });
    }
  },

  async pick() {
    const path = await tauri.pickFormationDir();
    if (path) await get().open(path);
  },

  async open(path) {
    set({ loading: true });
    try {
      const summary = await tauri.openFormation(path);
      set({
        formationPath: summary.path,
        noteCount: summary.note_count,
        currentNotePath: null,
        currentNoteContent: "",
        originalContent: "",
        isDirty: false,
      });
      await get().refreshNotes();
      // Kick off a background formation re-index (skips unchanged files).
      // Not awaited — progress arrives via `index-progress` events.
      tauri.indexFormation(false).catch((e) => console.warn("background index failed:", e));
    } finally {
      set({ loading: false });
    }
  },

  async refreshNotes() {
    const notes = await tauri.listNotes();
    set({ notes, noteCount: notes.length });
  },

  async openNote(relativePath) {
    let content = "";
    try {
      content = await tauri.readNote(relativePath);
    } catch {
      // The note may have been deleted (e.g. by an undo) — show it empty
      // rather than failing the open.
      content = "";
    }
    set({
      currentNotePath: relativePath,
      currentNoteContent: content,
      originalContent: content,
      isDirty: false,
    });
  },

  setContent(content) {
    set((state) => ({
      currentNoteContent: content,
      // Compare against the on-disk content so reverting to original clears
      // the dirty flag — comparing against the previous edit-state would
      // ratchet `isDirty` to true on the first keystroke and never clear it.
      isDirty: content !== state.originalContent,
    }));
  },

  async save() {
    const { currentNotePath, currentNoteContent } = get();
    if (!currentNotePath) return;
    await tauri.writeNote(currentNotePath, currentNoteContent);
    // Anchor `originalContent` to what we just wrote so a future revert is
    // detected correctly.
    set({ originalContent: currentNoteContent, isDirty: false });
    await get().refreshNotes();
  },

  closeFormation() {
    set({
      formationPath: null,
      noteCount: 0,
      notes: [],
      currentNotePath: null,
      currentNoteContent: "",
      originalContent: "",
      isDirty: false,
    });
  },

  async handleExternalChange(paths) {
    await get().refreshNotes();
    const { currentNotePath, isDirty } = get();
    if (currentNotePath && paths.includes(currentNotePath) && !isDirty) {
      // Reload the active note from disk so we don't show stale content.
      try {
        const content = await tauri.readNote(currentNotePath);
        set({ currentNoteContent: content, originalContent: content });
      } catch (e) {
        // File may have been deleted; clear it.
        console.warn("reload after external change failed:", e);
        set({ currentNotePath: null, currentNoteContent: "", originalContent: "" });
      }
    }
  },
}));

/** How long the quiet post-turn undo is offered from the toast. */
const UNDO_WINDOW_MS = 10_000;

/**
 * A formation modification surfaced as a quiet undo right after it completes
 * (ADR-0009 §6, ADR-0010 §8). Tagged union so the toast can render
 * kind-specific summaries and dispatch to the right undo path.
 */
export type UndoableAction =
  | {
      kind: "chatTurn";
      /** The audit turn id passed to `undo_turn`. */
      turnId: string;
      /** Notes the turn changed — drives the toast's summary line. */
      changedNoteCount: number;
      /** Graph Facts the turn recorded — drives the toast's summary line. */
      recordedFactCount: number;
    }
  | {
      kind: "taskCompletion";
      /** The audit entry id passed to `undo_task_completion`. */
      entryId: string;
      /** Task title for the toast label, e.g. "Logged 'Call dentist'". */
      taskTitle: string;
      /** Formation-relative path of the daily note, for toast subtext. */
      dailyNotePath: string;
    };

interface AuditState {
  /** Every turn's audit entry, newest-first (ADR-0009 §6). */
  entries: AuditEntry[];
  /** The just-completed action, undoable until the window closes; else null. */
  undoable: UndoableAction | null;
  /** Re-list the audit log from disk. */
  refresh: () => Promise<void>;
  /** Arm the quiet undo toast for a freshly-completed turn or task-completion. */
  armUndo: (action: UndoableAction) => void;
  /** Revert a whole turn — restore changed notes, delete recorded Facts. */
  undoTurn: (turnId: string) => Promise<void>;
  /** Revert one recorded Fact from a turn, leaving its notes and other Facts. */
  undoFact: (turnId: string, factId: string) => Promise<void>;
  /**
   * Revert one task-completion append from the daily note.
   * Returns the result so UndoToast can surface the `editedSinceAppended` case.
   */
  undoTaskCompletion: (entryId: string) => Promise<UndoTaskCompletionResult>;
  /**
   * Undo the action the quiet toast is currently offering.
   * Returns the `UndoTaskCompletionResult` for task-completion entries so
   * UndoToast can react to `editedSinceAppended`; returns `"ok"` for chat turns.
   */
  undoFromToast: () => Promise<UndoTaskCompletionResult | "ok">;
  /** Dismiss the quiet undo toast without reverting. */
  dismissUndo: () => void;
  /** Subscribe to the `daily-note-appended` event; call once per app lifetime. */
  setup: () => Promise<() => void>;
}

export const useAuditStore = create<AuditState>((set, get) => {
  let undoTimer: ReturnType<typeof setTimeout> | null = null;

  function clearUndo() {
    if (undoTimer) clearTimeout(undoTimer);
    undoTimer = null;
    set({ undoable: null });
  }

  // The agent edits notes on disk — refresh the file list and reload the
  // active note so the editor shows the post-undo / post-turn content.
  async function reloadFormation() {
    const fs = useFormationStore.getState();
    await fs.refreshNotes();
    if (fs.currentNotePath) await fs.openNote(fs.currentNotePath);
  }

  return {
    entries: [],
    undoable: null,

    async refresh() {
      try {
        set({ entries: await tauri.listAudit() });
      } catch (e) {
        console.warn("audit refresh failed:", e);
      }
    },

    armUndo(action) {
      if (undoTimer) clearTimeout(undoTimer);
      set({ undoable: action });
      undoTimer = setTimeout(() => {
        undoTimer = null;
        set({ undoable: null });
      }, UNDO_WINDOW_MS);
    },

    async undoTurn(turnId) {
      // If the quiet toast is offering this same turn, retire it.
      const current = get().undoable;
      if (current?.kind === "chatTurn" && current.turnId === turnId) clearUndo();
      // Re-throw so the audit panel can show an inline error. We still try to
      // refresh below in `finally` so a partial failure doesn't leave the
      // panel showing stale state.
      try {
        await tauri.undoTurn(turnId);
      } finally {
        await get().refresh();
        await reloadFormation();
      }
    },

    async undoFact(turnId, factId) {
      try {
        await tauri.undoFact(turnId, factId);
      } finally {
        await get().refresh();
      }
    },

    async undoTaskCompletion(entryId) {
      const result = await tauri.undoTaskCompletion(entryId);
      // Always refresh so the panel reflects the new state (removed or
      // editedSinceAppended) and reload the formation so the daily note shows
      // without the bullet.
      await get().refresh();
      await reloadFormation();
      return result;
    },

    async undoFromToast() {
      const pending = get().undoable;
      if (!pending) return "ok";
      clearUndo();
      // The quiet undo toast is best-effort — swallow errors here so a
      // backend hiccup doesn't surface as an unhandled rejection. The audit
      // panel's revert path re-throws and shows the failure inline.
      if (pending.kind === "chatTurn") {
        try {
          await get().undoTurn(pending.turnId);
        } catch (e) {
          console.warn("quiet undo failed:", e);
        }
        return "ok";
      }
      // taskCompletion — return result so UndoToast can react to editedSinceAppended.
      try {
        return await get().undoTaskCompletion(pending.entryId);
      } catch (e) {
        console.warn("quiet undo (task completion) failed:", e);
        return "ok";
      }
    },

    dismissUndo() {
      clearUndo();
    },

    async setup() {
      // Subscribe to the indexer's daily-note-appended event. When it fires,
      // refresh the audit list (the new task_completion entry is on disk) and
      // arm the quiet undo toast. Modelled on useRemindersStore's setup action.
      const unlisten = await listen<DailyNoteAppendedPayload>("daily-note-appended", (event) => {
        const { entryId, dailyNotePath, bulletText } = event.payload;
        // Derive the task title from the bullet text by stripping the leading
        // "- " prefix — the standard shape the indexer appends.
        const taskTitle = bulletText.replace(/^-\s+/, "");
        get()
          .refresh()
          .catch(() => {});
        get().armUndo({ kind: "taskCompletion", entryId, taskTitle, dailyNotePath });
      });
      return unlisten;
    },
  };
});

interface RemindersState {
  /** Every task in the formation, mirrored from the `task` table. */
  tasks: Task[];
  /** The most recently fired reminder, shown as a toast; null when dismissed. */
  dueToast: Task | null;
  /** Reload the task list from the backend. */
  refresh: () => Promise<void>;
  /** Mark a task complete, then refresh. */
  complete: (id: string) => Promise<void>;
  /** Push a task's reminder to an RFC3339 time, then refresh. */
  snooze: (id: string, until: string) => Promise<void>;
  /** Surface a fired reminder as a toast — driven by the `reminder-due` event. */
  showDueToast: (task: Task) => void;
  dismissToast: () => void;
}

// ---------------------------------------------------------------------------
// Working Set (ADR-0011 §3) — the "what's in play" panel state.
// ---------------------------------------------------------------------------

interface WorkingSetState {
  workingSet: WorkingSet | null;
  setWorkingSet: (ws: WorkingSet) => void;
  /** Optimistically remove a dismissed open loop without waiting for a refresh. */
  removeOpenLoop: (loopId: string) => void;
}

export const useWorkingSetStore = create<WorkingSetState>((set) => ({
  workingSet: null,
  setWorkingSet: (ws) => set({ workingSet: ws }),
  removeOpenLoop: (loopId) =>
    set((state) => {
      if (!state.workingSet) return state;
      return {
        workingSet: {
          ...state.workingSet,
          openLoops: state.workingSet.openLoops.filter((l) => l.id !== loopId),
        },
      };
    }),
}));

export const useRemindersStore = create<RemindersState>((set, get) => ({
  tasks: [],
  dueToast: null,

  async refresh() {
    try {
      set({ tasks: await tauri.listTasks() });
    } catch (e) {
      console.warn("reminders refresh failed:", e);
    }
  },

  async complete(id) {
    try {
      await tauri.completeTask(id);
    } catch (e) {
      console.warn("complete task failed:", e);
    }
    // A completed task should not linger as a toast.
    set((s) => (s.dueToast?.id === id ? { dueToast: null } : {}));
    await get().refresh();
  },

  async snooze(id, until) {
    try {
      await tauri.snoozeTask(id, until);
    } catch (e) {
      console.warn("snooze task failed:", e);
    }
    set((s) => (s.dueToast?.id === id ? { dueToast: null } : {}));
    await get().refresh();
  },

  showDueToast(task) {
    set({ dueToast: task });
  },

  dismissToast() {
    set({ dueToast: null });
  },
}));
