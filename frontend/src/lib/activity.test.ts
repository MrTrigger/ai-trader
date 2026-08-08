import { expect, test } from "bun:test";
import { activity, type Activity } from "./activity";

/**
 * The colour table is a contract with the operator's eye, not a style
 * choice: green means working, yellow means running but not working, red
 * means failing, grey means deliberately off. Someone tuning a pill later
 * should have to change this file on purpose.
 */
const cases: [string, Parameters<typeof activity>[0], Activity["key"], Activity["tone"]][] = [
  ["bars arriving", { enabled: true, feed: { healthy: true, market_open: true } }, "working", "go"],
  ["market closed", { enabled: true, feed: { healthy: true, market_open: false } }, "idle", "consequence"],
  ["feed unhealthy", { enabled: true, feed: { healthy: false, market_open: true } }, "failure", "alarm"],
  ["halted by operator", { enabled: true, control: "halted" }, "halted", "quiet"],
  ["stopped by operator", { enabled: true, control: "stopped" }, "stopped", "quiet"],
  ["disabled in registry", { enabled: false }, "disabled", "quiet"],
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
  expect(activity({ enabled: true, feed: { healthy: false, market_open: false } }).key).toBe("failure");
});

test("the control word outranks a failing feed", () => {
  // A halted bot is not going to trade whatever the feed does; leading
  // with the fault would imply there is something to fix first.
  expect(activity({ enabled: true, control: "halted", feed: { healthy: false } }).key).toBe("halted");
});

test("Halt is patient, Stop is not", () => {
  // Halt only prevents the NEXT entry, so noticing it a cycle later costs
  // nothing — "halting" stays a quiet transition however long it takes.
  expect(activity({ enabled: true, control: "halted", unackSeconds: 99_999 }).key).toBe("halted");
  expect(activity({ enabled: true, control: "halted", unackSeconds: 99_999 }).tone).toBe("quiet");

  // Stop means "be flat". Briefly in flight is a transition...
  expect(activity({ enabled: true, control: "stopped", unackSeconds: 3 }).key).toBe("stopping");
  // ...but past the grace nothing is listening, and the operator is
  // carrying risk they believe they cancelled. That is a fault, in red.
  const stuck = activity({ enabled: true, control: "stopped", unackSeconds: 600 });
  expect(stuck.key).toBe("stop-not-applied");
  expect(stuck.tone).toBe("alarm");
});

test("a bot with no feed is never called idle", () => {
  // Crypto is 24/7 and reports no market, so absent feed health there is
  // no quiet period to claim.
  expect(activity({ enabled: true }).key).toBe("working");
});
