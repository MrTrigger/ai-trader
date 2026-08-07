import { useMemo, useState } from "react";
import { api } from "../api/client";
import type { Broker } from "../api/types";

/**
 * One settings surface for every bot, asking the two questions that decide
 * where an order goes — kept apart because they carry different weight:
 * switching broker is routine, switching to real money is not.
 */
export function SettingsModal({
  botId,
  broker,
  assetClass,
  onClose,
  onSaved,
}: {
  botId: string;
  broker?: Broker;
  assetClass?: string;
  onClose: () => void;
  onSaved: () => void;
}) {
  const opts = broker?.options ?? [];
  const venues = useMemo(() => [...new Set(opts.map((o) => o.venue_id))], [opts]);
  const [venue, setVenue] = useState(broker?.venue_id ?? venues[0] ?? "");
  const [kind, setKind] = useState(broker?.kind ?? "paper");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const chosen = opts.find((o) => o.venue_id === venue && o.kind === kind);
  const current = broker?.account_id ?? null;
  const changed = !!chosen && chosen.account_id !== current;

  async function apply() {
    if (!chosen) return;
    if (kind === "live") {
      const typed = window.prompt(
        `This points ${botId} at REAL MONEY (${chosen.account_id}).\n\nType LIVE to confirm.`,
      );
      if (typed !== "LIVE") return;
    }
    setBusy(true);
    setErr(null);
    try {
      await api.setAccount(botId, chosen.account_id);
      onSaved();
      onClose();
    } catch (e) {
      setErr((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div
      className="fixed inset-0 z-50 grid place-items-center bg-black/70 p-5 backdrop-blur-[2px]"
      onClick={onClose}
    >
      <div
        role="dialog"
        aria-modal
        aria-label={`Settings for ${botId}`}
        className="w-full max-w-lg overflow-hidden rounded-2xl border border-line2 bg-raised shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <header className="flex items-baseline gap-3 border-b border-line px-5 py-4">
          <h2 className="font-display text-[15px] font-semibold">{botId}</h2>
          <span className="text-[11px] text-faint">wiring · takes effect at the bot's next start</span>
        </header>

        <div className="space-y-5 p-5">
          <Field label="Broker" hint="who routes the orders">
            <select
              value={venue}
              onChange={(e) => setVenue(e.target.value)}
              className="w-full rounded-lg border border-line2 bg-sunk px-3 py-2 text-[13px] text-ink"
            >
              {venues.map((v) => {
                const proto = opts.find((o) => o.venue_id === v)?.protocol;
                return (
                  <option key={v} value={v}>
                    {v}
                    {proto && proto !== v ? ` (via ${proto})` : ""}
                  </option>
                );
              })}
            </select>
            <p className="mt-2 text-[11px] text-faint">
              Only brokers that can trade {assetClass ?? "this bot's instruments"} appear here.
            </p>
          </Field>

          <Field label="Money" hint="whose capital is at risk">
            <select
              value={kind}
              onChange={(e) => setKind(e.target.value)}
              className={`w-full rounded-lg border bg-sunk px-3 py-2 text-[13px] ${
                kind === "live" ? "border-consequence/50 text-consequence" : "border-line2 text-ink"
              }`}
            >
              {["paper", "live"].map((k) => {
                const has = opts.some((o) => o.venue_id === venue && o.kind === k);
                return (
                  <option key={k} value={k} disabled={!has}>
                    {k === "live" ? "Real money" : "Paper"}
                    {has ? "" : " — no account"}
                  </option>
                );
              })}
            </select>
            <p className="mt-2 text-[11px] leading-relaxed text-faint">
              A real account also needs the deployment's ALLOW_LIVE flag. Your choice and the
              deployment's permission both have to say yes.
            </p>
          </Field>

          <div
            className={`rounded-lg border px-3 py-2.5 font-mono text-[12px] ${
              chosen ? "border-line bg-sunk text-dim" : "border-alarm/40 text-alarm"
            }`}
          >
            {chosen ? (
              <>
                Account <b className="text-ink">{chosen.account_id}</b> · credentials{" "}
                <b className="text-ink">{chosen.credential_ref}</b>
                <br />
                {changed ? (
                  <>currently <b className="text-ink">{current ?? "unbound"}</b> — applying rebinds it</>
                ) : (
                  "this is what it already uses"
                )}
              </>
            ) : (
              <>No {venue} account registered for {kind} money.</>
            )}
          </div>

          {err && <p className="text-[12px] text-alarm">{err}</p>}
        </div>

        <footer className="flex justify-end gap-2 border-t border-line px-5 py-4">
          <button
            onClick={onClose}
            className="rounded-lg border border-line2 px-4 py-2 font-display text-[13px] font-semibold text-ink hover:bg-white/5"
          >
            Cancel
          </button>
          <button
            onClick={apply}
            disabled={!changed || busy}
            className={`rounded-lg border px-4 py-2 font-display text-[13px] font-semibold transition disabled:opacity-35
                        ${kind === "live" ? "border-consequence/50 text-consequence hover:bg-consequence/10"
                                          : "border-go/40 text-go hover:bg-go/10"}`}
          >
            {busy ? "…" : "Apply"}
          </button>
        </footer>
      </div>
    </div>
  );
}

function Field({ label, hint, children }: { label: string; hint: string; children: React.ReactNode }) {
  return (
    <div>
      <div className="mb-2 flex items-baseline gap-2">
        <b className="font-display text-[12.5px] font-semibold">{label}</b>
        <span className="text-[11px] text-faint">{hint}</span>
      </div>
      {children}
    </div>
  );
}
