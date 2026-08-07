import { useState } from "react";
import { api } from "../api/client";

/**
 * Three verbs, because stopping a bot is two different instructions.
 *
 *   Resume  trade again
 *   Halt    open nothing new; leave the book exactly as it is
 *   Stop    open nothing new AND close the book at market
 *
 * Halt is the reversible one you reach for at 3am. Stop costs a spread.
 * They are separate buttons so the choice exists at the moment it matters.
 */
export function Controls({
  botId,
  halted,
  flat,
  note,
  onDone,
}: {
  botId: string;
  halted: boolean;
  flat: boolean;
  note?: string;
  onDone: () => void;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  const [msg, setMsg] = useState<{ text: string; bad?: boolean } | null>(null);

  async function run(verb: "resume" | "halt" | "stop") {
    if (verb === "stop" && !window.confirm(
      `Stop ${botId}?\n\nOpen positions are closed at market. Halt instead if you only want to stop opening new ones.`,
    )) return;
    setBusy(verb);
    setMsg(null);
    try {
      await api.control(botId, verb);
      setMsg({
        text:
          verb === "resume" ? "Resumed."
          : verb === "halt" ? "Halted. Open positions untouched."
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
        <Btn kind="go" disabled={!halted || !!busy} busy={busy === "resume"} onClick={() => run("resume")}>
          Resume
        </Btn>
        <Btn kind="quiet" disabled={halted || !!busy} busy={busy === "halt"} onClick={() => run("halt")}>
          Halt
        </Btn>
        <Btn kind="alarm" disabled={(halted && flat) || !!busy} busy={busy === "stop"} onClick={() => run("stop")}>
          Stop
        </Btn>
      </div>
      <p className="mt-3 text-[11px] leading-relaxed text-faint">
        {note ?? (
          <>
            <b className="text-dim">Halt</b> stops new entries at the next cycle and leaves open positions alone.{" "}
            <b className="text-dim">Stop</b> also closes them at market.
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
