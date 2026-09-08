-- A separate paper book keeps venue coverage and fills attributable.
ALTER TABLE bots ADD COLUMN executor_config text;
ALTER TABLE bots ADD COLUMN core_config text;
ALTER TABLE bots ADD COLUMN data_root text;
ALTER TABLE bots ADD COLUMN trade_protocol text;
ALTER TABLE bots ADD COLUMN paper_only boolean NOT NULL DEFAULT false;

UPDATE bots SET trade_protocol = 'hyperliquid' WHERE bot_id = 'crypto-portfolio';

INSERT INTO venues (venue_id, kind, asset_classes, protocol, notes)
VALUES ('lighter', 'exchange', '{crypto}', 'lighter', 'Public market data; paper execution only.')
ON CONFLICT (venue_id) DO NOTHING;

INSERT INTO accounts (account_id, venue_id, kind, credential_ref, quote_currency, notes)
VALUES ('lighter-sim', 'lighter', 'paper', 'PUBLIC', 'USDC', 'Local simulation over public Lighter data; no trading credentials.')
ON CONFLICT (account_id) DO NOTHING;

INSERT INTO bots (bot_id, display_name, cadence, asset_class, decision_core,
                  enabled, state_dir, executor_config, core_config, data_root, trade_protocol, paper_only, notes)
VALUES ('crypto-lighter', 'Crypto portfolio · Lighter paper', 'cron', 'crypto',
        'service/crates/crypto-portfolio', false, '/data/lighter/state',
        '/app/lighter-bot.json', '/app/lighter-config.toml', '/data/lighter', 'lighter', true,
        'Separate Lighter paper validation. Deployment enables it separately; cron owns scheduling.')
ON CONFLICT (bot_id) DO NOTHING;

INSERT INTO account_bindings (bot_id, account_id, scope)
VALUES ('crypto-lighter', 'lighter-sim', 'trade')
ON CONFLICT (bot_id, account_id) DO NOTHING;

ALTER TABLE accounts ADD CONSTRAINT lighter_paper_only
    CHECK (venue_id <> 'lighter' OR kind = 'paper');
