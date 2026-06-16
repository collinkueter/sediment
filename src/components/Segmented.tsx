import type { ReactNode } from "react";

export interface SegmentedOption<T extends string> {
  value: T;
  label: ReactNode;
}

/**
 * The one segmented pill toggle used across the app (theme picker, note
 * Read/Source, Settings appearance). A single rhythm — `bg-bg-sunk` track,
 * `bg-raised` active pill — so these controls never drift apart again.
 */
export function Segmented<T extends string>({
  value,
  onChange,
  options,
  ariaLabel,
}: {
  value: T;
  onChange: (value: T) => void;
  options: SegmentedOption<T>[];
  ariaLabel?: string;
}) {
  return (
    // biome-ignore lint/a11y/useSemanticElements: a <fieldset> brings layout/legend baggage; role="group" on the pill track is the lighter ARIA-correct choice for a toggle cluster
    <div className="inline-flex rounded-lg bg-bg-sunk p-0.5" role="group" aria-label={ariaLabel}>
      {options.map((opt) => (
        <button
          key={opt.value}
          type="button"
          aria-pressed={value === opt.value}
          onClick={() => onChange(opt.value)}
          className={`flex items-center gap-1.5 rounded-md px-2.5 py-1 text-[12px] font-medium transition-colors ${
            value === opt.value ? "bg-raised text-ink shadow-sm" : "text-muted hover:text-ink"
          }`}
        >
          {opt.label}
        </button>
      ))}
    </div>
  );
}
