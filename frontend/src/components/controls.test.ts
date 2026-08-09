import { expect, test } from "bun:test";
import { verbs } from "./Controls";

/**
 * Which buttons an operator is offered, as a table.
 *
 * This exists because the rule has been wrong twice, in opposite directions,
 * and neither was caught by anything but a human looking at the page:
 *
 *   1. Start AND Stop both live on an already-stopped bot, so a second Stop
 *      was an invitation to guess what it would do (nothing).
 *   2. Stop withheld from a bot nothing had run recently — which is exactly
 *      the bot whose book might still be open, and whose operator needs to
 *      record "be flat" for whenever something next runs it.
 *
 * A control that is offered when it does nothing teaches the operator that
 * these buttons are decorative. One withheld when it is the only way to reach
 * an open book is worse than that.
 */
type Case = {
  name: string;
  control?: string | null;
  staleSeconds?: number | null;
  flat: boolean;
  start: boolean;
  halt: boolean;
  stop: boolean;
  verb: "Start" | "Resume";
};

const cases: Case[] = [
  {
    name: "running and publishing: halt or stop, no start",
    control: "running",
    staleSeconds: 20,
    flat: true,
    start: false,
    halt: true,
    stop: true,
    verb: "Start",
  },
  {
    name: "halted and flat: Resume back, or Stop to end the process",
    control: "halted",
    staleSeconds: 20,
    flat: true,
    start: true,
    halt: false,
    stop: true,
    verb: "Resume",
  },
  {
    name: "stopped and flat: only Start — a second Stop does nothing",
    control: "stopped",
    staleSeconds: 20,
    flat: true,
    start: true,
    halt: false,
    stop: false,
    verb: "Start",
  },
  {
    name: "stopped but the book is open: Stop stays, flattening is never withheld",
    control: "stopped",
    staleSeconds: 20,
    flat: false,
    start: true,
    halt: false,
    stop: true,
    verb: "Start",
  },
  {
    name: "stale for hours, halted: Stop is the only way to ask for flat",
    control: "halted",
    staleSeconds: 19_000,
    flat: false,
    start: true,
    halt: false,
    stop: true,
    verb: "Resume",
  },
  {
    name: "registered, never run, never told anything: only Start",
    control: null,
    staleSeconds: null,
    flat: true,
    start: true,
    halt: false,
    stop: false,
    verb: "Start",
  },
  {
    name: "never run but Start was pressed: Stop can withdraw it",
    control: "running",
    staleSeconds: null,
    flat: true,
    start: false,
    halt: true,
    stop: true,
    verb: "Start",
  },
];

for (const c of cases) {
  test(c.name, () => {
    const v = verbs({ control: c.control, staleSeconds: c.staleSeconds, flat: c.flat });
    expect({ start: v.canStart, halt: v.canHalt, stop: v.canStop }).toEqual({
      start: c.start,
      halt: c.halt,
      stop: c.stop,
    });
    expect(v.startVerb).toBe(c.verb);
  });
}

test("no heartbeat at all is unreachable, not 'reachable by default'", () => {
  // The absence of evidence read as evidence of absence, backwards: a bot that
  // has never published had `staleSeconds == null`, which the old rule treated
  // as fine, so the page skipped the standing notice that nothing was
  // listening.
  expect(verbs({ control: "running", staleSeconds: null, flat: true }).unreachable).toBe(true);
  expect(verbs({ control: "running", staleSeconds: null, flat: true }).neverRan).toBe(true);
  expect(verbs({ control: "running", staleSeconds: 30, flat: true }).unreachable).toBe(false);
});
