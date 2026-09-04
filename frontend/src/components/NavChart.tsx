import { money, num } from "../lib/format";

type Run = { recorded_at?: string; nav?: string | number; outcome?: string };
type Benchmark = { label: string; points: [string, number][] };

/** The smallest drawdown the panel will draw full-height. */
const DD_FLOOR = 0.01;

/**
 * Round tick steps a reader can do arithmetic with: 1, 2, 2.5 or 5 x 10^n.
 * Auto-scaling to the exact extremes gives axes labelled 98,961 and 100,110,
 * which are two facts rather than a scale.
 */
function ticks(lo: number, hi: number, want = 4): number[] {
  if (!(hi > lo)) return [lo];
  const raw = (hi - lo) / want;
  const mag = Math.pow(10, Math.floor(Math.log10(raw)));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * mag).find((s) => s >= raw) ?? 10 * mag;
  const out: number[] = [];
  for (let v = Math.ceil(lo / step) * step; v <= hi + step / 1e6; v += step) out.push(v);
  return out;
}

/**
 * NAV as the runs recorded it, with its drawdown underneath.
 *
 * Step interpolation, deliberately: equity only changes when a run lands, and
 * a smooth slope between two runs would draw motion nothing recorded. The
 * drawdown band is the same series against its own high-water mark - the two
 * cannot disagree because they are one array read twice.
 *
 * The live NAV is carried as the final point. Without it the curve ended at the
 * last run and climbed while the headline underneath it read -$1,038 since
 * inception: two numbers for the same book, disagreeing on the same screen.
 *
 * The drawdown axis never scales tighter than one percent. Fitting the panel to
 * whatever the worst happens to be drew a 1.1 BASIS POINT dip as a full-height
 * decline under a label reading "worst -0.0%" - true, and unreadable as
 * anything but a crash.
 *
 * Under two points there is no curve to draw and the panel says so rather than
 * rendering a single dot as if it were a trend.
 */
export function NavChart({
  runs,
  initialCash,
  now,
  benchmark,
}: {
  runs: Run[];
  initialCash?: number;
  /** Mark-to-market right now, if the book reports one. */
  now?: number;
  benchmark?: Benchmark;
}) {
  const recorded = runs
    .filter((r) => r.nav != null)
    .map((r) => ({ t: r.recorded_at ?? "", v: num(r.nav), live: false }))
    .filter((p) => Number.isFinite(p.v) && p.v > 0)
    .sort((a, b) => a.t.localeCompare(b.t));

  const pts =
    now != null && Number.isFinite(now) && now > 0
      ? [...recorded, { t: "now", v: now, live: true }]
      : recorded;

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
  const EQ_H = 150;
  const DD_H = 52;
  const GUTTER = "58px";

  const base = initialCash && initialCash > 0 ? initialCash : pts[0].v;
  const vals = pts.map((p) => p.v);
  const benchmarkByTime = new Map(
    (benchmark?.points ?? []).filter(([, value]) => Number.isFinite(value) && value > 0),
  );
  const benchmarkPoints = pts.flatMap((point, index) => {
    const value = benchmarkByTime.get(point.t);
    return value == null ? [] : [{ index, value }];
  });
  const benchmarkValues = benchmarkPoints.map(({ value }) => value);
  const yTicks = ticks(
    Math.min(...vals, ...benchmarkValues, base),
    Math.max(...vals, ...benchmarkValues, base),
  );
  // Padded, so a tick at the extreme of the range sits INSIDE the box. A label
  // centred on the boundary hangs half of itself into whatever comes next.
  const inner = {
    lo: Math.min(...vals, ...benchmarkValues, base, ...yTicks),
    hi: Math.max(...vals, ...benchmarkValues, base, ...yTicks),
  };
  const pad = (inner.hi - inner.lo || Math.abs(inner.hi) * 0.01 || 1) * 0.09;
  const lo = inner.lo - pad;
  const hi = inner.hi + pad;
  const span = hi - lo;

  const x = (i: number) => (i / (pts.length - 1)) * W;
  // Fractions of the plot box, so the HTML labels and the SVG paths are placed
  // by the same arithmetic and cannot drift apart.
  const yf = (v: number) => (hi - v) / span;
  const y = (v: number) => 3 + yf(v) * (EQ_H - 6);

  const line = pts.map((p, i) => `${i ? "L" : "M"}${x(i).toFixed(1)} ${y(p.v).toFixed(1)}`).join(" ");
  const area = `${line} L${x(pts.length - 1).toFixed(1)} ${EQ_H} L${x(0).toFixed(1)} ${EQ_H} Z`;
  const benchmarkLine = benchmarkPoints
    .map(({ index, value }, i) => `${i ? "L" : "M"}${x(index).toFixed(1)} ${y(value).toFixed(1)}`)
    .join(" ");

  // Drawdown against the running peak — the number an operator actually feels.
  let peak = base;
  const dd = pts.map((p) => {
    peak = Math.max(peak, p.v);
    return p.v / peak - 1;
  });
  const worst = Math.min(...dd, 0);
  const ddSpan = Math.max(Math.abs(worst), DD_FLOOR);
  const ddTicks = ticks(-ddSpan, 0, 2).filter((v) => v < -1e-9);
  const ddBox = ddSpan * 1.12;
  const ddf = (v: number) => Math.abs(v) / ddBox;
  const ddY = (v: number) => 2 + ddf(v) * (DD_H - 4);
  const ddLine = dd.map((v, i) => `${i ? "L" : "M"}${x(i).toFixed(1)} ${ddY(v).toFixed(1)}`).join(" ");

  const last = pts[pts.length - 1].v;
  const up = last >= base;
  const stroke = up ? "#4ADE9B" : "#F4635E";
  const day = (t: string) => (t === "now" ? "now" : t.slice(5, 10));

  return (
    <div>
      <Plot gutter={GUTTER} height={EQ_H}>
        {yTicks.map((v) => (
          <Label key={v} top={yf(v)} text={money(v, 0)} />
        ))}
        <svg
          viewBox={`0 0 ${W} ${EQ_H}`}
          className="absolute inset-0 h-full w-full"
          preserveAspectRatio="none"
          aria-hidden
        >
          <defs>
            <linearGradient id="navfill" x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor={stroke} stopOpacity="0.20" />
              <stop offset="100%" stopColor={stroke} stopOpacity="0" />
            </linearGradient>
          </defs>
          {yTicks.map((v) => (
            <line
              key={v}
              x1="0"
              y1={y(v)}
              x2={W}
              y2={y(v)}
              stroke="#1C2534"
              strokeWidth="1"
              vectorEffect="non-scaling-stroke"
            />
          ))}
          {/* Inception, so the curve is read against what was put in, not its own low. */}
          <line
            x1="0"
            y1={y(base)}
            x2={W}
            y2={y(base)}
            stroke="#2A3648"
            strokeWidth="1"
            strokeDasharray="3 3"
            vectorEffect="non-scaling-stroke"
          />
          <path d={area} fill="url(#navfill)" />
          {benchmarkPoints.length >= 2 && (
            <path
              d={benchmarkLine}
              fill="none"
              stroke="#6E8BFF"
              strokeWidth="1.5"
              strokeDasharray="5 4"
              strokeLinejoin="round"
              vectorEffect="non-scaling-stroke"
            />
          )}
          <path
            d={line}
            fill="none"
            stroke={stroke}
            strokeWidth="1.75"
            strokeLinejoin="round"
            vectorEffect="non-scaling-stroke"
          />
        </svg>
        {/* The endpoint dot is HTML, not SVG: a circle in a non-uniformly
            scaled viewBox comes out an ellipse whose shape depends on the
            panel's width. */}
        <span
          className="absolute -ml-[3px] -mt-[3px] h-1.5 w-1.5 rounded-full"
          style={{ left: "100%", top: `${yf(last) * 100}%`, background: stroke }}
        />
      </Plot>

      <div className="mt-3 flex flex-wrap items-center gap-x-5 gap-y-1 text-[10.5px] text-dim">
        <Legend colour={stroke} label="Portfolio" value={last / base - 1} />
        {benchmark && benchmarkPoints.length >= 2 && (
          <Legend
            colour="#6E8BFF"
            dashed
            label={benchmark.label}
            value={benchmarkPoints[benchmarkPoints.length - 1].value / base - 1}
          />
        )}
      </div>

      <div className="mt-2 flex items-baseline justify-between">
        <span className="text-[10px] uppercase tracking-[0.14em] text-faint">Drawdown</span>
        <span className="num text-[10.5px] text-faint">
          worst {(worst * 100).toFixed(2)}% · {recorded.length} runs · from {money(base, 0)}
        </span>
      </div>

      <Plot gutter={GUTTER} height={DD_H} className="mt-1">
        <Label top={0.04} text="0%" />
        {ddTicks.map((v) => (
          <Label key={v} top={ddf(v)} text={`${(v * 100).toFixed(1)}%`} />
        ))}
        <svg
          viewBox={`0 0 ${W} ${DD_H}`}
          className="absolute inset-0 h-full w-full"
          preserveAspectRatio="none"
          aria-hidden
        >
          {ddTicks.map((v) => (
            <line
              key={v}
              x1="0"
              y1={ddY(v)}
              x2={W}
              y2={ddY(v)}
              stroke="#1C2534"
              strokeWidth="1"
              vectorEffect="non-scaling-stroke"
            />
          ))}
          <path d={`${ddLine} L${x(dd.length - 1)} 2 L${x(0)} 2 Z`} fill="#F4635E" fillOpacity="0.16" />
          <path
            d={ddLine}
            fill="none"
            stroke="#F4635E"
            strokeWidth="1.25"
            vectorEffect="non-scaling-stroke"
          />
        </svg>
      </Plot>

      <div
        className="num mt-1 flex justify-between text-[10px] text-faint"
        style={{ marginLeft: GUTTER }}
      >
        <span>{day(pts[0].t)}</span>
        <span>{day(pts[pts.length - 1].t)}</span>
      </div>
    </div>
  );
}

function Legend({
  colour,
  dashed = false,
  label,
  value,
}: {
  colour: string;
  dashed?: boolean;
  label: string;
  value: number;
}) {
  return (
    <span className="flex items-center gap-1.5">
      <span
        className="inline-block w-5 border-t-2"
        style={{ borderColor: colour, borderStyle: dashed ? "dashed" : "solid" }}
      />
      <span>{label}</span>
      <span className="num text-faint">
        {value >= 0 ? "+" : ""}{(value * 100).toFixed(2)}%
      </span>
    </span>
  );
}

/** A plot box with a label gutter, sized in pixels so the two agree. */
function Plot({
  gutter,
  height,
  className = "",
  children,
}: {
  gutter: string;
  height: number;
  className?: string;
  children: React.ReactNode;
}) {
  return (
    <div className={`relative ${className}`} style={{ height, marginLeft: gutter }}>
      {children}
    </div>
  );
}

/** An axis label, hung in the gutter to the left of the plot it belongs to. */
function Label({ top, text }: { top: number; text: string }) {
  return (
    <span
      className="num absolute -translate-y-1/2 pr-2 text-right text-[10px] text-faint"
      style={{ top: `${top * 100}%`, right: "100%", width: "58px" }}
    >
      {text}
    </span>
  );
}
