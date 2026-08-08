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
 * Colour answers "does this want me?", and each one means one thing:
 *
 *   green   working as deployed
 *   yellow  running but not working — no fault, and no work either
 *   red     failing
 *   grey    deliberately not running
 *
 * So halted and disabled are grey rather than red: the operator chose
 * them, and red spent on a chosen state is red that no longer means
 * something broke. Idle is yellow for the converse reason — nothing is
 * wrong, but a bot that isn't trading isn't doing the job it was
 * deployed for, and that should not wear the same green as one that is.
 */
export type Activity = {
  key: "disabled" | "stopped" | "halted" | "failure" | "idle" | "working";
  /** Fleet-card length. */
  label: string;
  /** Bot-page header length; keeps the control word visible. */
  long: string;
  tone?: "quiet" | "go" | "alarm" | "consequence";
};

export function activity(a: {
  enabled?: boolean;
  /** The canonical control word: running | halted | stopped. */
  control?: string | null;
  /**
   * The control was set after the bot last published, so the bot has not
   * seen it yet. Worth its own word: "stopped" and "stopping" are
   * different promises, and a cron bot can sit in the second for hours.
   */
  pending?: boolean;
  feed?: FeedHealth;
}): Activity {
  if (a.enabled === false) {
    return { key: "disabled", label: "disabled", long: "disabled", tone: "quiet" };
  }
  // Control outranks everything the bot reports about itself: the operator
  // has said what should happen, and a bot that has not caught up yet must
  // not be drawn as though nothing was asked. The page previously read a
  // `kill_switch` field that the schema-1 document does not have, so Stop
  // resolved to false and the pill stayed green on a stopped bot.
  if (a.control === "stopped") {
    const label = a.pending ? "stopping" : "stopped";
    return { key: "stopped", label, long: label, tone: "quiet" };
  }
  if (a.control === "halted") {
    const label = a.pending ? "halting" : "halted";
    return { key: "halted", label, long: label, tone: "quiet" };
  }

  // A feed that reports itself unhealthy outranks everything below: the bot
  // cannot know whether it should be trading if it cannot see. Not
  // "degraded" — that word suggests reduced service, and there is no
  // service: no bars means no decisions at all.
  if (a.feed && a.feed.healthy === false) {
    return { key: "failure", label: "failure", long: "running · failure", tone: "alarm" };
  }

  // Nothing is due. Only claimable for bots that report a market: a 24/7
  // venue is never idle for want of a session, so absent feed health this
  // stays "running" rather than inventing a quiet period.
  if (a.feed && a.feed.market_open === false) {
    return { key: "idle", label: "idle", long: "running · idle", tone: "consequence" };
  }

  return { key: "working", label: "running", long: "running", tone: "go" };
}
