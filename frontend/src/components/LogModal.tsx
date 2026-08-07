import { useQuery } from "@tanstack/react-query";
import { stamp } from "../lib/format";

/** What the bot said. Reading it should not require a terminal. */
export function LogModal({ botId, onClose }: { botId: string; onClose: () => void }) {
  const q = useQuery({
    queryKey: ["logs", botId],
    queryFn: async () => {
      const r = await fetch(`/api/bots/${encodeURIComponent(botId)}/logs`);
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      return (await r.json()) as { lines: { at: string; level: string; line: string }[] };
    },
    refetchInterval: 5000,
  });

  return (
    <div className="fixed inset-0 z-50 grid place-items-center bg-black/70 p-5 backdrop-blur-[2px]" onClick={onClose}>
      <div
        role="dialog"
        aria-modal
        aria-label={`Log for ${botId}`}
        className="flex h-[70vh] w-full max-w-3xl flex-col overflow-hidden rounded-2xl border border-line2 bg-raised shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <header className="flex items-baseline gap-3 border-b border-line px-5 py-4">
          <h2 className="font-display text-[15px] font-semibold">{botId}</h2>
          <span className="text-[11px] text-faint">log · newest first · refreshes every 5s</span>
          <button onClick={onClose} className="ml-auto text-[12px] text-dim hover:text-ink">
            Close
          </button>
        </header>
        <div className="flex-1 overflow-y-auto px-5 py-4">
          {q.isLoading && <p className="text-[12px] text-faint">Loading…</p>}
          {q.data?.lines.length === 0 && (
            <p className="text-[12px] text-faint">
              Nothing logged yet. The bot writes here when something changes — the feed going
              down or recovering, a halt, a session rolling.
            </p>
          )}
          <div className="space-y-1.5">
            {q.data?.lines.map((l, i) => (
              <div key={i} className="flex gap-3 font-mono text-[11.5px] leading-relaxed">
                <span className="shrink-0 text-faint">{stamp(l.at).slice(5, 19)}</span>
                <span
                  className={`shrink-0 uppercase ${
                    l.level === "error" ? "text-alarm" : l.level === "warn" ? "text-consequence" : "text-faint"
                  }`}
                >
                  {l.level}
                </span>
                <span className={l.level === "error" ? "text-ink" : "text-dim"}>{l.line}</span>
              </div>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}
