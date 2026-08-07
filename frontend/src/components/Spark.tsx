/** A cumulative line, drawn small. No axes, no grid: at this size the shape
 *  is the only readable signal, and decoration would outweigh it. */
export function Spark({ points, tone = "#6E8BFF" }: { points: number[]; tone?: string }) {
  if (points.length < 2) {
    return (
      <div className="flex h-10 w-full items-center text-[11px] text-faint">
        a curve needs two points — this one has {points.length}
      </div>
    );
  }
  const w = 260, h = 40, pad = 2;
  const lo = Math.min(...points), hi = Math.max(...points);
  const span = hi - lo || 1;
  const d = points
    .map((p, i) => {
      const x = pad + (i / (points.length - 1)) * (w - pad * 2);
      const y = h - pad - ((p - lo) / span) * (h - pad * 2);
      return `${i ? "L" : "M"}${x.toFixed(1)} ${y.toFixed(1)}`;
    })
    .join(" ");
  const last = points[points.length - 1];
  const cx = w - pad, cy = h - pad - ((last - lo) / span) * (h - pad * 2);
  return (
    <svg viewBox={`0 0 ${w} ${h}`} className="h-10 w-full" preserveAspectRatio="none" aria-hidden>
      <path d={d} fill="none" stroke={tone} strokeWidth="1.5" vectorEffect="non-scaling-stroke" />
      <circle cx={cx} cy={cy} r="2" fill={tone} />
    </svg>
  );
}
