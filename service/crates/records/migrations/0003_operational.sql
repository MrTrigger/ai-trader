-- Step 4 (architecture-review-multibot.md): operational records move to
-- Postgres. The pod's filesystem is ephemeral; the DB is the only store
-- that survives a restart or reschedule, and the one place the control
-- plane reads. Every row is keyed by bot_id, and every table carries the
-- bot's own document verbatim in `payload` next to a few indexed columns —
-- the document shapes are the proven contract (state.json, journal lines,
-- RunRecord, ledger entries), the columns exist so the dashboard can query
-- without parsing every blob.

-- One row per completed run / session record. The runner's RunRecord and
-- any future bot's equivalent both land here; `outcome` is the triage
-- column ("executed", "halted", "refused", ...).
CREATE TABLE runs (
    bot_id      text        NOT NULL REFERENCES bots (bot_id),
    run_id      text        NOT NULL,
    recorded_at timestamptz NOT NULL,
    outcome     text        NOT NULL,
    payload     jsonb       NOT NULL,
    PRIMARY KEY (bot_id, run_id)
);
CREATE INDEX runs_bot_recent ON runs (bot_id, recorded_at DESC);

-- The append-only order-authorisation ledger (runner/src/ledger.rs).
-- Append-only is enforced by usage, not the schema; `seq` preserves the
-- exact order entries were written, per bot.
CREATE TABLE ledger_entries (
    seq             bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    bot_id          text        NOT NULL REFERENCES bots (bot_id),
    at              timestamptz NOT NULL,
    kind            text        NOT NULL,
    client_order_id text,
    payload         jsonb       NOT NULL
);
CREATE INDEX ledger_bot_seq ON ledger_entries (bot_id, seq);

-- Economically meaningful executions — the futures journal lines today,
-- venue fills for other bots as they land. `fill_key` is a caller-derived
-- idempotency key so replays and crash-recovery reruns never double-insert.
CREATE TABLE fills (
    bot_id     text        NOT NULL REFERENCES bots (bot_id),
    fill_key   text        NOT NULL,
    at         timestamptz NOT NULL,
    instrument text,
    sleeve     text,
    side       text,
    qty        numeric,
    price      numeric,
    pnl        numeric,
    reason     text,
    payload    jsonb       NOT NULL,
    PRIMARY KEY (bot_id, fill_key)
);
CREATE INDEX fills_bot_recent ON fills (bot_id, at DESC);

-- Control is an append-only event stream; the newest row per bot is the
-- current word. Absent rows mean HALTED — the fail-closed default every
-- reader must apply. `payload` carries the full control document in the
-- bot's own dialect (kill_switch/paused for runner bots, halted/sleeves
-- for the futures contract); `state` is the indexed summary.
CREATE TABLE control_events (
    seq     bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    bot_id  text        NOT NULL REFERENCES bots (bot_id),
    at      timestamptz NOT NULL DEFAULT now(),
    state   text        NOT NULL,
    reason  text        NOT NULL,
    set_by  text        NOT NULL,
    payload jsonb       NOT NULL DEFAULT '{}'::jsonb
);
CREATE INDEX control_bot_latest ON control_events (bot_id, seq DESC);

-- The bot's published state document (was state.json): heartbeat plus
-- whatever the dashboard shows. Latest wins; history is not kept — runs
-- and fills are the history.
CREATE TABLE bot_status (
    bot_id       text        PRIMARY KEY REFERENCES bots (bot_id),
    heartbeat_at timestamptz NOT NULL,
    payload      jsonb       NOT NULL
);

-- Crash-recovery snapshot (was runtime-snapshot.json): everything a bot
-- needs to resume mid-session after its process dies. Latest wins.
CREATE TABLE snapshots (
    bot_id   text        PRIMARY KEY REFERENCES bots (bot_id),
    taken_at timestamptz NOT NULL,
    payload  jsonb       NOT NULL
);

-- Simulated venue state for paper trading (was venue-state.json), keyed
-- by bot: each bot owns its paper book today, and that book must survive
-- a pod reschedule exactly like a real venue's would.
CREATE TABLE venue_sim_state (
    bot_id     text        PRIMARY KEY REFERENCES bots (bot_id),
    updated_at timestamptz NOT NULL,
    payload    jsonb       NOT NULL
);
