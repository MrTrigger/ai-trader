import { expect, test } from "bun:test";
import { activity, unackSeconds, type Activity } from "./activity";

const NOW = Date.parse("2026-08-12T10:00:00Z");
const ago = (s: number) => new Date(NOW - s * 1000).toISOString();

test("a control on a bot that has never published is never acknowledged", () => {
  // The fourth door into one green lie. `unackSeconds` could only tell that a
  // control was pending by comparing it against the bot's LAST PUBLISH — so
  // when there had never been one, the comparison was impossible and the code
  // read that as taken up. The cluster's futures bot, which has never once
  // reached its loop, showed a green RUNNING pill next to its own "no
  // heartbeat" and a red "nothing has ever run this bot".
  const pending = unackSeconds({
    control: "running",
    setAt: ago(200),
    heartbeatAgeSeconds: null,
    now: NOW,
  });
  expect(pending).toBeCloseTo(200, 0);
  // And the pill that follows from it, which is the whole point.
  expect(activity({ enabled: true, control: "running", unackSeconds: pending }).key).toBe(
    "not-running",
  );
  // Freshly pressed: a transition, not yet a fault.
  const fresh = unackSeconds({
    control: "running",
    setAt: ago(5),
    heartbeatAgeSeconds: null,
    now: NOW,
  });
  expect(activity({ enabled: true, control: "running", unackSeconds: fresh }).key).toBe("starting");
});

test("a bot that is publishing acknowledges by publishing", () => {
  // Set two minutes ago, published thirty seconds ago: the bot has seen it.
  expect(
    unackSeconds({ control: "running", setAt: ago(120), heartbeatAgeSeconds: 30, now: NOW }),
  ).toBeUndefined();
  // Set after the last publish: still pending, and measured from set_at.
  expect(
    unackSeconds({ control: "stopped", setAt: ago(10), heartbeatAgeSeconds: 300, now: NOW }),
  ).toBeCloseTo(10, 0);
  // The bot's own published state naming the outcome outranks every clock.
  expect(
    unackSeconds({
      control: "stopped",
      setAt: ago(10),
      publishedState: "halted",
      heartbeatAgeSeconds: 300,
      now: NOW,
    }),
  ).toBeUndefined();
});

test("no control word means nothing is pending", () => {
  expect(unackSeconds({ control: null, setAt: ago(10), now: NOW })).toBeUndefined();
  expect(unackSeconds({ control: "running", setAt: null, now: NOW })).toBeUndefined();
});

/**
 * The colour table is a contract with the operator's eye, not a style
 * choice: green means working, yellow means running but not working, red
 * means failing, grey means deliberately off. Someone tuning a pill later
 * should have to change this file on purpose.
 */
const cases: [string, Parameters<typeof activity>[0], Activity["key"], Activity["tone"]][] = [
  ["bars arriving", { enabled: true, control: "running", feed: { healthy: true, market_open: true } }, "working", "go"],
  ["market closed", { enabled: true, control: "running", feed: { healthy: true, market_open: false } }, "idle", "consequence"],
  ["feed unhealthy", { enabled: true, control: "running", feed: { healthy: false, market_open: true } }, "failure", "alarm"],
  ["halted by operator", { enabled: true, control: "halted" }, "halted", "quiet"],
  ["stopped by operator", { enabled: true, control: "stopped" }, "stopped", "quiet"],
  ["disabled in registry", { enabled: false }, "disabled", "quiet"],
  ["registered, never told anything", { enabled: true }, "unset", "quiet"],
];

for (const [name, input, key, tone] of cases) {
  test(`${name} → ${key} (${tone})`, () => {
    const a = activity(input);
    expect(a.key).toBe(key);
    expect(a.tone).toBe(tone);
  });
}

test("a failing feed outranks a closed market", () => {
  // Both are true during a weekend outage. The operator needs the fault,
  // not the reassurance — and the old card showed the reassurance.
  expect(
    activity({ enabled: true, control: "running", feed: { healthy: false, market_open: false } }).key,
  ).toBe("failure");
});

test("the control word outranks a failing feed", () => {
  // A halted bot is not going to trade whatever the feed does; leading
  // with the fault would imply there is something to fix first.
  expect(activity({ enabled: true, control: "halted", feed: { healthy: false } }).key).toBe("halted");
});

test("Halt is patient, Stop is not", () => {
  // Halt only prevents the NEXT entry, so a cycle's delay costs nothing and
  // "halting" stays quiet inside the grace.
  expect(activity({ enabled: true, control: "halted", unackSeconds: 20 }).tone).toBe("quiet");

  // Stop means "be flat". Briefly in flight is a transition...
  expect(activity({ enabled: true, control: "stopped", unackSeconds: 3 }).key).toBe("stopping");
  // ...but past the grace nothing is listening, and the operator is
  // carrying risk they believe they cancelled. That is a fault, in red.
  const stuck = activity({ enabled: true, control: "stopped", unackSeconds: 600 });
  expect(stuck.key).toBe("stop-not-applied");
  expect(stuck.tone).toBe("alarm");
});

test("an unacknowledged Start is not 'running'", () => {
  // The hole that produced a green RUNNING pill on a bot which had not run
  // in 25 hours: nothing above matched control=running, there was no feed
  // to fault, so it fell through to "working". Pressing Start records that
  // a bot SHOULD run; it does not make one run.
  expect(activity({ enabled: true, control: "running", unackSeconds: 5 }).key).toBe("starting");
  const stuck = activity({ enabled: true, control: "running", unackSeconds: 90_000 });
  expect(stuck.key).toBe("not-running");
  expect(stuck.tone).toBe("alarm");
  // Acknowledged, and 24/7 so no market to be closed for: plain working.
  expect(activity({ enabled: true, control: "running" }).key).toBe("working");
});

test("an unapplied Halt is also reported", () => {
  expect(activity({ enabled: true, control: "halted", unackSeconds: 5 }).key).toBe("halting");
  expect(activity({ enabled: true, control: "halted", unackSeconds: 90_000 }).key).toBe(
    "halt-not-applied",
  );
});

test("a bot with no feed is never called idle", () => {
  // Crypto is 24/7 and reports no market, so absent feed health there is
  // no quiet period to claim. The control word is part of the premise: this
  // case used to be written `activity({ enabled: true })`, which asserted
  // that a bot NOBODY HAS TOLD ANYTHING is green and working — the bug
  // below, pinned as if it were the requirement.
  expect(activity({ enabled: true, control: "running" }).key).toBe("working");
});

test("no control word is not 'running'", () => {
  // A registered bot that has never been told anything: the state every
  // freshly deployed one is in. Unknown reads as halted in the control
  // contract — fail closed — but this derivation had no case for it, so a
  // bot in the cluster with no control row and no heartbeat wore a green
  // RUNNING pill.
  for (const control of [null, undefined]) {
    const a = activity({ enabled: true, control });
    expect(a.key).toBe("unset");
    expect(a.label).toBe("not started");
    expect(a.tone).toBe("quiet");
  }
  // And it outranks a feed opinion: nothing is running, so there is nothing
  // for a feed to be healthy or unhealthy about.
  expect(activity({ enabled: true, feed: { healthy: false } as never }).key).toBe("unset");
});
