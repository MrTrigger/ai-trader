-- Where each bot publishes its operational state (step-4's Postgres records
-- will supersede this; until then the multi-bot UI reads per-bot state from
-- here). Relative to the ai-trader repo root.
ALTER TABLE bots ADD COLUMN state_dir text;
