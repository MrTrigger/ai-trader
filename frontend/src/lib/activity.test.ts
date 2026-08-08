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

test("a control the bot has not seen yet says so", () => {
  // Stop on a cron bot can sit unacknowledged for hours. "stopped" would
  // promise the book is closed; it is not, and the operator must know
  // which of the two they are looking at.
  expect(activity({ enabled: true, control: "stopped", pending: true }).label).toBe("stopping");
  expect(activity({ enabled: true, control: "halted", pending: true }).label).toBe("halting");
});

test("a bot with no feed is never called idle", () => {
  // Crypto is 24/7 and reports no market, so absent feed health there is
  // no quiet period to claim.
  expect(activity({ enabled: true }).key).toBe("working");
});
