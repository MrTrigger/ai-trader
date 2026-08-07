import type { ReactNode } from "react";

export function Card({
  title,
  aside,
  children,
  className = "",
}: {
  title?: string;
  aside?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section className={`card ${className}`}>
      {title && (
        <header className="flex items-baseline gap-3 border-b border-line px-4 py-3">
          <h2 className="eyebrow">{title}</h2>
          {aside && <span className="text-[11px] text-faint">{aside}</span>}
        </header>
      )}
      <div className="p-4">{children}</div>
    </section>
  );
}

/** A pulse whose life is the heartbeat's: it fades as the beat goes stale,
 *  so a dead process looks dead rather than merely unlabelled. */
export function Heart({ age }: { age?: number | null }) {
  const stale = (age ?? 0) > 900;
  return (
    <span className="inline-flex items-center gap-1.5">
      <span
        className={`h-[7px] w-[7px] rounded-full ${stale ? "bg-faint" : "bg-go animate-heart"}`}
        aria-hidden
      />
      <span className={`num text-[11px] ${stale ? "text-consequence" : "text-dim"}`}>{fmtAge(age)}</span>
    </span>
  );
}

function fmtAge(s?: number | null) {
  if (s == null) return "—";
  if (s < 90) return `${Math.round(s)}s`;
  if (s < 5400) return `${Math.round(s / 60)}m`;
  return `${(s / 3600).toFixed(1)}h`;
}

export function Pill({
  tone = "quiet",
  children,
}: {
  tone?: "quiet" | "go" | "alarm" | "consequence";
  children: ReactNode;
}) {
  const map = {
    quiet: "border-line2 text-dim",
    go: "border-go/40 text-go bg-go/5",
    alarm: "border-alarm/45 text-alarm bg-alarm/5",
    consequence: "border-consequence/45 text-consequence bg-consequence/5",
  } as const;
  return (
    <span className={`rounded-md border px-2 py-[3px] font-display text-[11px] font-semibold uppercase tracking-wide ${map[tone]}`}>
      {children}
    </span>
  );
}
