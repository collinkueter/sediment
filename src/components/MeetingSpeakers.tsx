import { Icon, initials } from "@/components/icons";
import { isUnknown, speakerTone } from "@/lib/speakers";
import { useFormationStore } from "@/lib/store";
import { tauri } from "@/lib/tauri";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

/**
 * Post-meeting speaker reconciliation (ADR-0017 §6), shown atop a Meeting note.
 *
 * Reviewing a finished meeting, you reconcile who spoke. The redesign (speaker-UX
 * rethink) treats this as *attribution*, not labelling: every decision sits next to
 * its evidence. Each unidentified voice is a card with its **signature line** (the
 * longest thing it said), how often it spoke, a ▶ to hear it, and inline naming —
 * so "who was this?" is answered from the words and the voice, not a bare number.
 * Already-named speakers collapse to a quiet row; an all-named meeting collapses to
 * a one-line summary.
 */

function personName(path: string): string {
  return path.replace(/^People\//, "").replace(/\.md$/i, "");
}

type Profile = {
  /** The transcript label — the identity we rename *from* (e.g. "Unknown speaker 2"). */
  name: string;
  /** How many transcript segments this voice has. */
  count: number;
  /** Offset label of the voice's first segment ("03:12"). */
  firstOffset: string | null;
  /** The voice's longest utterance — the memorable hook for "who was this?". */
  signature: string | null;
};

/** Parse the note's `<!-- sediment:speakers … -->` block into label → suggested
 *  name. The second pass writes a borderline Voiceprint match here instead of
 *  asserting it, so the panel can offer "probably <name>" for confirmation
 *  (confirm-don't-assert, ADR-0017 §6). Invisible in the rendered note. */
function suggestionsFromNote(md: string): Map<string, string> {
  const out = new Map<string, string>();
  const body = md.match(/<!--\s*sediment:speakers\s*([\s\S]*?)-->/)?.[1];
  if (!body) return out;
  for (const raw of body.split(/\r?\n/)) {
    const m = raw.match(/^(.+?)\s*=>\s*(.+?)\s*$/);
    if (m?.[1] && m[2]) out.set(m[1].trim(), m[2].trim());
  }
  return out;
}

/** A muted but *distinct* tone for an unidentified voice, so three unknowns don't
 *  read as one grey blur. Hashes the label; named voices keep their real tone. */
const PROVISIONAL_TONES = ["var(--gold)", "var(--sage)", "var(--accent)"] as const;
function provisionalTone(name: string): string {
  let h = 0;
  for (let i = 0; i < name.length; i++) h = (h * 31 + name.charCodeAt(i)) >>> 0;
  return PROVISIONAL_TONES[h % PROVISIONAL_TONES.length] as string;
}

/** Parse `## Transcript` bullets (`` - `[mm:ss]` **Speaker:** text ``) into
 *  per-speaker profiles, unidentified voices first (where the work is), then by
 *  first appearance. Evidence is derived from the note itself — no extra command. */
function profilesFromNote(md: string, attendees: string[]): Profile[] {
  const line = /^-\s+`\[([^\]]+)\]`\s+\*\*(.+?):\*\*\s+(.*)$/;
  const acc = new Map<string, { count: number; firstOffset: string; signature: string }>();
  let inTranscript = false;
  for (const raw of md.split(/\r?\n/)) {
    if (/^##\s/.test(raw)) {
      inTranscript = /^##\s+Transcript\s*$/.test(raw);
      continue;
    }
    if (!inTranscript) continue;
    const m = line.exec(raw.trim());
    if (!m) continue;
    const [, offset, speaker, text] = m as unknown as [string, string, string, string];
    const who = speaker.trim();
    const prev = acc.get(who);
    if (!prev) {
      acc.set(who, { count: 1, firstOffset: offset, signature: text });
    } else {
      prev.count += 1;
      if (text.length > prev.signature.length) prev.signature = text;
    }
  }
  // Union with the attendee list so a speaker with no parsed line still appears.
  const names = new Set<string>([...acc.keys(), ...attendees]);
  const profiles: Profile[] = [...names].map((name) => {
    const e = acc.get(name);
    return {
      name,
      count: e?.count ?? 0,
      firstOffset: e?.firstOffset ?? null,
      signature: e?.signature ?? null,
    };
  });
  const offsetVal = (p: Profile) =>
    p.firstOffset
      ? p.firstOffset.split(":").reduce((a, b) => a * 60 + Number(b), 0)
      : Number.MAX_SAFE_INTEGER;
  return profiles.sort((a, b) => {
    const au = isUnknown(a.name) ? 0 : 1;
    const bu = isUnknown(b.name) ? 0 : 1;
    if (au !== bu) return au - bu; // unidentified voices first — that's the work
    return offsetVal(a) - offsetVal(b);
  });
}

export function MeetingSpeakers({
  notePath,
  onReload,
  focusSpeaker,
}: {
  notePath: string;
  onReload: () => Promise<void> | void;
  /** A click on a transcript speaker label (NoteViewer) — expand the panel, open
   *  that speaker's card for renaming, and scroll it into view. The nonce lets the
   *  same name re-trigger on a repeat click. */
  focusSpeaker?: { name: string; nonce: number } | null;
}) {
  const notes = useFormationStore((s) => s.notes);
  const currentNotePath = useFormationStore((s) => s.currentNotePath);
  const currentNoteContent = useFormationStore((s) => s.currentNoteContent);

  const [attendees, setAttendees] = useState<string[]>([]);
  // Names with a playable voice clip — drives whether a card shows a ▶.
  const [clipNames, setClipNames] = useState<string[]>([]);
  // The card currently being named/reassigned (inline editor open), and its draft.
  const [editing, setEditing] = useState<string | null>(null);
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // All named → collapse to a quiet summary; clicking it reveals the cards.
  const [expanded, setExpanded] = useState(false);

  // Voice-clip playback: one reused <audio> and a cache of blob URLs (revoked on
  // unmount) so a card's ▶ plays the person's sample.
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const clipCache = useRef<Map<string, string>>(new Map());
  const [playing, setPlaying] = useState<string | null>(null);
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
      const el = audioRef.current;
      el.onended = () => setPlaying(null);
      el.src = url;
      setPlaying(name);
      await el.play();
      setError(null);
    } catch (err) {
      console.error("play voice clip failed:", err);
      setError("Couldn't play that clip.");
      setPlaying(null);
    }
  }, []);

  const refresh = useCallback(() => {
    tauri
      .meetingSpeakers(notePath)
      .then(setAttendees)
      .catch(() => setAttendees([]));
    tauri
      .meetingVoiceClips(notePath)
      .then(setClipNames)
      .catch(() => setClipNames([]));
  }, [notePath]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  // Transcript markdown for this note. The panel always sits atop the open note, so
  // the store's content is it — and updates live when the second pass refines it.
  const [fallbackContent, setFallbackContent] = useState("");
  const noteContent = currentNotePath === notePath ? currentNoteContent : fallbackContent;
  useEffect(() => {
    if (currentNotePath === notePath) return;
    tauri
      .readNote(notePath)
      .then(setFallbackContent)
      .catch(() => setFallbackContent(""));
  }, [notePath, currentNotePath]);

  const profiles = useMemo(
    () => profilesFromNote(noteContent, attendees),
    [noteContent, attendees],
  );
  const suggestions = useMemo(() => suggestionsFromNote(noteContent), [noteContent]);

  // Focus a card when its speaker is clicked in the transcript (NoteViewer). Runs
  // once per click (tracked by nonce) so a later note reload doesn't reopen it.
  const containerRef = useRef<HTMLDivElement>(null);
  const handledNonce = useRef<number | null>(null);
  useEffect(() => {
    if (!focusSpeaker || focusSpeaker.nonce === handledNonce.current) return;
    handledNonce.current = focusSpeaker.nonce;
    const { name } = focusSpeaker;
    if (!profiles.some((p) => p.name === name)) return;
    setExpanded(true);
    setEditing(name);
    setError(null);
    requestAnimationFrame(() => {
      const sel = `[data-speaker="${name.replace(/["\\]/g, "\\$&")}"]`;
      containerRef.current?.querySelector(sel)?.scrollIntoView({ block: "nearest" });
    });
  }, [focusSpeaker, profiles]);

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
    async (from: string, to: string) => {
      const next = to.trim();
      setEditing(null);
      setValue("");
      if (!next || next === from) return;
      setBusy(true);
      setError(null);
      try {
        const res = await tauri.assignMeetingSpeaker(notePath, from, next);
        setAttendees(res.attendees);
        await onReload();
        refresh();
      } catch (err) {
        console.error("assign speaker failed:", err);
        setError(typeof err === "string" ? err : "Couldn't assign that speaker.");
      } finally {
        setBusy(false);
      }
    },
    [notePath, onReload, refresh],
  );

  if (profiles.length === 0) return null;

  const unknown = profiles.filter((p) => isUnknown(p.name)).length;

  // All named and not yet expanded: collapse to a quiet summary.
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
              {profiles.length} {profiles.length === 1 ? "speaker" : "speakers"} · all named
            </span>
            <Icon.ChevronRight className="h-3 w-3 text-faint transition-transform group-hover:translate-x-0.5" />
          </button>
        </div>
      </div>
    );
  }

  return (
    <div ref={containerRef} className="border-line border-b bg-surface px-4 py-3">
      <div className="mx-auto flex max-w-[42rem] flex-col gap-2">
        <div className="flex items-center justify-between">
          <span className="inline-flex items-center gap-1.5 text-[10px] font-bold uppercase tracking-[.08em] text-ink-soft">
            <Icon.Mic className="h-3.5 w-3.5 text-muted" />
            Speakers
          </span>
          {unknown > 0 ? (
            <span className="text-[11px] text-muted">
              {unknown} {unknown === 1 ? "voice" : "voices"} to name
            </span>
          ) : (
            <button
              type="button"
              onClick={() => setExpanded(false)}
              aria-label="Hide meeting speakers"
              className="text-[11px] text-muted hover:text-ink-soft"
            >
              Hide
            </button>
          )}
        </div>

        {error && <span className="text-[11px] text-danger">{error}</span>}

        {profiles.map((p) => {
          const unk = isUnknown(p.name);
          const tone = unk ? provisionalTone(p.name) : speakerTone(p.name);
          const hasClip = clipNames.includes(p.name);
          const isEditing = editing === p.name;
          const targets = people.filter((n) => n !== p.name);
          // A borderline voiceprint match the second pass recorded but didn't assert.
          const suggested = unk ? suggestions.get(p.name) : undefined;
          return (
            <div
              key={p.name}
              data-speaker={p.name}
              className={[
                "scroll-mt-2 rounded-xl border bg-raised px-3 py-2.5 shadow-sm transition-colors",
                unk ? "border-dashed border-line-strong" : "border-line",
              ].join(" ")}
            >
              <div className="flex items-center gap-2.5">
                <span
                  className="inline-grid h-7 w-7 flex-none place-items-center rounded-full text-[9px] font-bold text-white"
                  style={{ background: tone }}
                  aria-hidden
                >
                  {unk ? "?" : initials(p.name)}
                </span>
                <div className="min-w-0 flex-1">
                  <span className={unk ? "text-[13px] text-ink-soft" : "text-[13px] text-ink"}>
                    {unk ? "Unidentified voice" : p.name}
                  </span>
                  <div className="text-[11px] text-muted">
                    {p.count > 0
                      ? `spoke ${p.count} ${p.count === 1 ? "time" : "times"}${p.firstOffset ? ` · from ${p.firstOffset}` : ""}`
                      : "no transcript lines"}
                  </div>
                </div>
                {hasClip && (
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => void playClip(p.name)}
                    title="Hear this voice"
                    aria-label="Hear this voice"
                    className="inline-flex flex-none items-center gap-1 rounded-full border border-line bg-surface px-2.5 py-1 text-[11px] text-muted shadow-sm transition-colors hover:border-accent hover:text-accent-ink disabled:opacity-50"
                  >
                    <Icon.Play className="h-3 w-3" />
                    {playing === p.name ? "playing" : "hear"}
                  </button>
                )}
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => {
                    setValue("");
                    setError(null);
                    setEditing(isEditing ? null : p.name);
                  }}
                  className={[
                    "inline-flex flex-none items-center gap-1 rounded-full border px-2.5 py-1 text-[11px] shadow-sm transition-colors disabled:opacity-50",
                    unk
                      ? "border-accent bg-accent-tint text-accent-ink"
                      : "border-line bg-surface text-muted hover:border-accent hover:text-accent-ink",
                  ].join(" ")}
                >
                  {unk ? (
                    "Name"
                  ) : (
                    <>
                      <Icon.Pencil className="h-3 w-3" />
                      Reassign
                    </>
                  )}
                </button>
              </div>

              {/* Confidence cue: a borderline match, offered for one-tap confirmation
                  rather than silently asserted (ADR-0017 §6). */}
              {suggested && !isEditing && (
                <div className="mt-2 flex items-center gap-2 rounded-lg border border-accent bg-accent-tint px-2.5 py-1.5">
                  <Icon.Sparkle aria-hidden className="h-3.5 w-3.5 shrink-0 text-accent-ink" />
                  <span className="min-w-0 flex-1 text-[12px] text-accent-ink">
                    Probably <span className="font-semibold">{suggested}</span> · by voice match
                  </span>
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => void assign(p.name, suggested)}
                    className="shrink-0 rounded-full bg-accent px-2.5 py-1 text-[11px] font-medium text-white hover:bg-accent-ink disabled:opacity-50"
                  >
                    Confirm {suggested}
                  </button>
                </div>
              )}

              {/* Evidence: the longest thing this voice said — the "who was this?" hook. */}
              {p.signature && (
                <p className="mt-2 border-line border-l-2 pl-2.5 font-serif text-[12.5px] leading-snug text-ink-soft">
                  “{p.signature}”
                </p>
              )}

              {/* Inline naming — "Self" first (it's you), then the people you know,
                  or type a new one. */}
              {isEditing && (
                <div className="mt-2.5 border-line border-t pt-2.5">
                  {(p.name !== "Self" || targets.length > 0) && (
                    <div className="mb-2 flex flex-wrap gap-1.5">
                      {p.name !== "Self" && (
                        <button
                          type="button"
                          onClick={() => void assign(p.name, "Self")}
                          className="inline-flex items-center gap-1.5 rounded-full border border-accent bg-accent-tint px-2.5 py-1 text-[12px] text-accent-ink hover:border-accent"
                        >
                          <span
                            className="inline-grid h-[16px] w-[16px] place-items-center rounded-full text-[8px] font-bold text-white"
                            style={{ background: speakerTone("Self") }}
                            aria-hidden
                          >
                            {initials("Self")}
                          </span>
                          That's me
                        </button>
                      )}
                      {targets.map((name) => (
                        <button
                          key={name}
                          type="button"
                          onClick={() => void assign(p.name, name)}
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
                  <div className="flex items-center gap-2">
                    <input
                      // biome-ignore lint/a11y/noAutofocus: an editor opened on intent should focus its field
                      autoFocus
                      value={value}
                      onChange={(e) => setValue(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") void assign(p.name, value);
                        else if (e.key === "Escape") setEditing(null);
                      }}
                      placeholder="Name this person…"
                      className="min-w-0 flex-1 rounded-md border border-line bg-surface px-2.5 py-1.5 text-[13px] text-ink placeholder:text-faint focus:border-accent-ink focus:outline-none"
                    />
                    <button
                      type="button"
                      onClick={() => void assign(p.name, value)}
                      disabled={!value.trim()}
                      className="shrink-0 rounded-md bg-accent px-3 py-1.5 text-[12px] font-medium text-white hover:bg-accent-ink disabled:opacity-40"
                    >
                      Name
                    </button>
                  </div>
                  <p className="mt-1.5 text-[10px] leading-snug text-faint">
                    Relabels the transcript and gives them a note in People. Skip anyone — they stay
                    unnamed, nothing breaks.
                  </p>
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
