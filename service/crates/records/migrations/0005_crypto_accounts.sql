-- Every bot picks its broker from the same table.
--
-- The crypto bot was the odd one out: it named its venue in its own config
-- file, so the fleet showed it as "unbound" while the futures bot showed a
-- real broker. That is a difference in plumbing, not in kind — Hyperliquid
-- is its broker exactly as AMP is the futures bot's — so it gets registry
-- accounts like everything else and the UI stops special-casing it.
--
-- Two accounts, because the crypto bot's two states are the same money
-- question every other bot answers: simulated fills over a live feed, or
-- the real account.

INSERT INTO accounts (account_id, venue_id, kind, credential_ref, quote_currency) VALUES
  ('hl-sim',  'hyperliquid', 'paper', 'HL_READONLY', 'USDC'),
  ('hl-live', 'hyperliquid', 'live',  'HL_AGENT',    'USDC')
ON CONFLICT (account_id) DO NOTHING;

-- Its current state: simulated fills. Recorded, not assumed.
INSERT INTO account_bindings (bot_id, account_id, scope)
SELECT 'crypto-portfolio', 'hl-sim', 'trade'
 WHERE EXISTS (SELECT 1 FROM bots WHERE bot_id = 'crypto-portfolio')
ON CONFLICT (bot_id, account_id) DO NOTHING;

-- Asset classes are what make a broker offerable to a bot: a futures book
-- must never be shown a crypto venue, and vice versa. The seeds carried
-- these already; this makes the futures pair explicit and complete.
UPDATE venues SET asset_classes = '{futures}' WHERE venue_id IN ('ib', 'amp');
UPDATE venues SET asset_classes = '{crypto}'  WHERE venue_id IN ('hyperliquid', 'paper');
