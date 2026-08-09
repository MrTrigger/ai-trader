import { money, num, signed, tone } from "../lib/format";

export type Position = {
  asset: string;
  qty: string | number;
  avg_price?: string | number;
  mark?: string | number;
  notional?: string | number;
  weight?: number;
  unrealised_pnl?: string | number;
  side?: string;
};

/**
 * The book, with the columns needed to judge it.
 *
 * The api has carried avg_price, mark, notional, weight and unrealised P&L all
 * along and the page showed three of them, so a reader could see WHAT was held
 * and nothing about whether it was working. Longs and shorts are separated
 * because a dollar-neutral book is two books whose sizes are supposed to
 * match, and a single sorted list hides exactly that.
 *
 * Weight is drawn as a bar from a centre line: sign is position, magnitude is
 * length, so concentration is visible without reading a single number.
 */
export function Positions({ positions }: { positions: Position[] }) {
  if (positions.length === 0) {
    return <p className="text-[12px] text-faint">Flat — nothing open.</p>;
  }

  const longs = positions.filter((p) => num(p.qty) > 0);
  const shorts = positions.filter((p) => num(p.qty) < 0);
  const grossOf = (rows: Position[]) =>
    rows.reduce((a, p) => a + Math.abs(num(p.notional)), 0);
  const pnlOf = (rows: Position[]) =>
    rows.reduce((a, p) => a + num(p.unrealised_pnl), 0);
  const widest = Math.max(...positions.map((p) => Math.abs(p.weight ?? 0)), 0.01);

  return (
    <div className="space-y-4">
      {[
        { label: "Long", rows: longs },
        { label: "Short", rows: shorts },
      ]
        .filter((g) => g.rows.length > 0)
        .map((g) => (
          <div key={g.label}>
            <div className="mb-1.5 flex items-baseline justify-between">
              <span className="eyebrow">
                {g.label} <span className="text-faint">· {g.rows.length}</span>
              </span>
              <span className="num text-[11px] text-faint">
                {money(grossOf(g.rows), 0)} ·{" "}
                <span className={tone(pnlOf(g.rows))}>{signed(pnlOf(g.rows))}</span>
              </span>
            </div>
            <div className="-mx-1 overflow-x-auto px-1">
            <table className="w-full min-w-[560px] text-[12px]">
              <thead>
                <tr className="text-[10px] uppercase tracking-[0.1em] text-faint">
                  <th className="pb-1 text-left font-normal">Asset</th>
                  <th className="pb-1 text-right font-normal">Qty</th>
                  <th className="pb-1 text-right font-normal">Entry</th>
                  <th className="pb-1 text-right font-normal">Mark</th>
                  <th className="pb-1 text-right font-normal">Notional</th>
                  <th className="pb-1 pl-3 text-right font-normal">Weight</th>
                  <th className="pb-1 text-right font-normal">Unrealised</th>
                </tr>
              </thead>
              <tbody className="num">
                {g.rows
                  .slice()
                  .sort((a, b) => Math.abs(num(b.notional)) - Math.abs(num(a.notional)))
                  .map((p) => {
                    const w = p.weight ?? 0;
                    const pct = (Math.abs(w) / widest) * 100;
                    return (
                      <tr key={p.asset} className="border-b border-line/40">
                        <td className="py-1.5 font-display text-ink">{p.asset}</td>
                        <td className="pr-3 text-right text-dim">{compact(p.qty)}</td>
                        <td className="pr-3 text-right text-dim">{price(p.avg_price)}</td>
                        <td className="pr-3 text-right">{price(p.mark)}</td>
                        <td className="pr-3 text-right">{money(Math.abs(num(p.notional)), 0)}</td>
                        <td className="w-[86px] pl-3">
                          <div className="flex items-center gap-1.5">
                            <div className="relative h-[5px] flex-1 rounded-full bg-line/60">
                              <div
                                className={`absolute top-0 h-full rounded-full ${
                                  w >= 0 ? "left-1/2 bg-go/70" : "right-1/2 bg-alarm/70"
                                }`}
                                style={{ width: `${pct / 2}%` }}
                              />
                            </div>
                            <span className="w-[38px] text-right text-[11px] text-dim">
                              {(w * 100).toFixed(1)}%
                            </span>
                          </div>
                        </td>
                        <td className={`text-right ${tone(p.unrealised_pnl)}`}>
                          {signed(p.unrealised_pnl ?? 0)}
                        </td>
                      </tr>
                    );
                  })}
              </tbody>
            </table>
            </div>
          </div>
        ))}
    </div>
  );
}

/** Crypto quantities span nine orders of magnitude; full precision is noise. */
const compact = (v: unknown) => {
  const n = Math.abs(num(v));
  if (n >= 1e6) return `${(n / 1e6).toFixed(2)}M`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(1)}k`;
  if (n >= 1) return n.toFixed(2);
  return n.toPrecision(3);
};

/** A $4,351 mark and a $0.0000071 mark need different decimals to say anything. */
const price = (v: unknown) => {
  if (v == null) return "—";
  const n = num(v);
  if (n >= 1000) return money(n, 0);
  if (n >= 1) return money(n, 2);
  if (n >= 0.01) return money(n, 4);
  return `$${n.toPrecision(3)}`;
};
