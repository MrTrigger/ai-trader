import { useState } from "react";
import { Link } from "react-router-dom";
import type { BotDetail } from "../api/types";
import { Card, Heart, Pill } from "../components/Card";
import { Controls } from "../components/Controls";
import { KillRail } from "../components/KillRail";
import { RouteChain } from "../components/RouteChain";
import { SettingsModal } from "../components/SettingsModal";
import { FeedStatus } from "../components/FeedStatus";
import { LogModal } from "../components/LogModal";
import { RunModal, type Run } from "../components/RunModal";
import { LocalConsole } from "../components/LocalConsole";
import { money, num, signed, stamp, tone } from "../lib/format";
import { activity, unackSeconds as unack } from "../lib/activity";

/** What a run did, in the space of a table row. */
function summarise(r: Record<string, unknown>): string {
  const sub = Number(r.orders_submitted ?? 0);
  const planned = Number(r.orders_planned ?? 0);
  if (planned === 0 && sub === 0) return "no orders — the book already matched the target";
  const bits = [`${sub} order${sub === 1 ? "" : "s"}`];
  const skipped = Number(r.orders_skipped ?? 0);
  if (skipped) bits.push(`${skipped} skipped`);
  const slices = Number(r.slices_planned ?? 0);
  if (slices > 1) bits.push(`${r.slices_completed}/${slices} slices`);
  return bits.join(" · ");
}

export function BotPage({ id, d, onRefresh }: { id: string; d: BotDetail; onRefresh: () => void }) {
  const [settings, setSettings] = useState(false);
  const [logs, setLogs] = useState(false);
  // The run AND the one before it: a book row can only say "trimmed from" if
  // something knows what it was trimmed from. `runs` is newest first, so the
  // previous run is the next index.
  const [openRun, setOpenRun] = useState<{ run: Run; before?: Run } | null>(null);
  const st = d.state ?? {};
  const det = st.detail ?? {};
  // The venue actually bound for trading, so a symbol links to ITS chart and
  // not to whichever exchange happens to list the same ticker.
  const venueId = d.broker?.venue_id ?? undefined;
  const sleeves = Object.entries(det.sleeves ?? {});
  const feed = det.feed;
  const ctl = d.controls as { state?: string; set_at?: string } | null;
  // Has the bot seen the control yet? lib/activity owns that question, so the
  // fleet card and this page cannot answer it differently — which they did.
  const unackSeconds = unack({
    control: ctl?.state,
    setAt: ctl?.set_at,
    publishedState: st.state,
    heartbeatAgeSeconds: d.heartbeat_age_seconds,
  });
  // What it is doing, which is not what it was told (the control word) and
  // not what it is wired to do (the route chain). See lib/activity.
  const act = activity({
    enabled: d.enabled,
    control: ctl?.state,
    unackSeconds,
    publishedState: st.state,
    stateReason: st.state_reason == null ? undefined : String(st.state_reason),
    feed,
  });
  const halted = ["halted", "self-halted", "stopped", "stopping", "stop-not-applied"].includes(act.key);
  const failing = act.key === "failure";
  const open = sleeves.filter(([, s]) => s.in_position).length;

  return (
    <div className="space-y-5">
      <div className="flex flex-wrap items-center gap-3">
        <Link to="/" className="font-display text-[11px] uppercase tracking-widest text-faint hover:text-brand">
          ← Fleet
        </Link>
        <h1 className="font-display text-[20px] font-semibold">{id}</h1>
        <Pill tone={act.tone}>
          {act.long}
          {/* A rail reason (feed-stall, kill criterion…) is information; the
              operator verbs just repeat the word beside them. */}
          {halted &&
          st.state_reason &&
          !["operator", "operator-stop"].includes(String(st.state_reason))
            ? ` · ${String(st.state_reason)}`
            : ""}
        </Pill>
        {act.key === "idle" && (
          <span className="text-[12px] text-faint">nothing due — the market is closed</span>
        )}
        {/* Stop ENDS the process (flatten, final publish, exit) — idling on
            a Gateway connection to do nothing was waste. So a stopped bot
            with a stale heartbeat is the system at rest, not a fault, and
            gets words instead of a red pulse. Every other state keeps the
            heart: there a dead publisher is exactly what it must reveal. */}
        {act.key === "stopped" ? (
          <span className="text-[12px] text-faint">no process — Start launches one</span>
        ) : act.key === "halted" && (d.heartbeat_age_seconds ?? Infinity) < 180 ? (
          <>
            <span className="text-[12px] text-faint">
              winding down — the process stays up until the book is flat
            </span>
            <Heart age={d.heartbeat_age_seconds} />
          </>
        ) : (
          <Heart age={d.heartbeat_age_seconds} />
        )}
        <span className="text-[12px] text-faint">{d.display_name} · {d.cadence}</span>
        <button
          onClick={() => setLogs(true)}
          className="ml-auto rounded-lg border border-line2 px-3 py-1.5 font-display text-[12px] text-dim
                     transition hover:border-brand/60 hover:text-brand"
        >
          Log
        </button>
      </div>

      {failing && (
        <div className="flex flex-wrap items-center gap-3 rounded-xl border border-alarm/40 bg-alarm/[0.07] px-4 py-3">
          <span className="font-display text-[12px] font-semibold uppercase tracking-wide text-alarm">
            Feed problem
          </span>
          <span className="text-[12.5px] text-ink">
            {feed?.failures
              ? `${feed.failures} failed fetch${feed.failures === 1 ? "" : "es"} in a row.`
              : "No bars are arriving."}{" "}
            {feed?.market_open
              ? "The market is open — the bot is blind and halts itself if this continues."
              : "The market is closed, so nothing is being missed yet."}
          </span>
          {feed?.last_error && (
            <span className="w-full font-mono text-[11px] leading-relaxed text-alarm/90">
              {feed.last_error}
            </span>
          )}
          <button
            onClick={() => setLogs(true)}
            className="ml-auto rounded-lg border border-alarm/40 px-3 py-1.5 font-display text-[12px] text-alarm hover:bg-alarm/10"
          >
            Open log
          </button>
        </div>
      )}

      <RouteChain
        broker={d.broker}
        mode={st.mode}
        enabled={d.enabled}
        onSettings={() => setSettings(true)}
      />

      <div className="grid gap-5 lg:grid-cols-[minmax(0,1fr)_330px]">
        <div className="min-w-0 space-y-5">
          {d.contract === "botstate" ? (
            <>
              <Card
                title="Book"
                // A bot that has never published has no instrument and no
                // sizing, and "· ? × ?" is a worse answer than no answer.
                aside={
                  det.instrument
                    ? `${det.instrument} · ${det.sizing?.mode ?? "?"} × ${det.sizing?.units ?? "?"}`
                    : undefined
                }
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
            <>
            <LocalConsole botId={id} venue={venueId} />
            <Card title="Run history" aside={`${(d.runs ?? []).length} recorded`}>
              {(d.runs ?? []).length === 0 ? (
                <p className="text-[12px] text-faint">Nothing has run yet.</p>
              ) : (
                <div className="space-y-2">
                  {(d.runs ?? []).slice(0, 25).map((r, i) => {
                    const rr = r as Record<string, unknown>;
                    const bad = String(rr.outcome) !== "executed";
                    return (
                      <button
                        key={i}
                        type="button"
                        onClick={() => setOpenRun({ run: r as Run, before: (d.runs ?? [])[i + 1] as Run | undefined })}
                        className="flex w-full items-baseline gap-3 border-b border-line/50 pb-2 text-left text-[12px] hover:bg-line/25 focus:bg-line/25 focus:outline-none"
                        title="what this run decided, and what let it"
                      >
                        <span className="num shrink-0 text-faint">{stamp(rr.recorded_at as string).slice(5, 16)}</span>
                        <span className={`shrink-0 font-display text-[11px] uppercase ${bad ? "text-consequence" : "text-go"}`}>
                          {String(rr.outcome)}
                        </span>
                        <span className="min-w-0 flex-1 truncate text-dim">
                          {(rr.detail as string) ?? summarise(rr)}
                        </span>
                        {(() => {
                          const res = rr.result as { pnl?: string; return_pct?: number } | undefined;
                          if (!res) return <span className="shrink-0 text-[11px] text-faint">unsettled</span>;
                          return (
                            <span className={`num shrink-0 text-[11.5px] ${tone(res.pnl)}`}>
                              {signed(res.pnl ?? 0)}
                              <span className="ml-1 text-faint">
                                {((res.return_pct ?? 0) * 100).toFixed(2)}%
                              </span>
                            </span>
                          );
                        })()}
                        <span className="shrink-0 text-[11px] text-faint">detail →</span>
                      </button>
                    );
                  })}
                </div>
              )}
            </Card>
            </>
          )}
        </div>

        <div className="space-y-5">
          <Card title="Controls" aside={ctl?.state ?? "never set"}>
            <Controls
              botId={id}
              control={ctl?.state}
              stopping={act.key === "stopping"}
              staleSeconds={d.heartbeat_age_seconds}
              // Only claim flat when we can actually see the book. The
              // sleeve list is the futures bot's shape; for anything else
              // `open` is 0 by absence, which would grey out Stop on a bot
              // holding positions. Unknown must fail towards being able to
              // flatten.
              flat={d.contract === "botstate" && open === 0}
              onDone={onRefresh}
            />
          </Card>

          <Card title="Feed" aside={feed?.source === "realtime" ? "realtime" : "polled"}>
            <FeedStatus feed={feed} />
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

      {logs && <LogModal botId={id} onClose={() => setLogs(false)} />}
      {openRun && (
        <RunModal
          run={openRun.run}
          before={openRun.before}
          venue={venueId}
          onClose={() => setOpenRun(null)}
        />
      )}

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
