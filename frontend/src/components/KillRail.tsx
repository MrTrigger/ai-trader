import { money, num } from "../lib/format";

/**
 * Distance to the kill line — deliberately NOT a progress bar.
 *
 * A progress bar fills toward something you want. This fills toward the
 * threshold that stops the book, so it reads from a safe left edge toward a
 * hard stop at the right, and the marker is the thing that moves. The bar
 * only takes colour as it approaches: a quiet rail is a quiet day.
 */
export function KillRail({
  rolling,
  limit,
  sessions,
  compact = false,
}: {
  rolling?: number;
  limit?: number;
  sessions?: number;
  compact?: boolean;
}) {
  const r = num(rolling);
  const l = num(limit);
  const frac = l !== 0 ? Math.min(Math.max(Math.abs(r / l), 0), 1) : 0;
  const near = frac >= 0.8 ? "alarm" : frac >= 0.5 ? "consequence" : "go";
  const colour = near === "alarm" ? "bg-alarm" : near === "consequence" ? "bg-consequence" : "bg-go/70";

  return (
    <div>
      {!compact && (
        <div className="mb-2 flex items-baseline justify-between">
          <span className="eyebrow">Kill line · rolling {sessions ?? 0} sessions</span>
          <span className="num text-[12px] text-dim">
            <span className={frac >= 0.5 ? "text-consequence" : "text-ink"}>{money(r, 0)}</span>
            <span className="text-faint"> of {money(l, 0)}</span>
          </span>
        </div>
      )}
      <div className="relative h-[6px] rounded-full bg-sunk ring-1 ring-inset ring-line">
        <div className={`h-full rounded-full ${colour} transition-[width] duration-500`} style={{ width: `${frac * 100}%` }} />
        {/* the line itself: a hard stop, not a gradient fade */}
        <div className="absolute right-0 top-1/2 h-3 w-[2px] -translate-y-1/2 rounded bg-alarm/80" />
      </div>
      {!compact && (
        <p className="mt-2 text-[11px] leading-relaxed text-faint">
          Breaching the line halts every sleeve. The bot stops itself; nobody has to be awake.
        </p>
      )}
    </div>
  );
}
