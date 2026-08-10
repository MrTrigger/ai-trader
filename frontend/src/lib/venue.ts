/**
 * Where to look at a symbol's chart, for the venue the bot actually trades on.
 *
 * Returns null for anything we cannot address. A link that goes to the wrong
 * venue's page for a symbol that happens to share a ticker is worse than no
 * link, and a dead link on every row of a table is worse than plain text — so
 * callers render text when this returns null rather than a disabled anchor.
 */
export function chartUrl(venue: string | undefined, asset: string): string | null {
  if (!asset) return null;
  switch (venue) {
    case "hyperliquid":
      return `https://app.hyperliquid.xyz/trade/${encodeURIComponent(asset)}`;
    default:
      return null;
  }
}
