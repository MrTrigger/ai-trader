-- How to start this bot, recorded where everything else about it lives.
--
-- Stop now means the PROCESS exits (flatten, publish a final state, quit)
-- rather than idling in a loop that burns a connection and a heartbeat on
-- doing nothing. That makes Start a real verb: something has to launch a
-- new process, and that something is the api — it is the one long-lived
-- resident of the pod, which makes it the supervisor by topology, not by
-- ambition. It can only do that if the launch command is data.
--
-- NULL means the api cannot start this bot (crypto's cron lives outside);
-- the UI already says so honestly.

ALTER TABLE bots ADD COLUMN launch text;

UPDATE bots SET launch = 'bots/futures/run.sh live' WHERE bot_id = 'futures-noise';
