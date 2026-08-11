import { useEffect } from "react";
import { AssetName } from "./AssetName";
import { money, num, signed, stamp, tone } from "../lib/format";

type Order = {
  asset?: string;
  side?: string;
  qty?: string | number;
  reason?: string;
  error?: string | null;
  client_order_id?: string;
  venue_order_id?: string | null;
};

type Slice = {
  index?: number;
  of?: number;
  send_at?: string;
  execution?: {
    started_at?: string;
    submitted?: Order[];
    not_attempted?: string[];
    rounded_out?: string[];
    halted_reason?: string | null;
    reconciled?: boolean;
    paused?: boolean;
  };
};

type BookMark = { asset: string; qty: string; mark: string; entry?: string | null };

export type Run = {
  run_id?: string;
  plan_id?: string;
  as_of?: string;
  recorded_at?: string;
  outcome?: string;
  detail?: string | null;
  orders_planned?: number;
  orders_submitted?: number;
  orders_skipped?: number;
  slices_completed?: number;
  slices_planned?: number;
  nav?: string | number;
  gross_exposure?: string | number;
  net_exposure?: string | number;
  control_state?: string;
  risk_checks?: { name: string; limit: string; value: string; passed: boolean; detail?: string | null }[];
  slices?: Slice[];
  book_after?: BookMark[];
  result?: {
    settled_at?: string;
    hours?: number;
    nav_start?: string;
    nav_end?: string;
    pnl?: string;
    return_pct?: number;
    unattributed?: string;
    contributors?: { asset: string; qty: string; mark_start: string; mark_end: string; pnl: string }[];
  } | null;
};

/**
 * Everything one decision did, and what let it.
 *
 * The row said "EXECUTED" and the record behind it held the risk checks, every
 * order with the reason it existed, and which of them never went out — all of
 * it already on the page and thrown away. An operator asking "why did it do
 * that" should not have to read a run store on a cluster.
 *
 * Orders are grouped by intent rather than listed flat, because the useful
 * question is not "what did it trade" but "what CHANGED": a book that opened
 * three names and closed one is a different animal from one that trimmed
 * twenty, and the two look identical as a list of fills.
 */
export function RunModal({
  run,
  before,
  venue,
  onClose,
}: {
  run: Run;
  /** The run immediately before this one, where there is one. */
  before?: Run;
  /** Which venue this bot trades on, so a symbol can link to its chart. */
  venue?: string;
  onClose: () => void;
}) {
  useEffect(() => {
    const esc = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    document.addEventListener("keydown", esc);
    return () => document.removeEventListener("keydown", esc);
  }, [onClose]);

  const slices = run.slices ?? [];
  const orders = slices.flatMap((s) => s.execution?.submitted ?? []);
  const failed = orders.filter((o) => o.error);
  const notAttempted = slices.flatMap((s) => s.execution?.not_attempted ?? []);
  const roundedOut = slices.flatMap((s) => s.execution?.rounded_out ?? []);
  const halted = slices.map((s) => s.execution?.halted_reason).find(Boolean);
  const reconciled = slices.every((s) => s.execution?.reconciled !== false);

  // One row per ASSET, not per order. A plan is executed in slices, so an
  // eight-name rebalance submits sixteen orders and listing them flat showed
  // every position twice - which reads as the book having traded it twice.
  const byAsset = new Map<string, { o: Order; qty: number; parts: number }>();
  for (const o of orders) {
    const k = `${o.reason ?? ""}|${o.asset ?? ""}|${o.side ?? ""}`;
    const prev = byAsset.get(k);
    if (prev) {
      prev.qty += num(o.qty);
      prev.parts += 1;
      if (o.error) prev.o = o;
    } else {
      byAsset.set(k, { o, qty: num(o.qty), parts: 1 });
    }
  }
  const merged = [...byAsset.values()];

  // Intent, not direction. "Buy" tells you nothing about whether the book grew.
  const groups = [
    { key: "Entry", label: "Opened", hint: "positions this run created" },
    { key: "Increase", label: "Added to", hint: "existing positions made larger" },
    { key: "Reduce", label: "Trimmed", hint: "existing positions made smaller" },
    { key: "Exit", label: "Closed", hint: "positions this run ended" },
  ].map((g) => ({ ...g, rows: merged.filter((m) => (m.o.reason ?? "") === g.key) }));
  const untouched = groups.every((g) => g.rows.length === 0);

  // What this run did to each name, from the reason the executor stamped on
  // the order. A position in the closing book with no order was simply carried.
  const intent = new Map<string, string>();
  for (const m of merged) if (m.o.asset) intent.set(m.o.asset, m.o.reason ?? "");
  const changeOf = (asset: string): keyof typeof CHANGE => {
    switch (intent.get(asset)) {
      case "Entry":
        return "new";
      case "Increase":
        return "added";
      case "Reduce":
        return "trimmed";
      case "Exit":
        return "closed";
      default:
        return "held";
    }
  };

  const nav = num(run.nav);
  const settled = !!run.result;
  const endMark = new Map<string, { mark: number; pnl: number }>();
  for (const c of run.result?.contributors ?? []) {
    endMark.set(c.asset, { mark: num(c.mark_end), pnl: num(c.pnl) });
  }

  // What the run before this one left, so a held or trimmed name can say what
  // it was held or trimmed FROM. A weight of -10% means one thing after a trim
  // from -14% and the opposite after a build from -6%, and the row alone
  // cannot tell those apart.
  const beforeNav = num(before?.nav);
  const wasOf = new Map<string, { qty: number; weight: number | null; pnl: number | null }>();
  for (const b of before?.book_after ?? []) {
    const qty = num(b.qty);
    const notional = qty * num(b.mark);
    wasOf.set(b.asset, {
      qty,
      weight: beforeNav ? (notional / beforeNav) * 100 : null,
      pnl: null,
    });
  }
  for (const c of before?.result?.contributors ?? []) {
    const w = wasOf.get(c.asset);
    if (w) w.pnl = num(c.pnl);
  }

  type Row = {
    asset: string;
    change: keyof typeof CHANGE;
    qty: number;
    entry: number | null;
    mark: number;
    notional: number;
    weight: number | null;
    markEnd: number | null;
    pnl: number | null;
    was: { qty: number; weight: number | null; pnl: number | null } | null;
  };
  // Runs recorded before the closing book was kept have nothing to list but
  // their exits, and a four-row table under "the book this run left" would
  // read as a four-name book. Say which it is.
  const noBook = (run.book_after ?? []).length === 0;
  const rows: Row[] = (run.book_after ?? []).map((b) => {
    const qty = num(b.qty);
    const mark = num(b.mark);
    const notional = qty * mark;
    const close = endMark.get(b.asset);
    const change = changeOf(b.asset);
    return {
      asset: b.asset,
      change,
      qty,
      entry: b.entry == null ? null : num(b.entry),
      mark,
      notional,
      weight: nav ? (notional / nav) * 100 : null,
      markEnd: close?.mark ?? null,
      pnl: close?.pnl ?? null,
      // Only where it answers something. A name this run opened was not held
      // before, so "from nothing" is noise on every new row.
      was: change === "held" || change === "trimmed" ? (wasOf.get(b.asset) ?? null) : null,
    };
  });
  // What this run REMOVED: held before, gone now. Taken from the two books
  // rather than from Exit orders, because a name can leave the book without
  // this run's record naming an exit — and "what did it drop" is the first
  // question asked of a rebalance, so it must not depend on which of two
  // sources happens to have the answer.
  const held = new Set(rows.filter((r) => r.qty !== 0).map((r) => r.asset));
  for (const [asset, was] of wasOf) {
    if (was.qty === 0 || held.has(asset)) continue;
    const existing = rows.find((r) => r.asset === asset);
    if (existing) {
      existing.change = "closed";
      existing.was = was;
      continue;
    }
    rows.push({
      asset,
      change: "closed",
      qty: 0,
      entry: null,
      // A name absent from the closing book has no mark of its own there; the
      // last price it was carried at is the honest one to show.
      mark: 0,
      notional: 0,
      weight: 0,
      markEnd: null,
      pnl: null,
      was,
    });
  }
  // An exit this run submitted, for a book old enough to have no previous
  // record to difference against.
  for (const m of merged) {
    if ((m.o.reason ?? "") !== "Exit" || !m.o.asset) continue;
    if (rows.some((r) => r.asset === m.o.asset)) continue;
    rows.push({
      asset: m.o.asset,
      change: "closed",
      qty: 0,
      entry: null,
      mark: 0,
      notional: 0,
      weight: 0,
      markEnd: null,
      pnl: null,
      was: wasOf.get(m.o.asset) ?? null,
    });
  }
  // Signed, so the book reads top to bottom as longest to shortest and the two
  // sides are two blocks rather than interleaved by size. Names this run
  // removed sit below both, ordered by the weight they used to carry: they are
  // not part of the book, and sorting them into the middle at zero would put
  // them exactly where the sign changes.
  rows.sort((a, b) => {
    const closed = (r: Row) => (r.change === "closed" && r.qty === 0 ? 1 : 0);
    if (closed(a) !== closed(b)) return closed(a) - closed(b);
    if (closed(a)) return Math.abs(b.was?.weight ?? 0) - Math.abs(a.was?.weight ?? 0);
    return (b.weight ?? 0) - (a.weight ?? 0);
  });

  // The book the run actually left, which is not the book it aimed at. The
  // recorded gross and net are the PLAN's — target weights, checked by the
  // risk layer before anything traded. A turnover budget that defers weight
  // lands somewhere else, and nothing on this page used to say where.
  const longPct = rows.reduce((a, r) => a + Math.max(r.weight ?? 0, 0), 0);
  const shortPct = rows.reduce((a, r) => a + Math.max(-(r.weight ?? 0), 0), 0);
  const sided = longPct > 0 && shortPct > 0;

  const failedChecks = (run.risk_checks ?? []).filter((c) => !c.passed);

  return (
    <div
      className="fixed inset-0 z-50 flex items-start justify-center overflow-y-auto bg-black/70 p-4 sm:p-8"
      onClick={onClose}
    >
      <div
        className="card w-full max-w-4xl"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label="Run detail"
      >
        <header className="flex flex-wrap items-baseline gap-x-3 gap-y-1 border-b border-line px-4 py-3">
          <h2 className="eyebrow">Run</h2>
          <span className="num text-[12px] text-ink">{stamp(run.recorded_at).slice(0, 16)}</span>
          <span className={`text-[11px] ${run.outcome === "executed" ? "text-go" : "text-consequence"}`}>
            {String(run.outcome ?? "").toUpperCase()}
          </span>
          <span className="text-[11px] text-faint">
            decided {stamp(run.as_of).slice(0, 16)}
          </span>
          <button onClick={onClose} className="ml-auto text-[11px] text-dim hover:text-ink">
            close ·  esc
          </button>
        </header>

        <div className="space-y-5 p-4">
          {run.detail && (
            <p className="rounded border border-consequence/40 bg-consequence/5 p-3 text-[12px] leading-relaxed text-consequence">
              {run.detail}
            </p>
          )}
          {halted && (
            <p className="rounded border border-alarm/40 bg-alarm/5 p-3 text-[12px] leading-relaxed text-alarm">
              {halted}
            </p>
          )}

          <div className="grid grid-cols-2 gap-x-6 gap-y-3 sm:grid-cols-3 lg:grid-cols-6">
            <Stat k="NAV" v={money(run.nav ?? 0)} />
            <Stat
              k="Gross"
              v={`${(longPct + shortPct).toFixed(1)}%`}
              sub={`target ${pct(run.gross_exposure)}`}
            />
            <Stat
              k="Net"
              v={`${(longPct - shortPct).toFixed(1)}%`}
              sub={`target ${pct(run.net_exposure)}`}
              alarm={Math.abs(longPct - shortPct) > 5}
            />
            <Stat
              k="Long / short"
              v={`${longPct.toFixed(1)}% / ${shortPct.toFixed(1)}%`}
              sub={sided ? `${(longPct / shortPct).toFixed(2)}\u00d7` : undefined}
              alarm={sided && (longPct / shortPct < 0.8 || longPct / shortPct > 1.25)}
            />
            <Stat
              k="Positions moved"
              v={String(merged.length)}
            />
            <Stat
              k="Orders"
              v={String(run.orders_submitted ?? 0)}
              sub={`${run.slices_completed ?? 0}/${run.slices_planned ?? 0} slices`}
            />
          </div>

          {run.result ? (
            <div className="rounded border border-line2 p-3">
              <div className="flex flex-wrap items-baseline justify-between gap-3">
                <div>
                  <p className="eyebrow mb-1">
                    What it earned{" "}
                    <span className="text-faint">
                      · the {fmtHours(run.result.hours)} this run's book was held
                    </span>
                  </p>
                  <p className={`num text-[22px] leading-none ${tone(run.result.pnl)}`}>
                    {signed(run.result.pnl ?? 0)}
                  </p>
                </div>
                <div className="flex gap-6 text-right">
                  <Stat k="Return" v={`${((run.result.return_pct ?? 0) * 100).toFixed(2)}%`} />
                  <Stat k="NAV" v={`${money(run.result.nav_start ?? 0)} → ${money(run.result.nav_end ?? 0)}`} />
                </div>
              </div>
              {/* Per-name attribution lives in the book table below, against the
                  position that earned it — a separate chip strip said the same
                  numbers twice and neither copy showed the size behind them. */}
              {Math.abs(num(run.result.unattributed)) > 0.005 && (
                <p className="mt-2 text-[11px] text-faint">
                  {signed(run.result.unattributed ?? 0)} unattributed — fees, and anything opened or
                  closed inside the period rather than held across it.
                </p>
              )}
            </div>
          ) : (
            <p className="rounded border border-line2 p-3 text-[11.5px] text-faint">
              Not settled yet. The period this run opened closes when the next one runs; the result
              is written then.
            </p>
          )}

          <div>
            <p className="eyebrow mb-2">
              The book this run left{" "}
              <span className="text-faint">
                {noBook
                  ? "· closing book not recorded — only what changed"
                  : `· ${rows.filter((r) => r.qty !== 0).length} names, marked when it finished`}
              </span>
            </p>
            {rows.length === 0 ? (
              <p className="text-[12px] text-faint">
                {untouched
                  ? "No orders — the book already matched the target."
                  : "No closing book was recorded for this run."}
              </p>
            ) : (
              <div className="-mx-1 overflow-x-auto px-1">
                <table className="w-full min-w-[740px] text-[12px]">
                  <thead>
                    <tr className="text-[10px] uppercase tracking-[0.1em] text-faint">
                      <th className="pb-1 text-left font-normal">Asset</th>
                      <th className="pb-1 text-left font-normal">Change</th>
                      <th className="pb-1 text-right font-normal">Qty</th>
                      <th className="pb-1 text-right font-normal">Weight</th>
                      <th className="pb-1 text-right font-normal">Entry price</th>
                      <th className="pb-1 text-right font-normal">Current price</th>
                      {settled && <th className="pb-1 text-right font-normal">Close price</th>}
                      {settled && <th className="pb-1 text-right font-normal">P&L</th>}
                    </tr>
                  </thead>
                  <tbody className="num">
                    {rows.map((r) => (
                      <tr key={r.asset} className="border-b border-line/40">
                        <td className="py-1.5 align-top">
                          <AssetName asset={r.asset} venue={venue} />
                        </td>
                        <td>
                          <span className={`rounded px-1.5 py-0.5 text-[10px] ${CHANGE[r.change].cls}`}>
                            {CHANGE[r.change].label}
                          </span>
                        </td>
                        <td className="pr-3 text-right align-top text-dim">
                          {compact(r.qty)}
                          <Was show={r.was != null && r.was.qty !== r.qty} text={r.was ? compact(r.was.qty) : ""} />
                        </td>
                        <td className="pr-3 text-right align-top text-dim">
                          {r.weight == null ? "—" : `${r.weight.toFixed(1)}%`}
                          <Was
                            show={
                              r.was?.weight != null &&
                              r.was.weight.toFixed(1) !== (r.weight ?? 0).toFixed(1)
                            }
                            text={r.was?.weight == null ? "" : `${r.was.weight.toFixed(1)}%`}
                          />
                        </td>
                        <td className="pr-3 text-right align-top text-dim">
                          {r.entry == null ? "—" : price(r.entry)}
                        </td>
                        <td className="pr-3 text-right align-top">{price(r.mark)}</td>
                        {settled && (
                          <td className="pr-3 text-right align-top text-dim">
                            {r.markEnd == null ? "—" : price(r.markEnd)}
                          </td>
                        )}
                        {settled && (
                          <td className={`text-right align-top ${tone(r.pnl)}`}>
                            {r.pnl == null ? "—" : signed(r.pnl)}
                            <Was
                              show={r.was?.pnl != null}
                              text={r.was?.pnl == null ? "" : signed(r.was.pnl)}
                            />
                          </td>
                        )}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </div>

          {(failed.length > 0 || notAttempted.length > 0 || roundedOut.length > 0) && (
            <div className="space-y-1.5 text-[11.5px]">
              <p className="eyebrow mb-1">Did not trade</p>
              {failed.map((o, i) => (
                <p key={i} className="text-alarm">
                  <span className="num">{o.asset}</span> — {o.error}
                </p>
              ))}
              {roundedOut.map((r, i) => (
                <p key={i} className="text-dim">
                  {r} <span className="text-faint">· below one lot on the venue's grid</span>
                </p>
              ))}
              {notAttempted.length > 0 && (
                <p className="text-consequence">
                  not attempted after an earlier failure: {notAttempted.join(", ")}
                </p>
              )}
            </div>
          )}

          <div>
            <p className="eyebrow mb-2">
              Risk checks{" "}
              <span className="text-faint">
                {failedChecks.length === 0 ? "· all passed" : `· ${failedChecks.length} failed`}
              </span>
            </p>
            <table className="w-full text-[11.5px]">
              <tbody className="num">
                {(run.risk_checks ?? []).map((c) => {
                  const used = num(c.value) / (num(c.limit) || 1);
                  return (
                    <tr key={c.name} className="border-b border-line/40">
                      <td className="py-1 font-display text-dim">{c.name}</td>
                      <td className="w-[120px] px-3">
                        <div className="h-[4px] rounded-full bg-line/60">
                          <div
                            className={`h-full rounded-full ${
                              !c.passed ? "bg-alarm" : used > 0.8 ? "bg-consequence" : "bg-go/70"
                            }`}
                            style={{ width: `${Math.min(100, Math.max(2, used * 100))}%` }}
                          />
                        </div>
                      </td>
                      <td className="text-right">
                        {short(c.value)} <span className="text-faint">/ {short(c.limit)}</span>
                      </td>
                      <td className="pl-3 text-right text-[11px] text-faint">{c.detail ?? ""}</td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>

          <div className="flex flex-wrap gap-x-5 gap-y-1 border-t border-line pt-3 text-[11px] text-faint">
            <span>reconciled {reconciled ? "yes" : "NO"}</span>
            <span>controls {run.control_state ?? "—"}</span>
            <span className="num">plan {String(run.plan_id ?? "").slice(0, 8)}</span>
            <span className="num">run {String(run.run_id ?? "").slice(0, 8)}</span>
          </div>
        </div>
      </div>
    </div>
  );
}

/** Green for risk added, red for risk removed - the direction of the book, not
 *  of the order. A "buy" that closes a short is a reduction. */
const CHANGE = {
  new: { label: "NEW", cls: "bg-go/15 text-go" },
  added: { label: "ADDED", cls: "bg-go/10 text-go/90" },
  trimmed: { label: "TRIMMED", cls: "bg-consequence/10 text-consequence" },
  closed: { label: "CLOSED", cls: "bg-alarm/10 text-alarm" },
  held: { label: "held", cls: "text-faint" },
} as const;

function Stat({
  k,
  v,
  sub,
  alarm = false,
}: {
  k: string;
  v: string;
  /** What it was supposed to be, where that differs from what it is. */
  sub?: string;
  alarm?: boolean;
}) {
  return (
    <div>
      <p className="eyebrow">{k}</p>
      <p className={`num mt-1 text-[13px] ${alarm ? "text-consequence" : ""}`}>{v}</p>
      {sub && <p className="num text-[10.5px] leading-tight text-faint">{sub}</p>}
    </div>
  );
}

/** "18h" reads; "18.0000001 hours" does not. */
const fmtHours = (h?: number) =>
  h == null ? "period" : h < 1.5 ? `${Math.round(h * 60)} min` : `${h.toFixed(1)}h`;

const pct = (v: unknown) => (v == null ? "—" : `${(num(v) * 100).toFixed(1)}%`);

const compact = (v: unknown) => {
  const n = Math.abs(num(v));
  if (n >= 1e6) return `${(n / 1e6).toFixed(2)}M`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(1)}k`;
  if (n >= 1) return String(Math.round(n * 100) / 100);
  return n.toPrecision(3);
};

/** A $4,351 mark and a $0.0000071 mark need different decimals to say anything. */
const price = (v: unknown) => {
  const n = num(v);
  if (n >= 1000) return money(n, 0);
  if (n >= 1) return money(n, 2);
  if (n >= 0.01) return money(n, 4);
  return `$${n.toPrecision(3)}`;
};

/** Risk values arrive with 28 decimals; three is all a reader can use. */
const short = (v: unknown) => {
  const n = num(v);
  return Number.isInteger(n) ? String(n) : n.toFixed(3);
};

/**
 * The value this cell had at the run before, under the value it has now.
 *
 * A trimmed name at -10% could have come down from -14% or been built up from
 * -6%, and the row on its own cannot tell those apart. Rendered only where
 * there is a previous value and it differs — a "from" line repeating the
 * number above it is noise in every row of the table.
 */
function Was({ show, text }: { show: boolean | undefined; text: string }) {
  if (!show || !text) return null;
  return <div className="text-[10px] leading-tight text-faint">was {text}</div>;
}
