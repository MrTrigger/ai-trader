import { useState } from "react";
import { Link } from "react-router-dom";
import type { BotDetail } from "../api/types";
import { Card, Heart, Pill } from "../components/Card";
import { Controls } from "../components/Controls";
import { KillRail } from "../components/KillRail";
import { RouteChain } from "../components/RouteChain";
import { SettingsModal } from "../components/SettingsModal";
import { money, num, signed, stamp, tone } from "../lib/format";

export function BotPage({ id, d, onRefresh }: { id: string; d: BotDetail; onRefresh: () => void }) {
  const [settings, setSettings] = useState(false);
  const st = d.state ?? {};
  const det = st.detail ?? {};
  const sleeves = Object.entries(det.sleeves ?? {});
  const halted = st.state === "halted" || (d.controls as { kill_switch?: boolean } | null)?.kill_switch === true;
  const open = sleeves.filter(([, s]) => s.in_position).length;

  return (
    <div className="space-y-5">
      <div className="flex flex-wrap items-center gap-3">
        <Link to="/" className="font-display text-[11px] uppercase tracking-widest text-faint hover:text-brand">
          ← Fleet
        </Link>
        <h1 className="font-display text-[20px] font-semibold">{id}</h1>
        {halted ? (
          <Pill tone="alarm">halted{st.state_reason ? ` · ${String(st.state_reason)}` : ""}</Pill>
        ) : (
          <Pill tone="go">{st.state ?? "—"}</Pill>
        )}
        <Heart age={d.heartbeat_age_seconds} />
        <span className="text-[12px] text-faint">{d.display_name} · {d.cadence}</span>
      </div>

      <RouteChain
        broker={d.broker}
        mode={st.mode}
        enabled={d.enabled}
        onSettings={() => setSettings(true)}
      />

      <div className="grid gap-5 lg:grid-cols-[minmax(0,1fr)_330px]">
        <div className="space-y-5">
          {d.contract === "botstate" ? (
            <>
              <Card
                title="Book"
                aside={`${det.instrument ?? ""} · ${det.sizing?.mode ?? "?"} × ${det.sizing?.units ?? "?"}`}
              >
                <div className="flex flex-wrap items-end justify-between gap-6">
                  <div>
                    <p className="eyebrow mb-1.5">Net, all sleeves</p>
                    <p className={`num text-[30px] font-medium leading-none ${tone(det.net_total)}`}>
                      {money(det.net_total ?? 0)}
                    </p>
                  </div>
                  <div className="flex gap-7 text-right">
                    <div>
                      <p className="eyebrow">Fills</p>
                      <p className="num mt-1 text-[15px]">{det.trades_total ?? 0}</p>
                    </div>
                    <div>
                      <p className="eyebrow">Open</p>
                      <p className={`num mt-1 text-[15px] ${open ? "text-consequence" : ""}`}>{open}</p>
                    </div>
                  </div>
                </div>

                <div className="mt-5">
                  <KillRail
                    rolling={det.kill?.rolling_net}
                    limit={det.kill?.limit}
                    sessions={det.kill?.rolling_sessions}
                  />
                </div>

                {/* Four independent walks, shown as four lanes — the shape of
                    the strategy, not a list of rows. */}
                <div className="mt-5 grid gap-3 sm:grid-cols-2">
                  {sleeves.map(([name, s]) => (
                    <div key={name} className="rounded-lg border border-line bg-sunk/50 p-3">
                      <div className="flex items-center gap-2">
                        <span className="font-display text-[12.5px] font-semibold">{name}</span>
                        {s.in_position && <Pill tone="consequence">{s.direction ?? "in position"}</Pill>}
                        {!s.enabled && <Pill>off</Pill>}
                        <span className={`num ml-auto text-[13px] ${tone(s.net_total)}`}>
                          {signed(s.net_total ?? 0)}
                        </span>
                      </div>
                      <p className="num mt-1.5 text-[11px] text-faint">
                        {s.trades_total ?? 0} trades · session {s.session ?? "—"}
                      </p>
                    </div>
                  ))}
                </div>
              </Card>

              <Card title="Fills" aside={`${(d.fills ?? []).length} recorded`}>
                {(d.fills ?? []).length === 0 ? (
                  <p className="text-[12px] text-faint">
                    No fills yet. They land here as the book trades.
                  </p>
                ) : (
                  <div className="overflow-x-auto">
                    <table className="w-full text-[12px]">
                      <thead className="text-faint">
                        <tr className="border-b border-line">
                          {["Session", "Sleeve", "Dir", "Entry", "Exit", "P&L", "Why"].map((h) => (
                            <th key={h} className="eyebrow py-2 pr-4 text-left font-normal">{h}</th>
                          ))}
                        </tr>
                      </thead>
                      <tbody className="num">
                        {[...(d.fills ?? [])].reverse().slice(0, 60).map((f, i) => (
                          <tr key={i} className="border-b border-line/50">
                            <td className="py-1.5 pr-4 text-dim">{f.session_date}</td>
                            <td className="pr-4">{f.sleeve}</td>
                            <td className={`pr-4 ${f.direction === "long" ? "text-go" : "text-alarm"}`}>{f.direction}</td>
                            <td className="pr-4">{f.entry?.toLocaleString()}</td>
                            <td className="pr-4">{f.exit?.toLocaleString()}</td>
                            <td className={`pr-4 ${tone(f.dollars)}`}>{signed(f.dollars)}</td>
                            <td className="text-faint">{f.reason}</td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                )}
              </Card>
            </>
          ) : (
            <Card title="Run history" aside={`${(d.runs ?? []).length} recorded`}>
              {(d.runs ?? []).length === 0 ? (
                <p className="text-[12px] text-faint">Nothing has run yet.</p>
              ) : (
                <div className="space-y-2">
                  {(d.runs ?? []).slice(0, 25).map((r, i) => {
                    const rr = r as Record<string, unknown>;
                    const bad = String(rr.outcome) !== "executed";
                    return (
                      <div key={i} className="flex gap-3 border-b border-line/50 pb-2 text-[12px]">
                        <span className="num shrink-0 text-faint">{stamp(rr.recorded_at as string).slice(5, 16)}</span>
                        <span className={`shrink-0 font-display text-[11px] uppercase ${bad ? "text-consequence" : "text-go"}`}>
                          {String(rr.outcome)}
                        </span>
                        <span className="text-dim">{(rr.detail as string) ?? ""}</span>
                      </div>
                    );
                  })}
                </div>
              )}
            </Card>
          )}
        </div>

        <div className="space-y-5">
          <Card title="Controls" aside={halted ? "halted" : "trading"}>
            <Controls
              botId={id}
              halted={halted}
              flat={open === 0}
              onDone={onRefresh}
            />
          </Card>

          {/* The rail lives with the book, where the number it measures is;
              this card carries only what a rail cannot show — the exact
              distance left. */}
          {d.contract === "botstate" && (
            <Card title="Kill line" aside={`${det.kill?.rolling_sessions ?? 0} of 60 sessions`}>
              <dl className="space-y-1.5 text-[12px]">
                <Row k="Rolling net" v={money(det.kill?.rolling_net ?? 0)} cls={tone(det.kill?.rolling_net)} />
                <Row k="Halts at" v={money(det.kill?.limit ?? 0)} cls="text-alarm" />
                <Row
                  k="Room left"
                  v={money(Math.abs(num(det.kill?.limit) - num(det.kill?.rolling_net)))}
                />
              </dl>
              <p className="mt-3 text-[11px] leading-relaxed text-faint">
                The window fills as sessions accumulate; the line only bites once
                60 of them are in it.
              </p>
            </Card>
          )}
        </div>
      </div>

      {settings && (
        <SettingsModal
          botId={id}
          broker={d.broker}
          assetClass={d.asset_class}
          onClose={() => setSettings(false)}
          onSaved={onRefresh}
        />
      )}
    </div>
  );
}

function Row({ k, v, cls = "" }: { k: string; v: string; cls?: string }) {
  return (
    <div className="flex justify-between">
      <dt className="text-faint">{k}</dt>
      <dd className={`num ${cls}`}>{v}</dd>
    </div>
  );
}
