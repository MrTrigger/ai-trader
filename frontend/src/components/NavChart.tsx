import { money, num } from "../lib/format";

type Run = { recorded_at?: string; nav?: string | number; outcome?: string };

/**
 * NAV as the runs recorded it, with its drawdown underneath.
 *
 * Step interpolation, deliberately: equity only changes when a run lands, and
 * a smooth slope between two runs would draw motion nothing recorded. The
 * drawdown band is the same series against its own high-water mark — the two
 * cannot disagree because they are one array read twice.
 *
 * Under two runs there is no curve to draw and the panel says so rather than
 * rendering a single dot as if it were a trend.
 */
export function NavChart({ runs, initialCash }: { runs: Run[]; initialCash?: number }) {
  const pts = runs
    .filter((r) => r.nav != null)
    .map((r) => ({ t: r.recorded_at ?? "", v: num(r.nav) }))
    .filter((p) => Number.isFinite(p.v) && p.v > 0)
    .sort((a, b) => a.t.localeCompare(b.t));

  if (pts.length < 2) {
    // Say it in one line rather than reserving the chart's full height for a
    // blank rectangle: an empty frame reads as a broken chart, and this is
    // simply a book too young to have a shape yet.
    return (
      <p className="text-[11.5px] text-faint">
        {pts.length === 1
          ? "One run recorded — a curve needs two."
          : "Nothing has run yet."}
      </p>
    );
  }

  const W = 640;
  const EQ_H = 92;
  const DD_H = 34;
  const PAD_L = 4;
  const PAD_R = 4;

  const base = initialCash && initialCash > 0 ? initialCash : pts[0].v;
  const vals = pts.map((p) => p.v);
  const lo = Math.min(...vals, base);
  const hi = Math.max(...vals, base);
  const span = hi - lo || Math.abs(hi) * 0.01 || 1;

  const x = (i: number) => PAD_L + (i / (pts.length - 1)) * (W - PAD_L - PAD_R);
  const y = (v: number) => 4 + (hi - v) * (EQ_H - 8) / span;

  const line = pts.map((p, i) => `${i ? "L" : "M"}${x(i).toFixed(1)} ${y(p.v).toFixed(1)}`).join(" ");
  const area = `${line} L${x(pts.length - 1).toFixed(1)} ${EQ_H} L${x(0).toFixed(1)} ${EQ_H} Z`;

  // Drawdown against the running peak — the number an operator actually feels.
  let peak = base;
  const dd = pts.map((p) => {
    peak = Math.max(peak, p.v);
    return p.v / peak - 1;
  });
  const worst = Math.min(...dd, 0);
  const ddSpan = Math.abs(worst) || 0.01;
  const ddY = (v: number) => 2 + (Math.abs(v) / ddSpan) * (DD_H - 6);
  const ddPath =
    dd.map((v, i) => `${i ? "L" : "M"}${x(i).toFixed(1)} ${ddY(v).toFixed(1)}`).join(" ") +
    ` L${x(dd.length - 1).toFixed(1)} 2 L${x(0).toFixed(1)} 2 Z`;

  const last = pts[pts.length - 1].v;
  const up = last >= base;
  const stroke = up ? "#4ADE9B" : "#F4635E";

  return (
    <div>
      <svg viewBox={`0 0 ${W} ${EQ_H}`} className="w-full" preserveAspectRatio="none" aria-hidden>
        <defs>
          <linearGradient id="navfill" x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" stopColor={stroke} stopOpacity="0.20" />
            <stop offset="100%" stopColor={stroke} stopOpacity="0" />
          </linearGradient>
        </defs>
        {/* Inception, so the curve is read against what was put in, not its own low. */}
        <line
          x1={PAD_L}
          y1={y(base)}
          x2={W - PAD_R}
          y2={y(base)}
          stroke="#2A3648"
          strokeWidth="1"
          strokeDasharray="3 3"
          vectorEffect="non-scaling-stroke"
        />
        <path d={area} fill="url(#navfill)" />
        <path
          d={line}
          fill="none"
          stroke={stroke}
          strokeWidth="1.75"
          strokeLinejoin="round"
          vectorEffect="non-scaling-stroke"
        />
        <circle cx={x(pts.length - 1)} cy={y(last)} r="2.5" fill={stroke} />
      </svg>
      <div className="mt-1 flex items-baseline justify-between">
        <span className="text-[10px] uppercase tracking-[0.14em] text-faint">Drawdown</span>
        <span className="num text-[10.5px] text-faint">
          worst {(worst * 100).toFixed(1)}% · {pts.length} runs · from {money(base, 0)}
        </span>
      </div>
      <svg viewBox={`0 0 ${W} ${DD_H}`} className="w-full" preserveAspectRatio="none" aria-hidden>
        <path d={ddPath} fill="#F4635E" fillOpacity="0.16" />
        <path
          d={dd.map((v, i) => `${i ? "L" : "M"}${x(i).toFixed(1)} ${ddY(v).toFixed(1)}`).join(" ")}
          fill="none"
          stroke="#F4635E"
          strokeWidth="1.25"
          vectorEffect="non-scaling-stroke"
        />
      </svg>
    </div>
  );
}
