import { useEffect } from "react";
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

type BookMark = { asset: string; qty: string; mark: string };

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
export function RunModal({ run, onClose }: { run: Run; onClose: () => void }) {
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

  type Row = {
    asset: string;
    change: keyof typeof CHANGE;
    qty: number;
    mark: number;
    notional: number;
    markEnd: number | null;
    pnl: number | null;
  };
  // Runs recorded before the closing book was kept have nothing to list but
  // their exits, and a four-row table under "the book this run left" would
  // read as a four-name book. Say which it is.
  const noBook = (run.book_after ?? []).length === 0;
  const rows: Row[] = (run.book_after ?? []).map((b) => {
    const qty = num(b.qty);
    const mark = num(b.mark);
    const close = endMark.get(b.asset);
    return {
      asset: b.asset,
      change: changeOf(b.asset),
      qty,
      mark,
      notional: qty * mark,
      markEnd: close?.mark ?? null,
      pnl: close?.pnl ?? null,
    };
  });
  // Positions this run ENDED are not in the closing book, and leaving them out
  // would make a run that flattened five names look like it did nothing to
  // them. Listed with a zero closing size, which is what happened.
  for (const m of merged) {
    if ((m.o.reason ?? "") !== "Exit" || !m.o.asset) continue;
    if (rows.some((r) => r.asset === m.o.asset)) continue;
    rows.push({
      asset: m.o.asset,
      change: "closed",
      qty: 0,
      mark: 0,
      notional: 0,
      markEnd: null,
      pnl: null,
    });
  }
  rows.sort((a, b) => Math.abs(b.notional) - Math.abs(a.notional));

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

          <div className="grid grid-cols-2 gap-x-6 gap-y-3 sm:grid-cols-5">
            <Stat k="NAV" v={money(run.nav ?? 0)} />
            <Stat k="Gross" v={pct(run.gross_exposure)} />
            <Stat k="Net" v={pct(run.net_exposure)} />
            <Stat
              k="Positions moved"
              v={String(merged.length)}
            />
            <Stat
              k="Orders"
              v={`${run.orders_submitted ?? 0} over ${run.slices_completed ?? 0}/${run.slices_planned ?? 0} slices`}
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
                  : `· ${rows.length} names, marked when it finished`}
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
                <table className="w-full min-w-[620px] text-[12px]">
                  <thead>
                    <tr className="text-[10px] uppercase tracking-[0.1em] text-faint">
                      <th className="pb-1 text-left font-normal">Asset</th>
                      <th className="pb-1 text-left font-normal">Change</th>
                      <th className="pb-1 text-right font-normal">Qty</th>
                      <th className="pb-1 text-right font-normal">Mark</th>
                      {settled && <th className="pb-1 text-right font-normal">Close</th>}
                      <th className="pb-1 text-right font-normal">Notional</th>
                      <th className="pb-1 text-right font-normal">Weight</th>
                      {settled && <th className="pb-1 text-right font-normal">P&L</th>}
                    </tr>
                  </thead>
                  <tbody className="num">
                    {rows.map((r) => (
                      <tr key={r.asset} className="border-b border-line/40">
                        <td className="py-1.5 font-display text-ink">{r.asset}</td>
                        <td>
                          <span className={`rounded px-1.5 py-0.5 text-[10px] ${CHANGE[r.change].cls}`}>
                            {CHANGE[r.change].label}
                          </span>
                        </td>
                        <td className="pr-3 text-right text-dim">{compact(r.qty)}</td>
                        <td className="pr-3 text-right">{price(r.mark)}</td>
                        {settled && (
                          <td className="pr-3 text-right text-dim">
                            {r.markEnd == null ? "—" : price(r.markEnd)}
                          </td>
                        )}
                        <td className="pr-3 text-right">{money(Math.abs(r.notional), 0)}</td>
                        <td className="pr-3 text-right text-dim">
                          {nav ? `${((r.notional / nav) * 100).toFixed(1)}%` : "—"}
                        </td>
                        {settled && (
                          <td className={`text-right ${tone(r.pnl)}`}>
                            {r.pnl == null ? "—" : signed(r.pnl)}
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

function Stat({ k, v }: { k: string; v: string }) {
  return (
    <div>
      <p className="eyebrow">{k}</p>
      <p className="num mt-1 text-[13px]">{v}</p>
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
