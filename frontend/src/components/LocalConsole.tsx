import { useQuery } from "@tanstack/react-query";
import { Card } from "./Card";
import { money, num, signed, tone } from "../lib/format";

type Local = {
  control_state?: string;
  controls_enabled?: boolean;
  book?: {
    nav?: string | number;
    cash?: string | number;
    total_pnl?: string | number;
    unrealised_pnl?: string | number;
    gross_exposure?: string | number;
    net_exposure?: string | number;
    positions?: { asset: string; qty: string | number; avg_price?: string | number; value?: string | number }[];
    balances?: { currency: string; total: string | number; available: string | number }[];
  };
  reconciliation?: { agrees?: boolean; explain?: string; ledger_entries?: number };
  health?: { ok?: boolean; notes?: string[] };
};

/**
 * The book of a bot this api process wraps directly.
 *
 * Only this process can read it — it has the state dir and the bot binary —
 * so the panel appears for that bot and stays absent everywhere else,
 * rather than showing empty cards that imply missing data.
 */
export function LocalConsole() {
  const q = useQuery({
    queryKey: ["local"],
    queryFn: async () => {
      const r = await fetch("/api/state", { headers: { accept: "application/json" } });
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      const j = (await r.json()) as Local & { fleet_only?: boolean };
      return j.fleet_only ? null : j;
    },
    refetchInterval: 10_000,
    retry: false,
  });

  const s = q.data;
  if (!s) return null;
  const b = s.book ?? {};
  const positions = b.positions ?? [];
  const balances = b.balances ?? [];

  return (
    <>
      <Card title="Book" aside="as this process reads it">
        <div className="flex flex-wrap items-end justify-between gap-6">
          <div>
            <p className="eyebrow mb-1.5">Net asset value</p>
            <p className="num text-[30px] font-medium leading-none">{money(b.nav ?? 0)}</p>
            <p className={`num mt-1.5 text-[12px] ${tone(b.total_pnl)}`}>
              {signed(b.total_pnl ?? 0)} <span className="text-faint">since inception</span>
            </p>
          </div>
          <div className="flex gap-7 text-right">
            <Stat k="Gross" v={pctOf(b.gross_exposure)} />
            <Stat k="Net" v={pctOf(b.net_exposure)} />
            <Stat k="Unrealised" v={signed(b.unrealised_pnl ?? 0)} cls={tone(b.unrealised_pnl)} />
          </div>
        </div>

        <div className="mt-5">
          <p className="eyebrow mb-2">Positions</p>
          {positions.length === 0 ? (
            <p className="text-[12px] text-faint">Flat — nothing open.</p>
          ) : (
            <table className="w-full text-[12px]">
              <tbody className="num">
                {positions.map((p) => (
                  <tr key={p.asset} className="border-b border-line/50">
                    <td className="py-1.5 font-display">{p.asset}</td>
                    <td className="pr-4 text-right">{String(p.qty)}</td>
                    <td className="pr-4 text-right text-dim">{p.avg_price ? money(p.avg_price) : "—"}</td>
                    <td className="text-right">{p.value ? money(p.value) : ""}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>

        {balances.length > 0 && (
          <div className="mt-5">
            <p className="eyebrow mb-2">Balances</p>
            <table className="w-full text-[12px]">
              <tbody className="num">
                {balances.map((x) => (
                  <tr key={x.currency} className="border-b border-line/50">
                    <td className="py-1.5 font-display">{x.currency}</td>
                    <td className="pr-4 text-right">{money(x.total)}</td>
                    <td className="text-right text-dim">{money(x.available)} available</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
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
      <p className={`num mt-1 text-[14px] ${cls}`}>{v}</p>
    </div>
  );
}

const pctOf = (v: unknown) =>
  v === "n/a" || v == null ? "—" : `${(num(v) * 100).toFixed(1)}%`;
