import { ago } from "../lib/format";

export type Feed = {
  source?: string;
  last_bar_age_seconds?: number | null;
  failures?: number;
  last_error?: string | null;
  market_open?: boolean;
  healthy?: boolean;
};

/**
 * Whether data is arriving — the thing that used to be invisible.
 *
 * A dead feed and a quiet market produce the same screen: no fills, no
 * errors, state "running". They are not the same situation and must not
 * look alike, so the feed reports its own age, its source, and the error
 * that stopped it.
 */
export function FeedStatus({ feed, compact = false }: { feed?: Feed; compact?: boolean }) {
  if (!feed) {
    return <span className="num text-[11px] text-faint">feed: not reported</span>;
  }
  const age = feed.last_bar_age_seconds ?? null;
  const down = (feed.failures ?? 0) > 0;
  const stale = feed.market_open && (age == null || age > 900);
  const tone = down || stale ? "text-alarm" : feed.market_open ? "text-go" : "text-dim";
  const label = down
    ? `feed down · ${feed.failures} failed poll${feed.failures === 1 ? "" : "s"}`
    : age == null
      ? "no bars yet"
      : `last bar ${ago(age)} ago`;

  if (compact) return <span className={`num text-[11px] ${tone}`}>{label}</span>;

  return (
    <div>
      <div className="flex items-baseline gap-2">
        <span className={`num text-[13px] ${tone}`}>{label}</span>
        <span className="text-[11px] text-faint">
          {feed.source === "realtime" ? "realtime stream" : "polled bars"}
          {feed.market_open === false && " · market closed"}
        </span>
      </div>
      {feed.last_error && (
        <p className="mt-1.5 break-words font-mono text-[11px] leading-relaxed text-alarm/90">
          {feed.last_error}
        </p>
      )}
      {!down && stale && (
        <p className="mt-1.5 text-[11px] leading-relaxed text-alarm/90">
          The market should be printing and nothing has arrived. The bot halts itself if this
          continues.
        </p>
      )}
    </div>
  );
}
