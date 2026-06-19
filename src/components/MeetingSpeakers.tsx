import { Icon, initials } from "@/components/icons";
import { isUnknown, speakerTone } from "@/lib/speakers";
import { useFormationStore } from "@/lib/store";
import { tauri } from "@/lib/tauri";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

/**
 * Post-meeting speaker panel (ADR-0017 §6), shown atop a Meeting note.
 *
 * Reviewing a finished meeting, you reconcile who spoke: each distinct speaker is a
 * chip; clicking one opens an assign popover offering the people you already know
 * (existing `People/` notes) plus a free-text field. Assigning relabels the
 * transcript + attendees in the note and gives that person their own file — so
 * "Unknown speaker 2" becomes `[[Sarah Chen]]`, linked to `People/Sarah Chen.md`.
 */

function personName(path: string): string {
  return path.replace(/^People\//, "").replace(/\.md$/i, "");
}

export function MeetingSpeakers({
  notePath,
  onReload,
}: {
  notePath: string;
  onReload: () => Promise<void> | void;
}) {
  const notes = useFormationStore((s) => s.notes);
  const [speakers, setSpeakers] = useState<string[]>([]);
  // Names with a playable voice clip — drives whether a chip shows a ▶ (never show a
  // play control that would do nothing).
  const [clipNames, setClipNames] = useState<string[]>([]);
  const [assigning, setAssigning] = useState<{ from: string; x: number; y: number } | null>(null);
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // When every speaker is already named there's no reconciliation work, so the
  // band collapses to a quiet one-line summary; clicking it reveals the chips.
  const [expanded, setExpanded] = useState(false);

  // Voice-clip playback (ADR-0017 §6): one reused <audio> element and a cache of
  // blob URLs (revoked on unmount) so a chip's ▶ plays the person's sample.
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const clipCache = useRef<Map<string, string>>(new Map());
  useEffect(() => {
    const cache = clipCache.current;
    return () => {
      for (const url of cache.values()) URL.revokeObjectURL(url);
    };
  }, []);

  const playClip = useCallback(async (name: string) => {
    try {
      let url = clipCache.current.get(name);
      if (!url) {
        const bytes = await tauri.readVoiceClip(name);
        if (!bytes || bytes.length === 0) {
          setError(`No voice clip for ${name} yet.`);
          return;
        }
        url = URL.createObjectURL(new Blob([new Uint8Array(bytes)], { type: "audio/wav" }));
        clipCache.current.set(name, url);
      }
      if (!audioRef.current) audioRef.current = new Audio();
      audioRef.current.src = url;
      await audioRef.current.play();
      setError(null);
    } catch (err) {
      console.error("play voice clip failed:", err);
      setError("Couldn't play that clip.");
    }
  }, []);

  const refresh = useCallback(() => {
    tauri
      .meetingSpeakers(notePath)
      .then(setSpeakers)
      .catch(() => setSpeakers([]));
    tauri
      .meetingVoiceClips(notePath)
      .then(setClipNames)
      .catch(() => setClipNames([]));
  }, [notePath]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  // People you already know — the quick-pick targets, minus whoever's being named.
  const people = useMemo(
    () =>
      notes
        .filter((n) => n.relative_path.startsWith("People/"))
        .map((n) => personName(n.relative_path))
        .sort((a, b) => a.localeCompare(b)),
    [notes],
  );

  const assign = useCallback(
    async (to: string) => {
      if (!assigning) return;
      const from = assigning.from;
      const next = to.trim();
      setAssigning(null);
      setValue("");
      if (!next || next === from) return;
      setBusy(true);
      setError(null);
      try {
        const res = await tauri.assignMeetingSpeaker(notePath, from, next);
        setSpeakers(res.attendees);
        await onReload();
        refresh(); // re-sync clip availability (a renamed speaker may now carry one)
      } catch (err) {
        console.error("assign speaker failed:", err);
        setError(typeof err === "string" ? err : "Couldn't assign that speaker.");
      } finally {
        setBusy(false);
      }
    },
    [assigning, notePath, onReload, refresh],
  );

  if (speakers.length === 0) return null;

  const unknown = speakers.filter(isUnknown).length;
  const targets = assigning ? people.filter((p) => p !== assigning.from) : [];

  // All named and not yet expanded: collapse to a quiet summary. The full
  // interactive band is reserved for when there's naming work (unknown > 0).
  if (unknown === 0 && !expanded) {
    return (
      <div className="border-line border-b bg-surface px-4 py-2">
        <div className="mx-auto max-w-[42rem]">
          <button
            type="button"
            onClick={() => setExpanded(true)}
            aria-label="Show meeting speakers"
            aria-expanded={false}
            className="group inline-flex items-center gap-1.5 text-[11px] text-muted transition-colors hover:text-ink-soft"
          >
            <Icon.Mic className="h-3.5 w-3.5 text-faint" />
            <span>
              {speakers.length} {speakers.length === 1 ? "speaker" : "speakers"} · all assigned
            </span>
            <Icon.ChevronRight className="h-3 w-3 text-faint transition-transform group-hover:translate-x-0.5" />
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="border-line border-b bg-surface px-4 py-2">
      <div className="mx-auto flex max-w-[42rem] flex-wrap items-center gap-2">
        {unknown === 0 ? (
          <button
            type="button"
            onClick={() => setExpanded(false)}
            aria-label="Hide meeting speakers"
            aria-expanded={true}
            className="inline-flex shrink-0 items-center gap-1.5 text-[10px] font-bold uppercase tracking-[.08em] text-ink-soft transition-colors hover:text-ink"
          >
            <Icon.Mic className="h-3.5 w-3.5 text-muted" />
            Speakers
            <Icon.ChevronDown className="h-3 w-3 text-faint" />
          </button>
        ) : (
          <span className="inline-flex shrink-0 items-center gap-1.5 text-[10px] font-bold uppercase tracking-[.08em] text-ink-soft">
            <Icon.Mic className="h-3.5 w-3.5 text-muted" />
            Speakers
          </span>
        )}

        {speakers.map((name) => {
          const unk = isUnknown(name);
          return (
            <span key={name} className="inline-flex items-center gap-1">
              <button
                type="button"
                disabled={busy}
                onClick={(e) => {
                  const r = e.currentTarget.getBoundingClientRect();
                  setValue("");
                  setAssigning({ from: name, x: r.left, y: r.bottom + 6 });
                }}
                title={unk ? `Assign ${name} to a person` : `Reassign ${name}`}
                className={[
                  "group inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-[12.5px] shadow-sm transition-[border-color,transform] duration-150",
                  "hover:-translate-y-px hover:border-accent disabled:opacity-50",
                  unk ? "border-line-strong border-dashed bg-bg-sunk" : "border-line bg-raised",
                ].join(" ")}
              >
                <span
                  className="inline-grid h-[18px] w-[18px] flex-none place-items-center rounded-full text-[9px] font-bold text-white"
                  style={{ background: speakerTone(name) }}
                  aria-hidden
                >
                  {unk ? "?" : initials(name)}
                </span>
                <span className={unk ? "text-muted" : "text-ink"}>{name}</span>
                <Icon.Pencil className="h-3 w-3 text-faint opacity-0 transition-opacity group-hover:opacity-100" />
              </button>
              {/* Hear this person's voice — only when a clip actually exists, so the
                  ▶ is never a control that does nothing (ADR-0017 §6). */}
              {!unk && clipNames.includes(name) && (
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => void playClip(name)}
                  title={`Hear ${name}'s voice`}
                  aria-label={`Hear ${name}'s voice`}
                  className="grid h-[26px] w-[26px] flex-none place-items-center rounded-full border border-line bg-raised text-muted shadow-sm transition-colors hover:border-accent hover:text-accent-ink disabled:opacity-50"
                >
                  <Icon.Play className="h-3 w-3" />
                </button>
              )}
            </span>
          );
        })}

        {unknown > 0 && !error && <span className="text-[11px] text-muted">{unknown} to name</span>}
        {error && <span className="truncate text-[11px] text-danger">{error}</span>}

        {/* Assign popover */}
        {assigning && (
          <>
            <button
              type="button"
              aria-label="Close"
              className="fixed inset-0 z-40 cursor-default"
              onClick={() => setAssigning(null)}
            />
            <div
              className="fixed z-50 w-64 rounded-lg border border-line-strong bg-raised p-3 shadow-2xl"
              style={{
                left: Math.min(assigning.x, window.innerWidth - 268),
                top: Math.min(assigning.y, window.innerHeight - 200),
              }}
            >
              <p className="mb-2 text-[10px] font-bold uppercase tracking-[.08em] text-muted">
                Assign {assigning.from} to…
              </p>
              {targets.length > 0 && (
                <div className="mb-2 flex max-h-32 flex-wrap gap-1.5 overflow-y-auto">
                  {targets.map((name) => (
                    <button
                      key={name}
                      type="button"
                      onClick={() => void assign(name)}
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
                // biome-ignore lint/a11y/noAutofocus: a popover opened on intent should focus its field
                autoFocus
                value={value}
                onChange={(e) => setValue(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void assign(value);
                  else if (e.key === "Escape") setAssigning(null);
                }}
                placeholder="New person…"
                className="w-full rounded-md border border-line bg-surface px-2.5 py-1.5 text-[13px] text-ink placeholder:text-faint focus:border-accent-ink focus:outline-none"
              />
              <p className="mt-1.5 text-[10px] leading-snug text-faint">
                Relabels the transcript and gives them a note in People.
              </p>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
