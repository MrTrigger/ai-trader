-- Register the Stockholm equities bot as its own fleet member. The frontend
-- derives navigation tabs from this registry, so identity belongs here rather
-- than in a hard-coded React list.
--
-- Deliberately disabled and without a launch command: this migration makes
-- the work visible, but cannot start an unfinished runtime or place an order.

UPDATE venues
   SET asset_classes = array_append(asset_classes, 'equities')
 WHERE venue_id = 'ib'
   AND NOT ('equities' = ANY(asset_classes));

INSERT INTO bots (
    bot_id,
    display_name,
    cadence,
    asset_class,
    decision_core,
    enabled,
    state_dir,
    launch,
    notes
) VALUES (
    'stockholm-portfolio',
    'Stockholm long/short portfolio',
    'cron',
    'equities',
    'service/crates/stockholm-portfolio',
    false,
    'var/stockholm/state',
    NULL,
    'Nasdaq Stockholm Main Market Large/Mid/Small Cap only. Daily evaluation; the current research candidate uses a 20-session holding/rebalance cadence. Disabled until validation; no forced long/short neutrality.'
)
ON CONFLICT (bot_id) DO UPDATE SET
    display_name = EXCLUDED.display_name,
    cadence = EXCLUDED.cadence,
    asset_class = EXCLUDED.asset_class,
    decision_core = EXCLUDED.decision_core,
    state_dir = COALESCE(bots.state_dir, EXCLUDED.state_dir),
    notes = EXCLUDED.notes;
