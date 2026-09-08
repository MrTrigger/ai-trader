import { z } from "zod";

const amount = z.union([z.string(), z.number()]);
const optionalAmount = amount.nullish().transform((v) => v ?? undefined);
const BookView = z.object({
  bot_id: z.string(),
  book: z.object({
    nav: amount,
    cash: amount,
    total_pnl: amount,
    unrealised_pnl: amount,
    realised_pnl: amount,
    fees_paid: amount,
    gross_exposure: amount,
    net_exposure: amount,
    fills_count: z.number(),
    positions: z.array(z.object({
      asset: z.string(),
      qty: amount,
      avg_price: optionalAmount,
      mark: optionalAmount,
      notional: optionalAmount,
      weight: z.number().nullish().transform((v) => v ?? undefined),
      unrealised_pnl: optionalAmount,
      side: z.string().optional(),
    })),
  }),
  runs: z.array(z.object({
    recorded_at: z.string(),
    nav: optionalAmount,
    outcome: z.string(),
  })),
  benchmark: z.object({ label: z.string(), points: z.array(z.tuple([z.string(), z.number()])) }).nullish(),
  reconciliation: z.object({ agrees: z.boolean().optional(), explain: z.string().optional() }).nullish(),
  health: z.object({ ok: z.boolean(), notes: z.array(z.string()) }),
});

export function parseBook(value: unknown, botId: string) {
  const book = BookView.parse(value);
  if (book.bot_id !== botId) throw new Error("Book response belongs to another bot");
  return book;
}

export function bookUrl(botId: string) {
  return `/api/bots/${encodeURIComponent(botId)}/book`;
}

export async function fetchBook(botId: string) {
  const response = await fetch(bookUrl(botId), { headers: { accept: "application/json" } });
  if (!response.ok) throw new Error(`Book unavailable (HTTP ${response.status})`);
  return parseBook(await response.json(), botId);
}
