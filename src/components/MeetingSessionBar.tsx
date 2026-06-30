import { Icon, initials } from "@/components/icons";
import { isUnknown, speakerTone } from "@/lib/speakers";
import { useFormationStore } from "@/lib/store";
import { type SessionEvent, tauri } from "@/lib/tauri";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

/**
 * Meeting Session capture bar (ADR-0017 §4) — a *slim* recording strip.
 *
 * The meeting collapses into the single conversation (ADR-0017 §4): this bar is
 * just the recording control + live status, not a second surface. While recording,
 * the backend captures (mic + system-output loopback) → on-device ASR → diarization
 * and streams the transcript into the **Meeting note** (open it to watch it grow);
 * your chat in the main conversation is automatically grounded on the meeting,
 * during it and for a window after (so "what did Sarah say about Q3?" just works).
 * Speakers show as coloured avatars — click one to name them ("that was Sarah"),
 * which also enrolls their Voiceprint. On stop, a background distillation turn
 * surfaces a one-line receipt + undo (and an optional content-derived title).
 *
 * Design language: Strata — `bg-surface` chrome at the InFocusBar height, Plex Mono
 * for the elapsed clock, the terracotta accent for the primary action, and one
 * alive element: the danger-red recording pulse.
 */

function fmtOffset(ms: number): string {
  const t = Math.max(0, Math.floor(ms / 1000));
  const s = String(t % 60).padStart(2, "0");
  const m = Math.floor(t / 60);
  if (m < 60) return `${String(m).padStart(2, "0")}:${s}`;
  return `${Math.floor(m / 60)}:${String(m % 60).padStart(2, "0")}:${s}`;
}

function basename(path: string): string {
  const cut = path.replace(/\\/g, "/").split("/").pop() ?? path;
  return cut.replace(/\.md$/i, "");
}

export function MeetingSessionBar() {
  const openNote = useFormationStore((s) => s.openNote);
  const notes = useFormationStore((s) => s.notes);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [title, setTitle] = useState("");
  // "Who's here" roster picked before recording (ADR-0017 §A2). Seeds the diarizer
  // with only these voices so the people in the room are named from their first
  // words. Entirely optional — Record never waits on it.
  const [expected, setExpected] = useState<string[]>([]);
  const [showRoster, setShowRoster] = useState(false);
  const people = useMemo(
    () =>
      notes
        .filter((n) => n.relative_path.startsWith("People/"))
        .map((n) => n.relative_path.replace(/^People\//, "").replace(/\.md$/i, ""))
        .sort((a, b) => a.localeCompare(b)),
    [notes],
  );
  const [notePath, setNotePath] = useState<string | null>(null);
  // Mirror of notePath read from event callbacks (which capture a stale closure),
  // so the distillation receipt is correlated to the meeting it belongs to.
  const notePathRef = useRef<string | null>(null);
  const [attendees, setAttendees] = useState<string[]>([]);
  const [startedAt, setStartedAt] = useState<number | null>(null);
  const [elapsed, setElapsed] = useState(0);
  const [barError, setBarError] = useState<string | null>(null);
  const busy = useRef(false);

  // Speaker rename popover, anchored to the clicked avatar.
  const [rename, setRename] = useState<{ from: string; x: number; y: number } | null>(null);
  const [renameValue, setRenameValue] = useState("");

  // The speaker of the most recent transcript segment — "who's talking now" — so the
  // current speaker can be named the moment they change (ADR-0017 §6), not only from
  // the static attendee avatars.
  const [currentSpeaker, setCurrentSpeaker] = useState<string | null>(null);
  // A name a speaker said out loud ("I'm Sarah"), offered as a one-tap rename for the
  // still-unnamed current speaker (ADR-0017 §6, suggest-not-assert).
  const [suggestion, setSuggestion] = useState<{ speaker: string; name: string } | null>(null);

  // ASR model readiness: null = checking, true = installed, false = needs download.
  // A build without `local-asr` lacks the command — treat as ready (manual only).
  const [asrReady, setAsrReady] = useState<boolean | null>(null);
  const [setupPhase, setSetupPhase] = useState<string | null>(null);
  const [setupPct, setSetupPct] = useState<number | null>(null);

  // Post-stop progress (ADR-0017 §2): the meeting finishes in the background — the
  // offline second pass sharpens the transcript, then distillation summarizes. We
  // narrate it so the polish is *visible* (the user's last impression is the improved
  // transcript, not the rough live one), then it hands off to the distill receipt.
  // null = nothing pending; "refining" = working; "refined" = transcript sharpened.
  const [finishPhase, setFinishPhase] = useState<null | "refining" | "refined">(null);

  // The end-of-session distillation receipt (ADR-0017 §7): a quiet summary + undo,
  // plus an optional content-derived title offered as a one-tap rename.
  const [distill, setDistill] = useState<{
    summary: string;
    turnId: string;
    suggestedTitle: string | null;
    notePath: string | null;
  } | null>(null);
  const [renaming, setRenaming] = useState(false);

  useEffect(() => {
    tauri
      .checkAsrReadiness()
      .then((r) => setAsrReady(r.allPresent))
      .catch(() => setAsrReady(true));
  }, []);

  // Tick the elapsed clock once a second while recording.
  useEffect(() => {
    if (startedAt === null) return;
    setElapsed(Date.now() - startedAt);
    const id = window.setInterval(() => setElapsed(Date.now() - startedAt), 1000);
    return () => window.clearInterval(id);
  }, [startedAt]);

  const onEvent = useCallback((e: SessionEvent) => {
    switch (e.kind) {
      case "status":
        if (e.state === "stopped") {
          setSessionId(null);
          setStartedAt(null);
          setCurrentSpeaker(null);
          setSuggestion(null);
        }
        break;
      case "attendeeChanged":
        setAttendees(e.attendees);
        break;
      case "segment":
        // Track who's speaking now so the strip can offer to name them on change.
        setCurrentSpeaker(e.segment.speaker);
        break;
      case "speakerNameSuggested":
        // Only surface while the named speaker is still the one talking and unnamed.
        setSuggestion({ speaker: e.speaker, name: e.name });
        break;
      case "transcriptRefined": {
        // The second pass sharpened the transcript — mark it so the receipt can show
        // the win, and force-reload the note if the user is viewing this meeting
        // (belt-and-suspenders with the file watcher).
        setFinishPhase("refined");
        const store = useFormationStore.getState();
        if (store.currentNotePath && store.currentNotePath === notePathRef.current) {
          store.openNote(store.currentNotePath).catch(() => {});
        }
        break;
      }
      case "distilled":
        // Distillation done — hand the progress receipt over to the summary receipt.
        setFinishPhase(null);
        // Correlate to the meeting it belongs to (notePathRef), not whatever note
        // the bar happens to be pointing at now (a 2nd meeting may have started).
        setDistill({
          summary: e.summary,
          turnId: e.turnId,
          suggestedTitle: e.suggestedTitle,
          notePath: notePathRef.current,
        });
        break;
      // `segment` / `note` stream into the Meeting note, not this bar — open the
      // note to watch the transcript grow, or just chat about it below.
    }
  }, []);

  const downloadModels = useCallback(async () => {
    setSetupPhase("starting…");
    setSetupPct(null);
    try {
      await tauri.downloadAsrModel((p) => {
        setSetupPct(p.total > 0 ? Math.round((p.completed / p.total) * 100) : null);
        setSetupPhase(p.done ? "done" : p.phase);
      });
      setAsrReady(true);
    } catch (err) {
      console.error("ASR model download failed:", err);
      setSetupPhase("download failed — see logs");
      setSetupPct(null);
    }
  }, []);

  const start = useCallback(async () => {
    if (busy.current) return;
    busy.current = true;
    setBarError(null);
    try {
      const res = await tauri.sessionStart(
        title.trim() || "Meeting",
        onEvent,
        expected.length ? expected : undefined,
      );
      setSessionId(res.sessionId);
      setNotePath(res.notePath);
      notePathRef.current = res.notePath;
      setAttendees([]);
      setStartedAt(Date.now());
      setDistill(null);
      setFinishPhase(null);
      setShowRoster(false);
    } catch (err) {
      console.error("session start failed:", err);
      setBarError("Couldn't start recording — check the mic permission, then retry.");
    } finally {
      busy.current = false;
    }
  }, [title, onEvent, expected]);

  const stop = useCallback(async () => {
    if (!sessionId) return;
    setBarError(null);
    try {
      const res = await tauri.sessionStop(sessionId);
      // Only collapse the bar on a confirmed stop; otherwise capture may still be
      // running and we'd lie about the state. (The `status:stopped` event also
      // clears these, belt-and-suspenders.)
      setSessionId(null);
      setStartedAt(null);
      // Surface the background finishing pass so the polish is visible, not silent.
      if (res.segmentCount > 0) setFinishPhase("refining");
    } catch (err) {
      console.error("session stop failed:", err);
      setBarError("Couldn't stop the recording — it may still be running. Try again.");
    }
  }, [sessionId]);

  // Commit a live speaker rename (ADR-0017 §6); the backend relabels the Meeting
  // note's transcript + attendees and enrolls the voiceprint.
  const commitRename = useCallback(
    async (to: string) => {
      if (!sessionId || !rename) return;
      const from = rename.from;
      const next = to.trim();
      setRename(null);
      setRenameValue("");
      if (!next || next === from) return;
      setAttendees((prev) => prev.map((a) => (a === from ? next : a)));
      try {
        await tauri.sessionRenameSpeaker(sessionId, from, next);
      } catch (err) {
        console.error("rename speaker failed:", err);
      }
    },
    [sessionId, rename],
  );

  const openRename = useCallback((from: string, target: HTMLElement) => {
    const r = target.getBoundingClientRect();
    setRenameValue("");
    setRename({ from, x: r.left, y: r.bottom + 6 });
  }, []);

  // Accept a heard-name suggestion (ADR-0017 §6): rename the speaker to the detected
  // name, which also enrolls their Voiceprint + voice clip for next time.
  const acceptSuggestion = useCallback(async () => {
    if (!sessionId || !suggestion) return;
    const { speaker, name } = suggestion;
    setSuggestion(null);
    setAttendees((prev) => prev.map((a) => (a === speaker ? name : a)));
    try {
      await tauri.sessionRenameSpeaker(sessionId, speaker, name);
    } catch (err) {
      console.error("accept name suggestion failed:", err);
    }
  }, [sessionId, suggestion]);

  const undoDistill = useCallback(async () => {
    if (!distill) return;
    try {
      await tauri.undoTurn(distill.turnId);
    } catch (err) {
      console.error("undo distillation failed:", err);
    } finally {
      setDistill(null);
    }
  }, [distill]);

  // Accept the distillation's suggested title: rename the Meeting note (file + H1
  // + graph node) and re-point the local note link, then drop the suggestion.
  const acceptRename = useCallback(async () => {
    const next = distill?.suggestedTitle?.trim();
    const target = distill?.notePath;
    if (!next || !target || renaming) return;
    setRenaming(true);
    try {
      const res = await tauri.renameMeetingNote(target, next);
      // Only re-point the bar's own note link if it's still the same meeting.
      if (notePathRef.current === target) {
        setNotePath(res.notePath);
        notePathRef.current = res.notePath;
        setTitle(next);
      }
      setDistill((d) => (d ? { ...d, suggestedTitle: null } : d));
    } catch (err) {
      console.error("rename meeting failed:", err);
    } finally {
      setRenaming(false);
    }
  }, [distill, renaming]);

  // Decline just the rename, keeping the receipt's summary + undo in place.
  const dismissRename = useCallback(() => {
    setDistill((d) => (d ? { ...d, suggestedTitle: null } : d));
  }, []);

  // The background-finishing receipt (ADR-0017 §2): shown between Stop and the
  // distillation summary so the work is *visible*. It ends on "Transcript sharpened",
  // making the accuracy win the last thing the user sees (peak-end). Shares the
  // bottom-right slot with the summary receipt and yields to it the moment it lands.
  const finishToast =
    finishPhase && !distill ? (
      <div className="fixed right-5 bottom-5 z-50 flex w-[min(32rem,calc(100vw-2.5rem))] items-center gap-3 rounded-xl border border-line-strong bg-raised px-4 py-2.5 text-ink-soft shadow-2xl">
        {finishPhase === "refined" ? (
          <Icon.Check aria-hidden className="h-4 w-4 shrink-0 text-sage" />
        ) : (
          <span
            className="h-[7px] w-[7px] shrink-0 rounded-full bg-accent"
            style={{ animation: "infocus-pulse 1.6s ease-in-out infinite" }}
            aria-hidden
          />
        )}
        <span className="min-w-0 flex-1 truncate text-sm">
          {finishPhase === "refined"
            ? "Transcript sharpened — finishing the summary…"
            : "Wrapping up — sharpening the transcript…"}
        </span>
      </div>
    ) : null;

  // The distillation receipt is a quiet "summary + undo" notification. Anchored to
  // the bottom-right corner (not bottom-center) so it stays clear of the centered
  // chat composer and the centered Undo/Reminder toasts — no overlap.
  const distillToast = distill ? (
    <div className="fixed right-5 bottom-5 z-50 flex w-[min(32rem,calc(100vw-2.5rem))] flex-col gap-2 rounded-xl border border-line-strong bg-raised px-4 py-2.5 text-ink-soft shadow-2xl">
      <div className="flex items-center gap-3">
        <Icon.Sparkle aria-hidden className="h-4 w-4 shrink-0 text-gold" />
        <span className="min-w-0 flex-1 truncate text-sm">{distill.summary}</span>
        {distill.turnId && (
          <button
            type="button"
            onClick={() => void undoDistill()}
            className="flex items-center gap-1.5 rounded-lg border border-line-strong bg-raised px-3 py-1 font-semibold text-[12px] text-accent-ink hover:border-accent hover:bg-accent-tint"
          >
            <Icon.Undo className="h-3 w-3" />
            Undo
          </button>
        )}
        <button
          type="button"
          aria-label="Dismiss"
          onClick={() => setDistill(null)}
          className="grid h-6 w-6 place-items-center rounded-md text-muted hover:bg-bg-sunk hover:text-ink"
        >
          <Icon.X className="h-4 w-4" />
        </button>
      </div>

      {/* Optional: rename the meeting to the title the distillation derived from
          what was actually discussed (ADR-0017 §7). Suggest, never assert. */}
      {distill.suggestedTitle && (
        <div className="flex items-center gap-2 border-line border-t pt-2">
          <Icon.Pencil aria-hidden className="h-3.5 w-3.5 shrink-0 text-muted" />
          <span className="min-w-0 flex-1 truncate text-[12.5px] text-ink-soft">
            Rename to <span className="font-semibold text-ink">“{distill.suggestedTitle}”</span>?
          </span>
          <button
            type="button"
            onClick={() => void acceptRename()}
            disabled={renaming}
            className="flex items-center gap-1.5 rounded-lg border border-line-strong bg-raised px-3 py-1 font-semibold text-[12px] text-accent-ink hover:border-accent hover:bg-accent-tint disabled:opacity-40"
          >
            <Icon.Check className="h-3 w-3" />
            Rename
          </button>
          <button
            type="button"
            onClick={dismissRename}
            className="rounded-md px-2 py-1 text-[12px] text-muted hover:bg-bg-sunk hover:text-ink"
          >
            Keep
          </button>
        </div>
      )}
    </div>
  ) : null;

  // ── Idle: model setup needed ──────────────────────────────────────────────
  if (!sessionId && asrReady === false) {
    return (
      <>
        <div className="flex items-center gap-3 border-b border-line bg-surface px-5 py-2.5">
          <span className="inline-flex shrink-0 items-center gap-1.5 text-[10px] font-bold uppercase tracking-[.08em] text-ink-soft">
            <Icon.Mic className="h-3.5 w-3.5 text-muted" />
            Meeting
          </span>
          <div className="min-w-0 flex-1">
            <p className="truncate text-[12.5px] text-ink-soft">
              {setupPhase
                ? `Setting up transcription · ${setupPhase}${setupPct !== null ? ` · ${setupPct}%` : ""}`
                : "On-device transcription models needed — once, ~1 GB, then it runs offline."}
            </p>
            {setupPhase && setupPhase !== "download failed — see logs" && (
              <div className="mt-1 h-1 overflow-hidden rounded-full bg-bg-sunk">
                <div
                  className="h-full bg-accent transition-all"
                  style={{ width: setupPct !== null ? `${setupPct}%` : "33%" }}
                />
              </div>
            )}
          </div>
          <button
            type="button"
            onClick={() => void downloadModels()}
            disabled={!!setupPhase && setupPhase !== "download failed — see logs"}
            className="shrink-0 rounded-md bg-accent px-4 py-1.5 text-[13px] font-medium text-white hover:bg-accent-ink disabled:opacity-40"
          >
            Download models
          </button>
        </div>
        {finishToast}
        {distillToast}
      </>
    );
  }

  // ── Idle: ready to record ─────────────────────────────────────────────────
  if (!sessionId) {
    const toggleExpected = (name: string) =>
      setExpected((prev) =>
        prev.includes(name) ? prev.filter((n) => n !== name) : [...prev, name],
      );
    return (
      <>
        <div className="border-b border-line bg-surface px-5 py-2.5">
          <div className="flex items-center gap-3">
            <span className="inline-flex shrink-0 items-center gap-1.5 text-[10px] font-bold uppercase tracking-[.08em] text-ink-soft">
              <Icon.Mic className="h-3.5 w-3.5 text-muted" />
              Meeting
            </span>
            <input
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && void start()}
              placeholder="Name this meeting, then record…"
              className="min-w-0 flex-1 rounded-md border border-transparent bg-transparent px-2 py-1 text-[13px] text-ink placeholder:text-faint hover:border-line focus:border-accent-ink focus:bg-surface focus:outline-none"
            />
            {barError && (
              <span className="shrink-0 truncate text-[11px] text-danger">{barError}</span>
            )}
            {/* Optional "who's here" — pick the people in the room so they're named as
                they speak. Skippable; Record never waits on it (ADR-0017 §A2). */}
            {people.length > 0 && (
              <button
                type="button"
                onClick={() => setShowRoster((v) => !v)}
                aria-expanded={showRoster}
                title="Pick who's in the room (optional)"
                className={[
                  "inline-flex shrink-0 items-center gap-1.5 rounded-md border px-2.5 py-1.5 text-[12px] transition-colors",
                  expected.length > 0 || showRoster
                    ? "border-accent bg-accent-tint text-accent-ink"
                    : "border-line bg-transparent text-muted hover:border-accent hover:text-accent-ink",
                ].join(" ")}
              >
                <Icon.Person className="h-3.5 w-3.5" />
                {expected.length > 0 ? `${expected.length} here` : "Who's here"}
              </button>
            )}
            <button
              type="button"
              onClick={() => void start()}
              className="group inline-flex shrink-0 items-center gap-2 rounded-md bg-accent px-4 py-1.5 text-[13px] font-medium text-white hover:bg-accent-ink"
            >
              <span className="h-2 w-2 rounded-full bg-white/90 transition-transform group-hover:scale-110" />
              Record
            </button>
          </div>

          {showRoster && people.length > 0 && (
            <div className="mt-2.5 flex flex-wrap items-center gap-1.5 border-line border-t pt-2.5">
              <span className="mr-1 text-[10px] font-bold uppercase tracking-[.08em] text-faint">
                Who's here
              </span>
              {people.map((name) => {
                const on = expected.includes(name);
                return (
                  <button
                    key={name}
                    type="button"
                    onClick={() => toggleExpected(name)}
                    aria-pressed={on}
                    className={[
                      "inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-[12px] transition-colors",
                      on
                        ? "border-accent bg-accent-tint text-accent-ink"
                        : "border-line bg-raised text-ink-soft hover:border-accent",
                    ].join(" ")}
                  >
                    <span
                      className="inline-grid h-[16px] w-[16px] place-items-center rounded-full text-[8px] font-bold text-white"
                      style={{ background: speakerTone(name) }}
                      aria-hidden
                    >
                      {initials(name)}
                    </span>
                    {name}
                    {on && <Icon.Check className="h-3 w-3" />}
                  </button>
                );
              })}
              <span className="ml-1 text-[10px] text-faint">Named as they speak · optional</span>
            </div>
          )}
        </div>
        {finishToast}
        {distillToast}
      </>
    );
  }

  // ── Recording — a slim strip; the transcript lives in the Meeting note and the
  //    conversation below is grounded on it ──────────────────────────────────
  const named = attendees.filter((a) => !isUnknown(a));
  const renameTargets = rename ? named.filter((a) => a !== rename.from) : [];

  return (
    <div className="flex items-center gap-2.5 border-b border-line bg-surface px-5 py-2">
      <span className="inline-flex shrink-0 items-center gap-2 text-[11px] font-bold uppercase tracking-[.08em] text-danger">
        <span
          className="h-[7px] w-[7px] rounded-full bg-danger"
          style={{ animation: "infocus-pulse 1.6s ease-in-out infinite" }}
          aria-hidden
        />
        Recording
      </span>
      <span className="font-mono text-[12px] tabular-nums text-ink-soft">{fmtOffset(elapsed)}</span>
      {barError && <span className="truncate text-[11px] text-danger">{barError}</span>}

      {/* Attendee avatars — click to name a speaker ("that was Sarah", §6) */}
      {attendees.length > 0 && (
        <div className="flex min-w-0 items-center gap-1.5 overflow-hidden">
          <span className="h-[16px] w-px shrink-0 bg-line-strong" aria-hidden />
          <div className="flex items-center -space-x-1">
            {attendees.slice(0, 6).map((a) => (
              <button
                key={a}
                type="button"
                onClick={(e) => openRename(a, e.currentTarget)}
                title={`Name this speaker (${a})`}
                aria-label={`Name speaker ${a}`}
                className="inline-grid h-[20px] w-[20px] place-items-center rounded-full border border-surface text-[9px] font-bold text-white transition-transform hover:z-10 hover:scale-110"
                style={{ background: speakerTone(a) }}
              >
                {isUnknown(a) ? "?" : initials(a)}
              </button>
            ))}
          </div>
          {attendees.length > 6 && (
            <span className="text-[11px] text-muted">+{attendees.length - 6}</span>
          )}
        </div>
      )}

      {/* Who's talking now — name them the moment the speaker changes (ADR-0017 §6).
          Unknown → a dashed "Name" affordance; named → click to reassign. Hidden when
          a heard-name suggestion is already prompting for this same speaker, so the
          two naming affordances never stack. */}
      {currentSpeaker && suggestion?.speaker !== currentSpeaker && (
        <button
          type="button"
          onClick={(e) => openRename(currentSpeaker, e.currentTarget)}
          title={`Name the current speaker (${currentSpeaker})`}
          className={[
            "inline-flex shrink-0 items-center gap-1.5 rounded-full border px-2.5 py-1 text-[11.5px] transition-colors",
            isUnknown(currentSpeaker)
              ? "border-dashed border-line-strong bg-bg-sunk text-ink-soft hover:border-accent"
              : "border-line bg-raised text-ink hover:border-accent",
          ].join(" ")}
        >
          {/* Static tone dot — the recording pulse stays the bar's one alive element. */}
          <span
            className="h-[7px] w-[7px] rounded-full"
            style={{ background: speakerTone(currentSpeaker) }}
            aria-hidden
          />
          <span className="text-[9px] font-bold uppercase tracking-[.08em] text-faint">Now</span>
          {isUnknown(currentSpeaker) ? "Name speaker" : currentSpeaker}
          <Icon.Pencil className="h-3 w-3 text-faint" />
        </button>
      )}

      {/* Heard-name suggestion ("I'm Sarah") — suggested, one tap to accept (§6). */}
      {suggestion && (
        <div className="inline-flex shrink-0 items-center gap-1.5 rounded-full border border-accent bg-accent-tint px-2.5 py-1 text-[11.5px] text-accent-ink">
          <Icon.Sparkle className="h-3.5 w-3.5 shrink-0" aria-hidden />
          {/* Say *why* this appeared (we heard the name) — an unexplained auto-guess
              erodes trust; a transparent one earns the tap. */}
          <span className="truncate">
            Heard a name — call them <span className="font-semibold">{suggestion.name}</span>?
          </span>
          <button
            type="button"
            onClick={() => void acceptSuggestion()}
            className="rounded-md bg-accent px-2 py-0.5 font-semibold text-[11px] text-white hover:bg-accent-ink"
          >
            Name
          </button>
          <button
            type="button"
            aria-label="Dismiss name suggestion"
            onClick={() => setSuggestion(null)}
            className="grid h-5 w-5 place-items-center rounded text-accent-ink/70 hover:bg-accent/20 hover:text-accent-ink"
          >
            <Icon.X className="h-3.5 w-3.5" />
          </button>
        </div>
      )}

      {/* The "same chat" cue: the conversation below already knows about this. */}
      <span className="hidden items-center gap-1.5 text-[11px] text-muted lg:inline-flex">
        <Icon.Chat className="h-3 w-3" aria-hidden />
        Ask about it in the chat
      </span>

      <div className="ml-auto flex shrink-0 items-center gap-2">
        {notePath && (
          <button
            type="button"
            onClick={() => openNote(notePath).catch(() => {})}
            title={`Open transcript · ${notePath}`}
            className="inline-flex items-center gap-1.5 rounded-md px-2 py-1 text-[11.5px] text-muted hover:bg-bg-sunk hover:text-ink-soft"
          >
            <Icon.File className="h-3.5 w-3.5" />
            <span className="hidden max-w-[14rem] truncate sm:inline">{basename(notePath)}</span>
          </button>
        )}
        <button
          type="button"
          onClick={() => void stop()}
          className="inline-flex items-center gap-1.5 rounded-md border border-line-strong bg-raised px-3 py-1 text-[12.5px] font-medium text-ink-soft shadow-sm hover:border-danger hover:text-danger"
        >
          <Icon.Stop className="h-3.5 w-3.5" />
          Stop
        </button>
      </div>

      {/* Speaker rename popover (ADR-0017 §6) — "that was Sarah" */}
      {rename && (
        <>
          <button
            type="button"
            aria-label="Close"
            className="fixed inset-0 z-40 cursor-default"
            onClick={() => setRename(null)}
          />
          <div
            className="fixed z-50 w-64 rounded-lg border border-line-strong bg-raised p-3 shadow-2xl"
            style={{
              left: Math.min(rename.x, window.innerWidth - 268),
              top: Math.min(rename.y, window.innerHeight - 200),
            }}
          >
            <p className="mb-2 text-[10px] font-bold uppercase tracking-[.08em] text-muted">
              Who was speaking?
            </p>
            {renameTargets.length > 0 && (
              <div className="mb-2 flex flex-wrap gap-1.5">
                {renameTargets.map((name) => (
                  <button
                    key={name}
                    type="button"
                    onClick={() => void commitRename(name)}
                    className="inline-flex items-center gap-1.5 rounded-full border border-line bg-surface px-2.5 py-1 text-[12px] text-ink hover:border-accent"
                  >
                    <span
                      className="inline-grid h-[16px] w-[16px] place-items-center rounded-full text-[8px] font-bold text-white"
                      style={{ background: speakerTone(name) }}
                      aria-hidden
                    >
                      {initials(name)}
                    </span>
                    {name}
                  </button>
                ))}
              </div>
            )}
            <input
              // biome-ignore lint/a11y/noAutofocus: a popover that opens on intent should focus its field
              autoFocus
              value={renameValue}
              onChange={(e) => setRenameValue(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void commitRename(renameValue);
                else if (e.key === "Escape") setRename(null);
              }}
              placeholder="New name…"
              className="w-full rounded-md border border-line bg-surface px-2.5 py-1.5 text-[13px] text-ink placeholder:text-faint focus:border-accent-ink focus:outline-none"
            />
            <p className="mt-1.5 text-[10px] leading-snug text-faint">
              Naming a speaker relabels the transcript and remembers their voice for next time.
            </p>
          </div>
        </>
      )}

      {finishToast}

      {distillToast}
    </div>
  );
}
