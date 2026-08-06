-- Identity registries (architecture-review-multibot.md step 1, 2026-08-06).
-- DB-FIRST by operator mandate: ai-trader deploys to the triggerlab
-- cluster, so bots/venues/accounts/bindings are authoritative HERE, never
-- in files. The DB stores credential REFERENCES (which env/SOPS name),
-- never secrets.

CREATE TABLE venues (
    venue_id    text PRIMARY KEY,          -- 'paper', 'hyperliquid', 'ib'
    kind        text NOT NULL,             -- 'exchange' | 'broker' | 'sim'
    asset_classes text[] NOT NULL,         -- {'crypto'}, {'futures'}, ...
    notes       text,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE accounts (
    account_id  text PRIMARY KEY,          -- operator-chosen slug
    venue_id    text NOT NULL REFERENCES venues,
    -- reference into SOPS/env, e.g. 'HL_LIVE' -> HL_LIVE_* variables.
    credential_ref text NOT NULL,
    -- 'paper' | 'live'; a live account never doubles as a paper one.
    kind        text NOT NULL CHECK (kind IN ('paper', 'live')),
    quote_currency text NOT NULL,
    notes       text,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE bots (
    bot_id      text PRIMARY KEY,          -- '^[a-z0-9][a-z0-9-]{1,63}$'
    display_name text NOT NULL,
    -- 'cron' | 'bar_close' | 'stream' — how the decision core is driven.
    cadence     text NOT NULL,
    asset_class text NOT NULL,
    -- where the decision core lives (repo/module), for humans.
    decision_core text NOT NULL,
    enabled     boolean NOT NULL DEFAULT false,   -- fail closed
    notes       text,
    created_at  timestamptz NOT NULL DEFAULT now()
);

-- Which bot may trade which account, with what authority. One bot may have
-- several bindings (paper + live); an account SHOULD have at most one bot
-- until per-fill attribution is proven end to end (review finding 2).
CREATE TABLE account_bindings (
    bot_id      text NOT NULL REFERENCES bots,
    account_id  text NOT NULL REFERENCES accounts,
    -- 'readonly' | 'trade'; mirrors the process split (section 3.3).
    scope       text NOT NULL CHECK (scope IN ('readonly', 'trade')),
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (bot_id, account_id)
);

-- Known venues exist from day one; accounts and bots are registered by the
-- operator (via `bot identity register` or SQL).
INSERT INTO venues (venue_id, kind, asset_classes, notes) VALUES
    ('paper',       'sim',      '{crypto}',  'fake broker over a live feed'),
    ('hyperliquid', 'exchange', '{crypto}',  'perps; EIP-712 signing'),
    ('ib',          'broker',   '{futures}', 'Interactive Brokers (planned: futures-noise bot)');
