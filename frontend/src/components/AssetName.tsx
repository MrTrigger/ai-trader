import { chartUrl } from "../lib/venue";

/**
 * A symbol, linked to its chart on the venue the bot trades it on.
 *
 * Plain text where there is no chart to point at, rather than an anchor that
 * goes nowhere: every row of these tables is a symbol, so a dead link here is
 * a dead link twenty times over.
 */
export function AssetName({ asset, venue }: { asset: string; venue?: string }) {
  const href = chartUrl(venue, asset);
  if (!href) return <span className="font-display text-ink">{asset}</span>;
  return (
    <a
      href={href}
      target="_blank"
      rel="noreferrer noopener"
      title={`${asset} chart on ${venue}`}
      className="font-display text-ink underline decoration-line2 decoration-dotted underline-offset-[3px] hover:decoration-ink focus:decoration-ink focus:outline-none"
    >
      {asset}
    </a>
  );
}
