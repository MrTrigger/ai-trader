import type { FeedHealth } from "../api/types";

/**
 * What the bot is DOING — as opposed to what it was told, or what it is
 * wired to do.
 *
 * Three different facts kept getting rendered as one word:
 *
 *   control     running | halted | stopped   — what the operator set
 *   capability  orders armed | shadow | ...  — what the wiring permits
 *   activity    working | idle | degraded    — what is happening now
 *
 * The card showed the control word in green and let it stand for all
 * three. So a bot with a dead feed read "running", and so did a bot on a
 * Saturday with no bar due since Friday — the same green as one actually
 * trading. Control and capability are both answerable while the market is
 * shut, and both must stay answerable; activity is the one that changes
 * minute to minute, and it was the one missing.
 *
 * Idle is deliberately GO, not a warning. A closed market is the system
 * working correctly, and colouring it amber would train the eye to
 * discount amber by Monday — the boy who cried wolf, in a palette.
 */
export type Activity = {
  key: "disabled" | "halted" | "degraded" | "idle" | "working";
  /** Fleet-card length. */
  label: string;
  /** Bot-page header length; keeps the control word visible. */
  long: string;
  tone?: "go" | "alarm" | "consequence";
};

export function activity(a: {
  enabled?: boolean;
  halted?: boolean;
  feed?: FeedHealth;
}): Activity {
  if (a.enabled === false) return { key: "disabled", label: "disabled", long: "disabled" };
  if (a.halted) return { key: "halted", label: "halted", long: "halted", tone: "alarm" };

  // A feed that reports itself unhealthy outranks everything below: the bot
  // cannot know whether it should be trading if it cannot see.
  if (a.feed && a.feed.healthy === false) {
    return { key: "degraded", label: "degraded", long: "running · degraded", tone: "consequence" };
  }

  // Nothing is due. Only claimable for bots that report a market: a 24/7
  // venue is never idle for want of a session, so absent feed health this
  // stays "running" rather than inventing a quiet period.
  if (a.feed && a.feed.market_open === false) {
    return { key: "idle", label: "idle", long: "running · idle", tone: "go" };
  }

  return { key: "working", label: "running", long: "running", tone: "go" };
}
