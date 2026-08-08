import { useQuery } from "@tanstack/react-query";
import { NavLink, Route, Routes, useParams } from "react-router-dom";
import { api } from "./api/client";
import { Fleet } from "./pages/Fleet";
import { BotPage } from "./pages/BotPage";
import { activity } from "./lib/activity";

const POLL = 10_000;

export default function App() {
  const ov = useQuery({ queryKey: ["overview"], queryFn: api.overview, refetchInterval: POLL });

  return (
    <div className="mx-auto min-h-screen max-w-[1360px] px-5 pb-16">
      {/* The nav dot is the same state a third time; derive it from the one
          rule so it cannot contradict the card and the header. */}
      <Chrome
        bots={
          ov.data?.bots.map((b) => ({
            id: b.bot_id,
            tone: activity({
              enabled: b.enabled,
              control: b.status?.control_state,
              feed: b.status?.feed,
            }).tone,
          })) ?? []
        }
      />
      <main className="pt-5">
        {ov.isLoading && <p className="text-[12px] text-faint">Connecting to the registry…</p>}
        {ov.isError && (
          <p className="text-[12px] text-alarm">
            The registry is unreachable. The bots do not depend on this page — they keep their own
            state and controls; only this view is blind.
          </p>
        )}
        {ov.data && (
          <Routes>
            <Route path="/" element={<Fleet ov={ov.data} onRefresh={() => ov.refetch()} />} />
            <Route path="/bot/:id" element={<BotRoute />} />
          </Routes>
        )}
      </main>
    </div>
  );
}

function BotRoute() {
  const { id = "" } = useParams();
  const q = useQuery({ queryKey: ["bot", id], queryFn: () => api.detail(id), refetchInterval: POLL });
  if (q.isLoading) return <p className="text-[12px] text-faint">Loading {id}…</p>;
  if (q.isError) return <p className="text-[12px] text-alarm">{(q.error as Error).message}</p>;
  return <BotPage id={id} d={q.data!} onRefresh={() => q.refetch()} />;
}

type Tone = ReturnType<typeof activity>["tone"];

function Chrome({ bots }: { bots: { id: string; tone: Tone }[] }) {
  return (
    <header className="flex flex-wrap items-center gap-x-6 gap-y-3 border-b border-line py-4">
      <NavLink to="/" className="flex items-center gap-2.5">
        <Reticle />
        <span className="font-display text-[13px] font-semibold tracking-[0.08em]">
          TRIGGER<span className="text-faint">TRADER</span>
        </span>
      </NavLink>
      <nav className="flex items-center gap-1">
        <Tab to="/" end>Fleet</Tab>
        {bots.map((b) => (
          <Tab key={b.id} to={`/bot/${encodeURIComponent(b.id)}`}>
            <span className={`mr-1.5 inline-block h-[6px] w-[6px] rounded-full ${DOT[b.tone ?? "quiet"]}`} />
            {b.id}
          </Tab>
        ))}
      </nav>
      <span className="num ml-auto text-[11px] text-faint">
        {new Date().toISOString().slice(0, 16).replace("T", " ")}Z
      </span>
    </header>
  );
}

function Tab({ to, end, children }: { to: string; end?: boolean; children: React.ReactNode }) {
  return (
    <NavLink
      to={to}
      end={end}
      className={({ isActive }) =>
        `rounded-md px-2.5 py-1.5 font-display text-[12px] transition ${
          isActive ? "bg-white/[0.06] text-ink" : "text-dim hover:text-ink"
        }`
      }
    >
      {children}
    </NavLink>
  );
}

/** The mark: a reticle. This console exists to keep something in the
 *  crosshairs, and it is the one ornamental thing on the page. */
function Reticle() {
  return (
    <svg width="19" height="19" viewBox="0 0 32 32" aria-hidden>
      <g stroke="#6E8BFF" strokeWidth="2.4" fill="none" strokeLinecap="round">
        <circle cx="16" cy="16" r="7.5" />
        <path d="M16 3.5v5M16 23.5v5M3.5 16h5M23.5 16h5" />
      </g>
      <circle cx="16" cy="16" r="2.4" fill="#6E8BFF" />
    </svg>
  );
}

const DOT: Record<string, string> = {
  quiet: "bg-line2",
  go: "bg-go",
  consequence: "bg-consequence",
  alarm: "bg-alarm",
};
