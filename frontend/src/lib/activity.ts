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
  key:
    | "disabled"
    | "unset"
    | "stopped"
    | "stopping"
    | "stop-not-applied"
    | "halted"
    | "halting"
    | "halt-not-applied"
    | "starting"
    | "not-running"
    | "failure"
    | "idle"
    | "working";
  /** Fleet-card length. */
  label: string;
  /** Bot-page header length; keeps the control word visible. */
  long: string;
  tone?: "quiet" | "go" | "alarm" | "consequence";
};

/**
 * How long a Stop may sit unapplied before it is a fault rather than a
 * transition. The bot reads controls every few seconds, so anything past
 * this means nothing is listening.
 */
export const STOP_GRACE_S = 30;

/**
 * How long any other control may sit unacknowledged before we stop calling
 * it a transition. Nothing is at risk while a Halt or a Start is in
 * flight, so this is looser than the Stop grace — but past it, the
 * instruction is not being carried out and the page must not pretend
 * otherwise.
 */
export const APPLY_GRACE_S = 120;

/**
 * How long the operator's control word has gone unacknowledged, or `undefined`
 * once the bot has taken it up.
 *
 * This lived inline in the bot page, which had two consequences and both of
 * them were lies on the screen. The fleet card never computed it at all, so
 * every card with a `running` control was green whatever was happening. And
 * the page's version could only tell by comparing the control's timestamp
 * against the bot's last publish — so when there had NEVER been a publish, the
 * comparison was impossible and the code read that as acknowledged. A bot in
 * the cluster that had never once started drew a green RUNNING pill next to
 * its own "no heartbeat", and next to a red panel saying nothing had ever run
 * it. Never published is the strongest evidence there is that nothing has
 * taken the control up; it must fail closed, not open.
 */
export function unackSeconds(a: {
  /** The control word: running | halted | stopped. */
  control?: string | null;
  /** When the operator set it (ISO 8601). */
  setAt?: string | null;
  /** The state the bot itself last published, if it publishes one. */
  publishedState?: string | null;
  /** Age of that publish. `null`/absent means it has never published. */
  heartbeatAgeSeconds?: number | null;
  now?: number;
}): number | undefined {
  if (!a.control || !a.setAt) return undefined;
  const setAt = Date.parse(a.setAt);
  if (Number.isNaN(setAt)) return undefined;
  const now = a.now ?? Date.now();

  // Ask the bot first: a published state naming the control's outcome IS the
  // acknowledgement, and needs no clocks.
  const semanticAck =
    a.control === "running"
      ? a.publishedState === "running"
      : a.control === "stopped" || a.control === "halted"
        ? a.publishedState === "halted"
        : false;
  if (semanticAck) return undefined;

  const pending = now - setAt;
  // Never published: nothing can have taken this up.
  if (a.heartbeatAgeSeconds == null) return Math.max(0, pending / 1000);

  // Timestamp fallback, with a margin WIDER than its own noise. The ack
  // publish lands ~0.3s after set_at (the control is pushed), but the
  // heartbeat age is an integer, so now-minus-age carries up to a second of
  // rounding — more than the gap being measured. Without the margin, an
  // acknowledged Stop could read as pending on a coin flip.
  const lastPublish = now - a.heartbeatAgeSeconds * 1000;
  return setAt > lastPublish + 2_000 ? Math.max(0, pending / 1000) : undefined;
}

export function activity(a: {
  enabled?: boolean;
  /** The canonical control word: running | halted | stopped. */
  control?: string | null;
  /**
   * Seconds the control has gone unacknowledged — the bot has not
   * published since it was set. Undefined once it has.
   *
   * The two verbs need different patience. Halt only prevents the NEXT
   * entry, so noticing it a cycle later costs nothing and "halting" is
   * simply true. Stop means "be flat", the bot reads controls every few
   * seconds, and an unapplied Stop is market risk the operator believes
   * they already cancelled — so it stops being a pending state and
   * becomes a fault.
   */
  unackSeconds?: number;
  feed?: FeedHealth;
}): Activity {
  if (a.enabled === false) {
    return { key: "disabled", label: "disabled", long: "disabled", tone: "quiet" };
  }
  // No control word at all. The canonical contract says unknown reads as
  // halted — fail closed — and this derivation had no case for it, so it fell
  // through every branch below to the most optimistic answer there is: a green
  // RUNNING pill. The first bot ever to be in this state was a freshly
  // registered one in the cluster, which had never run and never been told to,
  // and the page announced it as running with "no heartbeat" beside it.
  if (a.control == null) {
    return { key: "unset", label: "not started", long: "not started", tone: "quiet" };
  }
  // Control outranks everything the bot reports about itself: the operator
  // has said what should happen, and a bot that has not caught up yet must
  // not be drawn as though nothing was asked. The page previously read a
  // `kill_switch` field that the schema-1 document does not have, so Stop
  // resolved to false and the pill stayed green on a stopped bot.
  if (a.control === "stopped") {
    if (a.unackSeconds == null) {
      return { key: "stopped", label: "stopped", long: "stopped", tone: "quiet" };
    }
    if (a.unackSeconds < STOP_GRACE_S) {
      return { key: "stopping", label: "stopping", long: "stopping", tone: "quiet" };
    }
    return {
      key: "stop-not-applied",
      label: "not stopped",
      long: "stop not applied",
      tone: "alarm",
    };
  }
  if (a.control === "halted") {
    if (a.unackSeconds == null) {
      return { key: "halted", label: "halted", long: "halted", tone: "quiet" };
    }
    if (a.unackSeconds < APPLY_GRACE_S) {
      return { key: "halting", label: "halting", long: "halting", tone: "quiet" };
    }
    return { key: "halt-not-applied", label: "not halted", long: "halt not applied", tone: "alarm" };
  }

  // An unacknowledged RUNNING is the one that got away, twice. Nothing
  // above matches it and there is no feed to fault, so it fell through to
  // "working" and drew a green RUNNING pill on a bot that had not run in a
  // day. Pressing Start does not make a bot run — it records that it
  // should — and until something picks that up, saying "running" is the
  // same lie the feed used to tell.
  if (a.control === "running" && a.unackSeconds != null) {
    if (a.unackSeconds < APPLY_GRACE_S) {
      return { key: "starting", label: "starting", long: "starting", tone: "quiet" };
    }
    return { key: "not-running", label: "not running", long: "not running", tone: "alarm" };
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
