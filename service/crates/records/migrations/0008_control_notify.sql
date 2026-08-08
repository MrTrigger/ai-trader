-- A control press should not wait to be discovered.
--
-- The bot learned about Stop by polling control_events every few seconds,
-- which put a poll interval between the operator and their own kill
-- switch. Postgres can push: every control write now NOTIFYs, and a bot
-- LISTENing wakes in milliseconds. The poll survives as a fallback for a
-- listener connection that has died — belt and braces, in that order.
--
-- The payload is the bot_id so one channel serves the whole fleet.

CREATE OR REPLACE FUNCTION notify_bot_control() RETURNS trigger AS $$
BEGIN
    PERFORM pg_notify('bot_control', NEW.bot_id);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER control_events_notify
    AFTER INSERT ON control_events
    FOR EACH ROW EXECUTE FUNCTION notify_bot_control();
