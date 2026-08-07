-- The instrument universe, as the venue reports it.
--
-- This replaces a hand-maintained `markets` array in each bot's JSON config.
-- That list was a second, stale copy of something the venue already knows: it
-- refused AAVE because nobody had typed it in, and would have accepted a lot
-- size the exchange changed months ago. A venue's own metadata is the only
-- authority on what it lists, and it belongs in the store every process reads,
-- not in a file beside one of them.
--
-- Keyed by venue rather than by bot: two bots on Hyperliquid see the same
-- exchange, and giving them separate copies is how the copies drift.
CREATE TABLE IF NOT EXISTS venue_markets (
    venue_id      TEXT NOT NULL,
    asset         TEXT NOT NULL,
    venue_symbol  TEXT NOT NULL,
    quote_currency TEXT NOT NULL,
    tick          NUMERIC NOT NULL,
    lot           NUMERIC NOT NULL,
    min_notional  NUMERIC NOT NULL,
    multiplier    NUMERIC NOT NULL DEFAULT 1,
    asset_class   TEXT NOT NULL DEFAULT 'crypto',
    refreshed_at  TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (venue_id, asset)
);

-- The account book: cash and positions, as the venue reports them.
--
-- The planner used to size every plan against the cash written in its own
-- data/book.json, so a 30k account was handed a plan built for 100k and ran
-- out of money nine orders in. Two books that are both authoritative is one
-- book too many.
CREATE TABLE IF NOT EXISTS account_book (
    bot_id        TEXT PRIMARY KEY,
    quote_currency TEXT NOT NULL,
    cash          NUMERIC NOT NULL,
    positions     JSONB NOT NULL,
    refreshed_at  TIMESTAMPTZ NOT NULL
);
