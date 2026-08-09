import { useState } from "react";
import { api } from "../api/client";
import { ago } from "../lib/format";

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
/**
 * Which verbs this bot is offered, and what the first one is called.
 *
 * A pure function because it is the part that keeps being wrong, and being
 * wrong here is not cosmetic: it decides whether an operator can reach the
 * book. It has been wrong twice — Start and Stop both live on an already
 * stopped bot, and then Stop withheld from a stale one that might still hold
 * positions — so the table in `controls.test.ts` is the specification and this
 * is only its implementation.
 */
export function verbs({
  control,
  staleSeconds,
  flat,
}: {
  control?: string | null;
  staleSeconds?: number | null;
  flat: boolean;
}) {
  // Nothing has published in two minutes: whatever we write, no process is
  // going to act on it. Saying so at the moment of pressing is the whole
  // point — a promise about "the next cycle" is a lie when there is no next
  // cycle, and this bot's book stays open until something runs it.
  //
  // No heartbeat AT ALL is the strongest form of that, not the absence of
  // evidence: a bot that has never published has certainly never run. Reading
  // null as "reachable" is why a freshly deployed bot offered Stop and skipped
  // the notice that nothing would act on it.
  const neverRan = staleSeconds == null;
  const unreachable = neverRan || staleSeconds > 120;
  const stopped = control === "stopped";
  const running = control === "running";
  // "Resume" is a promise about a book that is still there. Only a halt leaves
  // one, so it is the only word for coming back out of one; a bot that was
  // stopped, or has never been told anything at all, is being STARTED.
  const startVerb = control === "halted" ? "Resume" : "Start";

  // Each verb is offered exactly when pressing it would change something.
  // Both Start and Stop were enabled on an already-stopped bot, which asks
  // the operator to guess what a second Stop would do: nothing.
  const canStart = !running;
  const canHalt = running;
  // A bot nothing has EVER run, and nothing has ever been asked of: there is
  // no book to close and no instruction to withdraw, so Stop would record a
  // word about a process that has never existed. Deliberately narrower than
  // "unreachable": requiring a live process instead cost the operator of a
  // stale bot their only way to say "be flat when you next run".
  const untouched = neverRan && control == null;
  const canStop = !untouched && (!stopped || (!flat && !unreachable));
  return { neverRan, unreachable, running, stopped, startVerb, canStart, canHalt, canStop };
}

export function Controls({
  botId,
  control,
  stopping = false,
  staleSeconds,
  flat,
  note,
  onDone,
}: {
  botId: string;
  /** The canonical control word: running | halted | stopped. */
  control?: string | null;
  /** A Stop is in flight; starting now would race the flatten. */
  stopping?: boolean;
  /**
   * Age of the bot's last published state. A control is only an
   * instruction if something is listening for it, and this is how we know.
   */
  staleSeconds?: number | null;
  flat: boolean;
  note?: string;
  onDone: () => void;
}) {
  const { neverRan, unreachable, startVerb, canStart, canHalt, canStop } = verbs({
    control,
    staleSeconds,
    flat,
  });
  const [busy, setBusy] = useState<string | null>(null);
  const [msg, setMsg] = useState<{ text: string; bad?: boolean } | null>(null);

  async function run(verb: "resume" | "halt" | "stop") {
    if (verb === "stop" && !window.confirm(
      `Stop ${botId}?\n\nEverything open is closed at market, now. Halt instead to stop opening new positions and let the book wind down on its own exits.` +
      (unreachable
        ? `\n\nWARNING: ${neverRan ? "nothing has ever run this bot" : `nothing has run this bot in ${ago(staleSeconds)}`}. The stop is recorded, but no process is listening — the book stays open until something runs it.`
        : ""),
    )) return;
    setBusy(verb);
    setMsg(null);
    try {
      await api.control(botId, verb);
      // The bot obeys in under a second now (Postgres pushes the control);
      // the page's 10s poll must not be the thing that makes it look slow.
      // Refetch in a short burst so the pill catches the acknowledgement.
      for (const delay of [400, 1200, 2500, 5000]) {
        setTimeout(onDone, delay);
      }
      setMsg({
        text:
          unreachable
            ? neverRan
              ? "Recorded. Nothing has ever run this bot, so nothing will act on it yet."
              : `Recorded. Nothing has run this bot in ${ago(staleSeconds)}, so nothing will act on it yet.`
            : verb === "resume"
              ? `${startVerb === "Start" ? "Started" : "Resumed"}.`
              : verb === "halt"
                ? "Halted. Nothing new opens; what is held still exits on its own signals."
                : "Stopping. Closing everything at market.",
        bad: unreachable,
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
        <Btn kind="go" disabled={!canStart || stopping || !!busy} busy={busy === "resume"} onClick={() => run("resume")}>
          {startVerb}
        </Btn>
        <Btn kind="quiet" disabled={!canHalt || !!busy} busy={busy === "halt"} onClick={() => run("halt")}>
          Halt
        </Btn>
        {/* Stop stays available whenever anything is open, whatever the
            control word says — flattening is never the thing to withhold. */}
        <Btn kind="alarm" disabled={!canStop || !!busy} busy={busy === "stop"} onClick={() => run("stop")}>
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
      {/* Standing notice, not a toast. "Stop closes everything at market
          immediately" is false for a bot nothing is running, and the
          operator needs that on the page rather than once, in a message
          they have already dismissed. */}
      {unreachable && (
        <p className="mt-2 text-[12px] leading-relaxed text-alarm">
          {neverRan
            ? "Nothing has ever run this bot."
            : `Nothing has run this bot in ${ago(staleSeconds)}.`}{" "}
          Controls are recorded, but no process is listening — none of these take effect until
          something runs it.
        </p>
      )}
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
