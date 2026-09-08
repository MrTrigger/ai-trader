import { expect, test } from "bun:test";
import { bookUrl, parseBook } from "./book";

const view = {
  bot_id: "crypto-lighter",
  book: { nav: "100000", cash: "100000", total_pnl: "0", unrealised_pnl: "0", realised_pnl: "0",
    fees_paid: "0", gross_exposure: "0", net_exposure: "0", fills_count: 0, positions: [] },
  runs: [], health: { ok: true, notes: [] }, benchmark: null, reconciliation: null,
};

test("each crypto book has a distinct endpoint and checked identity", () => {
  expect(bookUrl("crypto-lighter")).toBe("/api/bots/crypto-lighter/book");
  expect(bookUrl("crypto-portfolio")).toBe("/api/bots/crypto-portfolio/book");
  expect(parseBook(view, "crypto-lighter").book.nav).toBe("100000");
  expect(() => parseBook(view, "crypto-portfolio")).toThrow("another bot");
  expect(() => parseBook({ fleet_only: true }, "crypto-lighter")).toThrow();
});
