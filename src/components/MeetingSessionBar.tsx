import { type SessionEvent, type TranscriptSegment, tauri } from "@/lib/tauri";
import { useCallback, useEffect, useRef, useState } from "react";

/**
 * Meeting Session capture bar (ADR-0017 §4).
 *
 * The transient capture surface: it exists only while a Session is open and
 * collapses back to nothing on stop — the durable artifact is the Meeting note.
 * On Start the backend runs real capture (mic + system-output loopback) → on-device
 * ASR → diarization, streaming `segment` events as people speak. The text field
 * below stays available for a hand-typed note or a manual correction; it is no
 * longer the source of transcript text.
 */
export function MeetingSessionBar() {
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [title, setTitle] = useState("");
  const [notePath, setNotePath] = useState<string | null>(null);
  const [segments, setSegments] = useState<TranscriptSegment[]>([]);
  const [attendees, setAttendees] = useState<string[]>([]);
  const [speaker, setSpeaker] = useState("Self");
  const [line, setLine] = useState("");
  const [asNote, setAsNote] = useState(false);
  const busy = useRef(false);
  // ASR model readiness: null = unknown/checking, true = installed, false = needs
  // download. A build without `local-asr` lacks the command — treat as ready so the
  // bar still works (manual segments only).
  const [asrReady, setAsrReady] = useState<boolean | null>(null);
  const [setupPhase, setSetupPhase] = useState<string | null>(null);
  // The end-of-session distillation receipt (ADR-0017 §7): a one-line summary +
  // the audit turn id, surfaced quietly with an undo after a meeting ends.
  const [distill, setDistill] = useState<{ summary: string; turnId: string } | null>(null);

  useEffect(() => {
    tauri
      .checkAsrReadiness()
      .then((r) => setAsrReady(r.allPresent))
      .catch(() => setAsrReady(true));
  }, []);

  const downloadModels = useCallback(async () => {
    setSetupPhase("starting…");
    try {
      await tauri.downloadAsrModel((p) => {
        const pct = p.total > 0 ? Math.round((p.completed / p.total) * 100) : 0;
        setSetupPhase(p.done ? "done" : `${p.phase} ${pct}%`);
      });
      setAsrReady(true);
    } catch (err) {
      console.error("ASR model download failed:", err);
      setSetupPhase("download failed — see logs");
    }
  }, []);

  const onEvent = useCallback((e: SessionEvent) => {
    switch (e.kind) {
      case "status":
        if (e.state === "stopped") {
          setSessionId(null);
        }
        break;
      case "segment":
        setSegments((prev) => [...prev, e.segment]);
        break;
      case "attendeeChanged":
        setAttendees(e.attendees);
        break;
      case "note":
        // Notes are time-anchored into ## Notes; surface them inline too.
        setSegments((prev) => [
          ...prev,
          { offsetMs: e.offsetMs, speaker: "📝 note", text: e.text },
        ]);
        break;
      case "distilled":
        // The background distillation finished — show its receipt + undo.
        setDistill({ summary: e.summary, turnId: e.turnId });
        break;
    }
  }, []);

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

  const start = useCallback(async () => {
    if (busy.current) return;
    busy.current = true;
    try {
      const res = await tauri.sessionStart(title.trim() || "Meeting", onEvent);
      setSessionId(res.sessionId);
      setNotePath(res.notePath);
      setSegments([]);
      setAttendees([]);
      setDistill(null);
    } catch (err) {
      console.error("session start failed:", err);
    } finally {
      busy.current = false;
    }
  }, [title, onEvent]);

  const push = useCallback(async () => {
    const text = line.trim();
    if (!sessionId || !text) return;
    try {
      if (asNote) {
        await tauri.sessionPushNote(sessionId, text);
      } else {
        await tauri.sessionPushSegment(sessionId, speaker.trim() || "Unknown", text);
      }
      setLine("");
    } catch (err) {
      console.error("session push failed:", err);
    }
  }, [sessionId, line, asNote, speaker]);

  const stop = useCallback(async () => {
    if (!sessionId) return;
    try {
      await tauri.sessionStop(sessionId);
    } catch (err) {
      console.error("session stop failed:", err);
    } finally {
      setSessionId(null);
    }
  }, [sessionId]);

  // "That was Sarah" — name a speaker (ADR-0017 §6). Relabels the transcript and
  // attendees in the note; optimistically relabel the live view too.
  const renameSpeaker = useCallback(
    async (from: string) => {
      if (!sessionId) return;
      const to = window.prompt(`Name this speaker (was "${from}")`, "")?.trim();
      if (!to || to === from) return;
      try {
        await tauri.sessionRenameSpeaker(sessionId, from, to);
        setSegments((prev) => prev.map((s) => (s.speaker === from ? { ...s, speaker: to } : s)));
      } catch (err) {
        console.error("rename speaker failed:", err);
      }
    },
    [sessionId],
  );

  const fmt = (ms: number) => {
    const t = Math.max(0, Math.floor(ms / 1000));
    return `${String(Math.floor(t / 60)).padStart(2, "0")}:${String(t % 60).padStart(2, "0")}`;
  };

  // The post-meeting distillation receipt (ADR-0017 §7): a quiet one-line summary
  // with a one-click undo. Shown above the idle bar after a Session ends.
  const distillBanner = distill ? (
    <div className="flex items-center gap-2 border-b border-line bg-bg px-3 py-1.5 text-xs">
      <span className="text-muted">✶ Distilled</span>
      <span className="min-w-0 flex-1 truncate">{distill.summary}</span>
      <button type="button" onClick={undoDistill} className="text-muted hover:underline">
        Undo
      </button>
      <button
        type="button"
        onClick={() => setDistill(null)}
        className="text-muted hover:text-fg"
        aria-label="Dismiss"
      >
        ✕
      </button>
    </div>
  ) : null;

  if (!sessionId) {
    // Models missing → prompt a one-time download instead of opening a Session
    // that can't transcribe (ADR-0016 explicit-setup posture).
    if (asrReady === false) {
      return (
        <>
          {distillBanner}
          <div className="flex items-center gap-2 border-b border-line bg-bg px-3 py-1.5 text-sm">
            <span className="text-muted">Meeting</span>
            <span className="min-w-0 flex-1 truncate text-muted">
              {setupPhase
                ? `Setting up transcription · ${setupPhase}`
                : "On-device transcription model needed (~0.3 GB, one time)"}
            </span>
            <button
              type="button"
              onClick={downloadModels}
              disabled={!!setupPhase && setupPhase !== "download failed — see logs"}
              className="rounded bg-accent px-3 py-1 font-medium text-white hover:opacity-90 disabled:opacity-50"
            >
              Download models
            </button>
          </div>
        </>
      );
    }
    return (
      <>
        {distillBanner}
        <div className="flex items-center gap-2 border-b border-line bg-bg px-3 py-1.5 text-sm">
          <span className="text-muted">Meeting</span>
          <input
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && start()}
            placeholder="Title (e.g. Q3 Planning)"
            className="min-w-0 flex-1 rounded border border-line bg-transparent px-2 py-1 outline-none focus:border-accent"
          />
          <button
            type="button"
            onClick={start}
            className="rounded bg-accent px-3 py-1 font-medium text-white hover:opacity-90"
          >
            ● Start session
          </button>
        </div>
      </>
    );
  }

  return (
    <div className="flex flex-col border-b border-line bg-bg text-sm">
      <div className="flex items-center gap-2 px-3 py-1.5">
        <span className="inline-flex items-center gap-1.5 font-medium text-red-500">
          <span className="h-2 w-2 animate-pulse rounded-full bg-red-500" /> Recording
        </span>
        {notePath && <span className="truncate text-xs text-muted">{notePath}</span>}
        {attendees.length > 0 && (
          <span className="truncate text-xs text-muted">· {attendees.join(", ")}</span>
        )}
        <button
          type="button"
          onClick={stop}
          className="ml-auto rounded border border-line px-3 py-1 hover:border-accent"
        >
          ■ Stop
        </button>
      </div>

      {segments.length > 0 && (
        <div className="max-h-40 overflow-y-auto px-3 pb-2">
          {segments.map((s, i) => (
            <div key={`${s.offsetMs}-${i}`} className="leading-relaxed">
              <span className="text-muted">[{fmt(s.offsetMs)}]</span>{" "}
              {s.speaker.startsWith("📝") ? (
                <span className="font-medium">{s.speaker}:</span>
              ) : (
                <button
                  type="button"
                  onClick={() => renameSpeaker(s.speaker)}
                  title="Name this speaker"
                  className="font-medium hover:underline"
                >
                  {s.speaker}:
                </button>
              )}{" "}
              {s.text}
            </div>
          ))}
        </div>
      )}

      <div className="flex items-center gap-2 border-t border-line px-3 py-1.5">
        {!asNote && (
          <input
            value={speaker}
            onChange={(e) => setSpeaker(e.target.value)}
            placeholder="Speaker"
            className="w-28 rounded border border-line bg-transparent px-2 py-1 outline-none focus:border-accent"
          />
        )}
        <input
          value={line}
          onChange={(e) => setLine(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && push()}
          placeholder={asNote ? "Note alongside the meeting…" : "Type a manual line or correction…"}
          className="min-w-0 flex-1 rounded border border-line bg-transparent px-2 py-1 outline-none focus:border-accent"
        />
        <label className="flex items-center gap-1 text-xs text-muted">
          <input type="checkbox" checked={asNote} onChange={(e) => setAsNote(e.target.checked)} />
          note
        </label>
        <button
          type="button"
          onClick={push}
          className="rounded bg-accent px-3 py-1 font-medium text-white hover:opacity-90"
        >
          Add
        </button>
      </div>
    </div>
  );
}
