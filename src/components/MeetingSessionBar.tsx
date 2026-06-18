import { type SessionEvent, type TranscriptSegment, tauri } from "@/lib/tauri";
import { useCallback, useRef, useState } from "react";

/**
 * Meeting Session capture bar (ADR-0016, plan M1).
 *
 * The transient capture surface (ADR-0016 §4): it exists only while a Session is
 * open and collapses back to nothing on stop — the durable artifact is the
 * Meeting note. M1 has no audio, so segments are pushed by hand here (the "fake
 * source") to validate the spine UI → note → stream end-to-end. M2+ replaces the
 * manual inputs with real capture; the event contract this renders does not change.
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
    }
  }, []);

  const start = useCallback(async () => {
    if (busy.current) return;
    busy.current = true;
    try {
      const res = await tauri.sessionStart(title.trim() || "Meeting", onEvent);
      setSessionId(res.sessionId);
      setNotePath(res.notePath);
      setSegments([]);
      setAttendees([]);
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

  const fmt = (ms: number) => {
    const t = Math.max(0, Math.floor(ms / 1000));
    return `${String(Math.floor(t / 60)).padStart(2, "0")}:${String(t % 60).padStart(2, "0")}`;
  };

  if (!sessionId) {
    return (
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
              <span className="font-medium">{s.speaker}:</span> {s.text}
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
          placeholder={
            asNote ? "Note alongside the meeting…" : "What the speaker said… (M1 fake source)"
          }
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
