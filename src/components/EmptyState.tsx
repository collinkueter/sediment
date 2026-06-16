import type { ComponentType, ReactNode, SVGProps } from "react";

/**
 * The one resting-state block — a soft icon, a serif line, and a muted
 * sentence. Used wherever a surface is empty (no note open, fresh
 * conversation) so the app's quiet moments read with one editorial voice.
 */
export function EmptyState({
  icon: IconComp,
  title,
  description,
}: {
  icon?: ComponentType<SVGProps<SVGSVGElement>>;
  title: string;
  description?: ReactNode;
}) {
  return (
    <div className="flex h-full w-full flex-col items-center justify-center px-6 text-center">
      {IconComp && (
        <span className="mb-3.5 grid h-11 w-11 place-items-center rounded-full bg-bg-sunk text-faint">
          <IconComp className="h-5 w-5" />
        </span>
      )}
      <h2 className="font-serif text-[18px] font-semibold text-ink-soft">{title}</h2>
      {description && (
        <p className="mt-1.5 max-w-[19rem] text-[13px] leading-relaxed text-muted">{description}</p>
      )}
    </div>
  );
}
