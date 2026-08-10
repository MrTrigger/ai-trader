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
              {(run.result.contributors ?? []).length > 0 && (
                <div className="mt-3">
                  <p className="eyebrow mb-1.5">
                    Movers <span className="text-faint">· mark to mark, biggest first</span>
                  </p>
                  <div className="flex flex-wrap gap-1.5">
                    {(run.result.contributors ?? []).slice(0, 12).map((c) => (
                      <span
                        key={c.asset}
                        title={`${c.qty} @ ${c.mark_start} → ${c.mark_end}`}
                        className={`num rounded border border-line2 px-1.5 py-0.5 text-[11px] ${tone(c.pnl)}`}
                      >
                        <span className="text-ink">{c.asset}</span> {signed(c.pnl)}
                      </span>
                    ))}
                  </div>
                </div>
              )}
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
            <p className="eyebrow mb-2">What changed</p>
            {untouched ? (
              <p className="text-[12px] text-faint">
                No orders. The book already matched the target — every drift was zero.
              </p>
            ) : (
              <div className="space-y-3">
                {groups
                  .filter((g) => g.rows.length > 0)
                  .map((g) => (
                    <div key={g.key}>
                      <div className="mb-1 flex items-baseline gap-2">
                        <span className="text-[11.5px] text-ink">{g.label}</span>
                        <span className="text-[11px] text-faint">
                          {g.rows.length} · {g.hint}
                        </span>
                      </div>
                      <div className="flex flex-wrap gap-1.5">
                        {g.rows.map((m, i) => (
                          <span
                            key={i}
                            title={
                              `${m.o.side} ${m.qty} ${m.o.asset}` +
                              (m.parts > 1 ? ` over ${m.parts} slices` : "") +
                              (m.o.error ? ` — ${m.o.error}` : "")
                            }
                            className={`num rounded border px-1.5 py-0.5 text-[11px] ${
                              m.o.error ? "border-alarm/50 text-alarm" : "border-line2 text-dim"
                            }`}
                          >
                            <span className="text-ink">{m.o.asset}</span>{" "}
                            {String(m.o.side ?? "").toLowerCase()} {compact(m.qty)}
                          </span>
                        ))}
                      </div>
                    </div>
                  ))}
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

/** Risk values arrive with 28 decimals; three is all a reader can use. */
const short = (v: unknown) => {
  const n = num(v);
  return Number.isInteger(n) ? String(n) : n.toFixed(3);
};
