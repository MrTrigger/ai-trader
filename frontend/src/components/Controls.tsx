import { useState } from "react";
import { api } from "../api/client";

/**
 * Three verbs over a three-state machine.
 *
 *   running  --halt-->  halted   take nothing new on; wind down what is held
 *   running  --stop-->  stopped  open nothing new AND close at market
 *   halted   --resume--> running
 *   stopped  --start-->  running
 *
 * Halt is the graceful one: the book stops taking positions on, and the
 * ones it holds still exit when the strategy says so. Stop is the abrupt
 * one — everything closes at market, now, and it costs a spread.
 * They are separate buttons so the choice exists at the moment it matters.
 *
 * This took a boolean `halted` until it turned out a boolean cannot hold
 * three states: a stopped bot read as neither halted nor running, so the
 * way back was disabled and Stop was a one-way door from the dashboard.
 * The first button is therefore named for the transition it performs —
 * Start out of stopped, Resume out of halted — because those are
 * different promises about what happens to the book.
 */
export function Controls({
  botId,
  control,
  stopping = false,
  flat,
  note,
  onDone,
}: {
  botId: string;
  /** The canonical control word: running | halted | stopped. */
  control?: string | null;
  /** A Stop is in flight; starting now would race the flatten. */
  stopping?: boolean;
  flat: boolean;
  note?: string;
  onDone: () => void;
}) {
  const stopped = control === "stopped";
  const halted = stopped || control === "halted";
  const startVerb = stopped ? "Start" : "Resume";
  const [busy, setBusy] = useState<string | null>(null);
  const [msg, setMsg] = useState<{ text: string; bad?: boolean } | null>(null);

  async function run(verb: "resume" | "halt" | "stop") {
    if (verb === "stop" && !window.confirm(
      `Stop ${botId}?\n\nEverything open is closed at market, now. Halt instead to stop opening new positions and let the book wind down on its own exits.`,
    )) return;
    setBusy(verb);
    setMsg(null);
    try {
      await api.control(botId, verb);
      setMsg({
        text:
          verb === "resume" ? `${startVerb === "Start" ? "Started" : "Resumed"}. Trading again at the next cycle.`
          : verb === "halt" ? "Halted. Nothing new opens; what is held still exits on its own signals."
          : "Stopping. The book closes at the bot's next cycle.",
      });
      onDone();
    } catch (e) {
      setMsg({ text: (e as Error).message, bad: true });
    } finally {
      setBusy(null);
    }
  }

  return (
    <div>
      <div className="grid grid-cols-3 gap-2">
        <Btn kind="go" disabled={!halted || stopping || !!busy} busy={busy === "resume"} onClick={() => run("resume")}>
          {startVerb}
        </Btn>
        <Btn kind="quiet" disabled={halted || !!busy} busy={busy === "halt"} onClick={() => run("halt")}>
          Halt
        </Btn>
        {/* Stop stays available whenever anything is open, whatever the
            control word says — flattening is never the thing to withhold. */}
        <Btn kind="alarm" disabled={(stopped && flat) || !!busy} busy={busy === "stop"} onClick={() => run("stop")}>
          Stop
        </Btn>
      </div>
      <p className="mt-3 text-[11px] leading-relaxed text-faint">
        {note ?? (
          <>
            <b className="text-dim">Halt</b> takes nothing new on and lets the book wind down — open
            positions still close when the strategy exits them.{" "}
            <b className="text-dim">Stop</b> closes everything at market immediately.
          </>
        )}
      </p>
      {msg && (
        <p className={`mt-2 text-[12px] ${msg.bad ? "text-alarm" : "text-go"}`}>{msg.text}</p>
      )}
    </div>
  );
}

function Btn({
  kind, disabled, busy, onClick, children,
}: {
  kind: "go" | "quiet" | "alarm";
  disabled?: boolean;
  busy?: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  const map = {
    go: "border-go/40 text-go hover:bg-go/10",
    quiet: "border-line2 text-ink hover:bg-white/5",
    alarm: "border-alarm/45 text-alarm hover:bg-alarm/10",
  } as const;
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      className={`rounded-lg border px-3 py-2 font-display text-[13px] font-semibold transition
                  disabled:cursor-not-allowed disabled:opacity-35 ${map[kind]}`}
    >
      {busy ? "…" : children}
    </button>
  );
}
