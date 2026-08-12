import { z } from "zod";

/** The api's JSON, described once. Every screen reads these types, so a
 *  field that changes shape breaks the build rather than a page at 3am. */

export const Broker = z.object({
  venue_id: z.string().nullable().optional(),
  protocol: z.string().nullable().optional(),
  account_id: z.string().nullable().optional(),
  kind: z.string().nullable().optional(),
  credential_ref: z.string().nullable().optional(),
  options: z
    .array(
      z.object({
        account_id: z.string(),
        venue_id: z.string(),
        protocol: z.string().optional(),
        kind: z.string(),
        credential_ref: z.string().optional(),
      }),
    )
    .optional(),
});
export type Broker = z.infer<typeof Broker>;

/** What the feed is doing. The bot judges its own health — it knows
 *  whether the market should be printing right now. */
export const FeedHealth = z.object({
  source: z.string().optional(),
  last_bar_utc: z.string().nullable().optional(),
  last_bar_age_seconds: z.number().nullable().optional(),
  failures: z.number().optional(),
  last_error: z.string().nullable().optional(),
  market_open: z.boolean().optional(),
  healthy: z.boolean().optional(),
});
export type FeedHealth = z.infer<typeof FeedHealth>;

export const Status = z.object({
  feed: FeedHealth.optional(),
  contract: z.string().optional(),
  mode: z.string().nullable().optional(),
  halted: z.unknown().optional(),
  kill_switch: z.boolean().nullable().optional(),
  control_state: z.string().optional(),
  control_set_at: z.string().optional(),
  heartbeat_age_seconds: z.number().nullable().optional(),
  net_total: z.number().nullable().optional(),
  trades_total: z.number().nullable().optional(),
  last_run: z
    .object({ outcome: z.string().nullable().optional(), recorded_at: z.string().nullable().optional() })
    .nullable()
    .optional(),
});
export type Status = z.infer<typeof Status>;

export const BotSummary = z.object({
  bot_id: z.string(),
  display_name: z.string(),
  cadence: z.string(),
  asset_class: z.string(),
  enabled: z.boolean(),
  local: z.boolean().optional(),
  status: Status.optional(),
  broker: Broker.optional(),
  series: z.array(z.tuple([z.string(), z.union([z.number(), z.string()])])).optional(),
  series_kind: z.string().optional(),
});
export type BotSummary = z.infer<typeof BotSummary>;

export const FeedItem = z.object({
  at: z.string().nullable().optional(),
  bot_id: z.string(),
  kind: z.string().nullable().optional(),
  text: z.string().nullable().optional(),
});

export const Overview = z.object({
  available: z.boolean().optional(),
  bots: z.array(BotSummary).default([]),
  feed: z.array(FeedItem).default([]),
});
export type Overview = z.infer<typeof Overview>;

export const Sleeve = z.object({
  enabled: z.boolean().optional(),
  in_position: z.boolean().optional(),
  direction: z.string().nullable().optional(),
  session: z.string().nullable().optional(),
  trades_total: z.number().optional(),
  net_total: z.number().optional(),
});
export type Sleeve = z.infer<typeof Sleeve>;

export const Fill = z.object({
  sleeve: z.string().optional(),
  session_date: z.string().optional(),
  direction: z.string().optional(),
  entry: z.number().optional(),
  exit: z.number().optional(),
  exit_ts: z.string().optional(),
  dollars: z.number().optional(),
  reason: z.string().optional(),
});
export type Fill = z.infer<typeof Fill>;

export const BotDetail = z.object({
  contract: z.string(),
  source: z.string().optional(),
  display_name: z.string().optional(),
  cadence: z.string().optional(),
  asset_class: z.string().optional(),
  enabled: z.boolean().optional(),
  heartbeat_age_seconds: z.number().nullable().optional(),
  broker: Broker.optional(),
  state: z
    .object({
      mode: z.string().nullable().optional(),
      state: z.string().nullable().optional(),
      state_reason: z.unknown().optional(),
      headline: z.object({ net: z.number().optional(), fills: z.number().optional() }).optional(),
      detail: z
        .object({
          instrument: z.string().optional(),
          sizing: z.object({ mode: z.string().optional(), units: z.number().optional() }).optional(),
          kill: z
            .object({
              rolling_sessions: z.number().optional(),
              rolling_net: z.number().optional(),
              limit: z.number().optional(),
            })
            .optional(),
          sleeves: z.record(Sleeve).optional(),
          trades_total: z.number().optional(),
          net_total: z.number().optional(),
          // Without this the schema SILENTLY DROPPED it: zod strips keys it
          // was not told about, so the dashboard reported "feed: not
          // reported" while the bot was publishing feed health every cycle.
          feed: FeedHealth.optional(),
          note: z.string().optional(),
        })
        .optional(),
    })
    .optional(),
  fills: z.array(Fill).optional(),
  controls: z.record(z.unknown()).nullable().optional(),
  runs: z.array(z.record(z.unknown())).optional(),
});
export type BotDetail = z.infer<typeof BotDetail>;
