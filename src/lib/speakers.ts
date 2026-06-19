// Speaker "strata" colours (ADR-0017) — shared by the live capture bar and the
// post-meeting speaker panel so a voice keeps its colour everywhere. You are
// always the accent; unknown speakers stay neutral until named. Warm Strata hues.

const SPEAKER_TONES = ["var(--sage)", "var(--gold)", "var(--accent)", "var(--accent-ink)"] as const;

/** A speaker label not yet attributed to a person ("Unknown speaker 2"). */
export function isUnknown(name: string): boolean {
  return /^Unknown speaker/i.test(name);
}

/** A deterministic Strata colour for a speaker, stable across renders. */
export function speakerTone(name: string): string {
  if (name === "Self" || name === "You") return "var(--accent)";
  if (isUnknown(name)) return "var(--faint)";
  let h = 0;
  for (let i = 0; i < name.length; i++) h = (h * 31 + name.charCodeAt(i)) >>> 0;
  return SPEAKER_TONES[h % SPEAKER_TONES.length] as string;
}
