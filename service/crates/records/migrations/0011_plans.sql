-- The decision itself, kept beside the run that executed it.
--
-- A run record says what was traded and what the risk checks allowed. The PLAN
-- says what the model asked for: every target weight and conviction, the
-- provenance (model id, inputs hash, whether a constructor fell back), the
-- predicted cost, and the warnings — including how much weight the turnover
-- budget deferred. All of that lived in one file that the next cycle
-- overwrote, so `plan_id` on a run pointed at something already deleted, and
-- only the most recent decision could ever be replayed.
--
-- Separate from `runs` rather than embedded in its payload because the api
-- serves the twenty most recent runs verbatim on every page load, and a plan
-- is ~10KB. One row a day is under 4MB a year; twenty of them in every
-- response is not.
CREATE TABLE IF NOT EXISTS plans (
    bot_id      TEXT        NOT NULL,
    plan_id     TEXT        NOT NULL,
    -- The run that executed it, when one did. A plan refused by a gate never
    -- gets one, and is worth keeping precisely because it was refused.
    run_id      TEXT,
    as_of       TIMESTAMPTZ NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL,
    payload     JSONB       NOT NULL,
    PRIMARY KEY (bot_id, plan_id)
);

-- Answering "what did it decide that day" without scanning the table.
CREATE INDEX IF NOT EXISTS plans_bot_as_of ON plans (bot_id, as_of DESC);
CREATE INDEX IF NOT EXISTS plans_bot_run ON plans (bot_id, run_id);
