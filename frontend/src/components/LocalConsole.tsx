import { useQuery } from "@tanstack/react-query";
import { Card } from "./Card";
import { NavChart } from "./NavChart";
import { Positions } from "./Positions";
import { fetchBook } from "../api/book";
import { money, num, signed, tone } from "../lib/format";

export function LocalConsole({ botId, venue }: { botId: string; venue?: string }) {
  const q = useQuery({
    queryKey: ["book", botId],
    queryFn: () => fetchBook(botId),
    refetchInterval: 10_000,
    retry: false,
  });

  const s = q.data;
  if (q.error) return <Card title="Book"><p className="text-[12px] text-faint">{q.error.message}</p></Card>;
  if (!s) return null;
  // A local view that cannot name its bot cannot prove it is the right one.
  if (!s.bot_id || (botId && s.bot_id !== botId)) return null;
  const b = s.book ?? {};
  const positions = b.positions ?? [];
  const runs = s.runs ?? [];

  // Cash is the only real balance on a perps book: everything else in the
  // balances array is a position quantity, and showing it as money made the
  // page claim a $70,958 DOGE "balance" for a $5k position.
  const invested = positions.reduce((a, p) => a + Math.abs(num(p.notional)), 0);

  return (
    <>
      <Card title="Book" aside="stored fills and marks">
        <div className="flex flex-wrap items-end justify-between gap-x-8 gap-y-4">
          <div>
            <p className="eyebrow mb-1.5">Net asset value</p>
            <p className="num text-[32px] font-medium leading-none">{money(b.nav ?? 0)}</p>
            <p className={`num mt-1.5 text-[12px] ${tone(b.total_pnl)}`}>
              {signed(b.total_pnl ?? 0)} <span className="text-faint">since inception</span>
            </p>
          </div>
          <div className="grid grid-cols-3 gap-x-7 gap-y-3 text-right sm:grid-cols-6">
            <Stat k="Gross" v={pctOf(b.gross_exposure)} />
            <Stat k="Net" v={pctOf(b.net_exposure)} cls={netTone(b.net_exposure)} />
            <Stat k="Unrealised" v={signed(b.unrealised_pnl ?? 0)} cls={tone(b.unrealised_pnl)} />
            <Stat k="Realised" v={signed(b.realised_pnl ?? 0)} cls={tone(b.realised_pnl)} />
            <Stat k="Fees" v={money(b.fees_paid ?? 0)} cls="text-dim" />
            <Stat k="Names" v={String(positions.length)} />
          </div>
        </div>

        <div className="mt-5 border-t border-line pt-4">
          <NavChart
            runs={runs}
            initialCash={num(b.nav) - num(b.total_pnl)}
            now={num(b.nav)}
            benchmark={s.benchmark ?? undefined}
          />
        </div>

        <div className="mt-5 border-t border-line pt-4">
          <Positions positions={positions} venue={venue} />
        </div>

        <div className="mt-4 flex flex-wrap gap-x-6 gap-y-1 border-t border-line pt-3 text-[11px] text-faint">
          <span className="num">
            cash <span className="text-dim">{money(b.cash ?? 0)}</span>
          </span>
          <span className="num">
            invested <span className="text-dim">{money(invested, 0)}</span>
          </span>
          {b.fills_count != null && (
            <span className="num">
              fills <span className="text-dim">{b.fills_count}</span>
            </span>
          )}
        </div>
      </Card>

      <Card title="Reconciliation" aside={s.reconciliation?.agrees ? "agrees" : "disagrees"}>
        <p className={`text-[12px] leading-relaxed ${s.reconciliation?.agrees ? "text-dim" : "text-alarm"}`}>
          {s.reconciliation?.explain ?? "Not reported."}
        </p>
      </Card>

      {s.health && !s.health.ok && (
        <Card title="Health" aside="needs attention">
          <ul className="space-y-1.5 text-[12px] text-consequence">
            {(s.health.notes ?? []).map((n, i) => (
              <li key={i}>{n}</li>
            ))}
          </ul>
        </Card>
      )}
    </>
  );
}

function Stat({ k, v, cls = "" }: { k: string; v: string; cls?: string }) {
  return (
    <div>
      <p className="eyebrow">{k}</p>
      <p className={`num mt-1 text-[13.5px] ${cls}`}>{v}</p>
    </div>
  );
}

const pctOf = (v: unknown) =>
  v === "n/a" || v == null ? "—" : `${(num(v) * 100).toFixed(1)}%`;

/** Net is a deviation from dollar-neutral, not a gain: amber past 20%. */
const netTone = (v: unknown) => (Math.abs(num(v)) > 0.2 ? "text-consequence" : "");
