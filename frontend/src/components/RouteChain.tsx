import type { Broker } from "../api/types";

/**
 * The order route, drawn as the path it actually is.
 *
 * These four facts used to sit in four unrelated boxes, and the page could
 * say "paper" in one and "live" in another and be telling the truth twice.
 * As one chain — broker, then account, then whose money, then whether
 * orders leave the building — they cannot be read independently, which is
 * the point: an order traverses all four or none.
 */
export function RouteChain({
  broker,
  mode,
  enabled,
  onSettings,
}: {
  broker?: Broker;
  mode?: string | null;
  enabled?: boolean;
  onSettings?: () => void;
}) {
  const b = broker ?? {};
  const realMoney = b.kind === "live";
  const ex = execution(mode);
  const stations: Station[] = [
    { k: "Broker", v: b.venue_id ?? "unbound", note: b.protocol && b.protocol !== b.venue_id ? `via ${b.protocol}` : undefined },
    { k: "Account", v: b.account_id ?? "—" },
    { k: "Money", v: realMoney ? "real" : b.account_id ? "paper" : "—", accent: realMoney ? "consequence" : undefined },
    { k: "Execution", v: ex.label, accent: ex.live ? "alarm" : undefined },
  ];

  return (
    <div className="card flex flex-wrap items-stretch gap-y-2 px-1 py-1">
      {stations.map((s, i) => (
        <div key={s.k} className="flex items-center animate-draw" style={{ animationDelay: `${i * 55}ms` }}>
          {i > 0 && <Connector />}
          <div className="px-4 py-2">
            <div className="eyebrow">{s.k}</div>
            <div
              className={`num text-[13px] leading-5 ${
                s.accent === "consequence" ? "text-consequence" : s.accent === "alarm" ? "text-alarm" : "text-ink"
              }`}
            >
              {s.v}
              {s.note && <span className="ml-1.5 text-faint">{s.note}</span>}
            </div>
          </div>
        </div>
      ))}

      <div className="ml-auto flex items-center gap-3 px-3">
        <span className={`num text-[11px] ${enabled ? "text-dim" : "text-consequence"}`}>
          {enabled ? "registered" : "disabled in registry"}
        </span>
        {onSettings && (
          <button
            onClick={onSettings}
            className="rounded-lg border border-line2 bg-raised px-3.5 py-1.5 font-display text-[12px] font-semibold
                       text-ink transition hover:border-brand/60 hover:text-brand"
          >
            Settings
          </button>
        )}
      </div>
    </div>
  );
}

type Station = { k: string; v: string; note?: string; accent?: "consequence" | "alarm" };

/** A drawn segment, not a bullet: the eye follows it in the direction an
 *  order travels. */
function Connector() {
  return (
    <svg width="26" height="10" viewBox="0 0 26 10" aria-hidden className="shrink-0 text-line2">
      <path d="M0 5h18" stroke="currentColor" strokeWidth="1.5" />
      <path d="M18 1.5 24 5l-6 3.5z" fill="currentColor" />
    </svg>
  );
}

export function execution(mode?: string | null): { label: string; live: boolean } {
  switch (mode) {
    // "sending orders" reads as something happening now, and this cell is
    // wiring, not activity — it said "sending orders" all weekend with the
    // market shut and the bot idle. Armed is a state you can check before
    // an open, which is the question this cell exists to answer.
    case "live": return { label: "orders armed", live: true };
    case "shadow": return { label: "shadow, no orders", live: false };
    case "replay": return { label: "replay", live: false };
    case "paper": return { label: "simulated fills", live: false };
    case "live-readonly": return { label: "reading only", live: false };
    default: return { label: mode ?? "—", live: false };
  }
}
