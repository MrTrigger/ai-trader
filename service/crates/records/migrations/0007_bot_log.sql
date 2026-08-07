-- What the bot said, where the operator can read it.
--
-- The bot's log was a file next to the process. In a pod that file is
-- unreachable and dies with the container, and even locally it meant the
-- only way to learn "the feed has been failing for an hour" was to ssh in
-- and tail it — while the dashboard cheerfully said "running". Operational
-- narration belongs in the same store as every other operational record.
--
-- Deliberately not a firehose: bots write the lines a human would want at
-- 3am (feed state changes, halts, fills, session rolls, refusals), not a
-- debug stream. `seq` orders lines written inside the same second.

CREATE TABLE bot_log (
    seq    bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    bot_id text        NOT NULL REFERENCES bots (bot_id),
    at     timestamptz NOT NULL DEFAULT now(),
    -- info | warn | error. The dashboard colours on this, so it is a
    -- column rather than a prefix inside the message.
    level  text        NOT NULL DEFAULT 'info',
    line   text        NOT NULL
);
CREATE INDEX bot_log_recent ON bot_log (bot_id, seq DESC);
