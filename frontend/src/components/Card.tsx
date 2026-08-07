import { useEffect, useState, type ReactNode } from "react";

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

/**
 * A heartbeat that keeps counting between polls.
 *
 * A number that only moves when the page refreshes cannot tell you the
 * process died — it just looks like a slightly old number. This one ticks
 * every second from the age the server reported, so a stopped bot visibly
 * climbs instead of sitting still, and the pulse stops when it goes stale.
 */
export function Heart({ age }: { age?: number | null }) {
  const [extra, setExtra] = useState(0);
  useEffect(() => {
    setExtra(0);
    const t = setInterval(() => setExtra((n) => n + 1), 1000);
    return () => clearInterval(t);
  }, [age]);

  if (age == null) return <span className="num text-[11px] text-faint">no heartbeat</span>;
  const live = age + extra;
  const stale = live > 900;
  return (
    <span className="inline-flex items-center gap-1.5" title="time since the bot last published state">
      <span
        className={`h-[7px] w-[7px] rounded-full ${stale ? "bg-alarm" : "bg-go animate-heart"}`}
        aria-hidden
      />
      <span className={`num text-[11px] ${stale ? "text-alarm" : "text-dim"}`}>{fmtAge(live)}</span>
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
