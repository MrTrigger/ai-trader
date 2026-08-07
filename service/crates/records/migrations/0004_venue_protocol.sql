-- A venue is WHO holds the account; the protocol is HOW we reach them.
--
-- The first cut of the futures work conflated the two and registered a
-- venue called "rithmic". Rithmic is not a broker — it is a connectivity
-- platform. AMP Futures is the FCM: it holds the account, clears the
-- trades, and issues the Rithmic credentials. Several other brokers
-- (Ironbeam, Optimus, ...) are also reached through Rithmic, so the name
-- would have been wrong the moment a second one appeared.
--
-- Splitting them means adding a broker later is a registry row rather
-- than a code change: the adapter is selected by `protocol`, and the
-- operator picks a `venue_id` that names a company they have an account
-- with.

ALTER TABLE venues ADD COLUMN protocol text;

-- Existing venues speak their own protocol; only the futures one splits.
UPDATE venues SET protocol = venue_id WHERE protocol IS NULL;
ALTER TABLE venues ALTER COLUMN protocol SET NOT NULL;

-- accounts.venue_id is a plain foreign key (NO ACTION), so the venue
-- cannot be renamed out from under its accounts: add the correctly-named
-- row, re-point the children, drop the old one.
INSERT INTO venues (venue_id, kind, asset_classes, protocol, notes)
SELECT 'amp', kind, asset_classes, 'rithmic',
       'AMP Futures (FCM) — reached over the Rithmic R|Protocol gateway. '
       'Chosen over IB for futures: no per-trade currency conversion fee.'
  FROM venues WHERE venue_id = 'rithmic'
ON CONFLICT (venue_id) DO NOTHING;

UPDATE accounts SET venue_id = 'amp' WHERE venue_id = 'rithmic';
DELETE FROM venues WHERE venue_id = 'rithmic';
