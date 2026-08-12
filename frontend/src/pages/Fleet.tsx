import { Link } from "react-router-dom";
import type { Overview } from "../api/types";
import { Card, Heart, Pill } from "../components/Card";
import { Spark } from "../components/Spark";
import { execution } from "../components/RouteChain";
import { activity, unackSeconds as unack } from "../lib/activity";
import { FeedStatus } from "../components/FeedStatus";
import { money, num, signed, stamp, tone } from "../lib/format";

/**
 * The fleet answers one question before any other: is everything as I left
 * it? So the band leads with STATE — how many bots, how many armed to send
 * orders, how many halted, and whether any real money is exposed. P&L is
 * second, because a portfolio app opens with money and an operations
 * console opens with control.
 */
export function Fleet({ ov, onRefresh }: { ov: Overview; onRefresh: () => void }) {
  const bots = ov.bots ?? [];
  const sending = bots.filter((b) => b.status?.mode === "live").length;
  // Counted off the control word, so Stop registers here too — the old
  // kill_switch reading missed it entirely.
  const stopped = bots.filter((b) => ["halted", "stopped"].includes(b.status?.control_state ?? "")).length;
  const real = bots.filter((b) => b.broker?.kind === "live");
  const net = bots.reduce((a, b) => a + num(b.status?.net_total), 0);

  return (
    <div className="space-y-5">
      <section className="card p-5">
        <div className="flex flex-wrap items-end justify-between gap-6">
          <div>
            <p className="eyebrow mb-2">Fleet state</p>
            <p className="font-display text-[26px] font-semibold leading-none">
              {bots.length} bot{bots.length === 1 ? "" : "s"}
              <span className="text-faint"> · </span>
              <span className={sending ? "text-go" : "text-dim"}>{sending} armed</span>
              <span className="text-faint"> · </span>
              <span className={stopped ? "text-consequence" : "text-dim"}>{stopped} stopped</span>
            </p>
            <p className="mt-2 text-[12px] text-faint">
              {real.length === 0
                ? "No bot is bound to a real-money account."
                : `${real.length} bound to real money: ${real.map((b) => b.bot_id).join(", ")}.`}
            </p>
          </div>
          <div className="text-right">
            <p className="eyebrow mb-1.5">Net, all bots</p>
            <p className={`num text-[24px] font-medium ${tone(net)}`}>{money(net)}</p>
          </div>
        </div>
      </section>

      <div className="grid gap-5 lg:grid-cols-[minmax(0,1fr)_340px]">
        <div className="grid gap-4 sm:grid-cols-2">
          {bots.map((b) => {
            const st = b.status ?? {};
            const ex = execution(st.mode);
            const series = (b.series ?? []).map(([, v]) => num(v));
            return (
              <Link
                key={b.bot_id}
                to={`/bot/${encodeURIComponent(b.bot_id)}`}
                className="card group block p-4 transition hover:border-brand/50"
              >
                <div className="flex items-center gap-2.5">
                  <h3 className="font-display text-[15px] font-semibold">{b.bot_id}</h3>
                  {(() => {
                    const act = activity({
                      enabled: b.enabled,
                      control: st.control_state,
                      // The card used to omit this, so a control word nothing
                      // had answered still rendered green here even while the
                      // bot's own page called it out.
                      unackSeconds: unack({
                        control: st.control_state,
                        setAt: st.control_set_at,
                        heartbeatAgeSeconds: st.heartbeat_age_seconds,
                      }),
                      feed: st.feed,
                    });
                    return <Pill tone={act.tone}>{act.label}</Pill>;
                  })()}
                  <span className="ml-auto"><Heart age={st.heartbeat_age_seconds} /></span>
                </div>

                <p className="mt-1.5 text-[12px] text-faint">
                  {b.asset_class} · {b.cadence}
                </p>

                <p className="num mt-2 text-[12px] text-dim">
                  {b.broker?.venue_id ?? "unbound"}
                  <span className="text-faint"> → </span>
                  {b.broker?.account_id ?? "—"}
                  <span className="text-faint"> → </span>
                  <span className={b.broker?.kind === "live" ? "text-consequence" : ""}>
                    {b.broker?.kind === "live" ? "real" : "paper"}
                  </span>
                  <span className="text-faint"> → </span>
                  <span className={ex.live ? "text-alarm" : ""}>{ex.label}</span>
                </p>

                <div className="mt-2">
                  <FeedStatus feed={st.feed} compact />
                </div>

                <div className="mt-3"><Spark points={series} /></div>

                <div className="mt-3 grid grid-cols-3 gap-2 border-t border-line pt-3">
                  <Stat k="Net" v={st.net_total == null ? "—" : signed(st.net_total)} cls={tone(st.net_total)} />
                  <Stat k={st.contract === "botstate" ? "Fills" : "Runs"} v={String(st.trades_total ?? series.length)} />
                  <Stat k="Last" v={st.last_run?.recorded_at ? stamp(st.last_run.recorded_at).slice(5, 16) : "—"} />
                </div>
              </Link>
            );
          })}
        </div>

        <Card title="Activity" aside="every bot, newest first">
          <div className="max-h-[520px] space-y-3 overflow-y-auto pr-1">
            {(ov.feed ?? []).length === 0 && <p className="text-[12px] text-faint">Nothing has happened yet.</p>}
            {(ov.feed ?? []).map((e, i) => (
              <div key={i} className="border-l-2 border-line pl-3">
                <p className="num text-[10.5px] text-faint">{stamp(e.at).slice(5, 16)}</p>
                <p className="text-[12px] leading-snug">
                  <b className="font-display">{e.bot_id}</b>{" "}
                  <span className={/halt|refus|error|stall/i.test(e.kind ?? "") ? "text-alarm" : "text-dim"}>
                    {e.text ?? e.kind}
                  </span>
                </p>
              </div>
            ))}
          </div>
        </Card>
      </div>
      <button onClick={onRefresh} className="sr-only">refresh</button>
    </div>
  );
}

function Stat({ k, v, cls = "" }: { k: string; v: string; cls?: string }) {
  return (
    <div>
      <p className="eyebrow">{k}</p>
      <p className={`num mt-0.5 text-[13px] ${cls}`}>{v}</p>
    </div>
  );
}
