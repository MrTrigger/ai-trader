# Lighter paper deployment

`crypto-lighter` runs the frozen 24-hour crypto ranker with the
`risk_adjusted_score_selected` constructor. It screens the universe against
Lighter's native perpetual symbols before ranking. Its coverage differs from
the research portfolio, so this is a new forward experiment.

The adapter reads public markets, official mark/index prices and resting depth.
It has no account credentials, signer or order submission implementation. The
runner simulates fills with zero commission and 2.2 basis points of adverse
slippage. Funding, margin, queue position and partial fills are not simulated.
Those omissions limit how paper P&L can predict live results.

Migration 0012 creates venue `lighter`, account `lighter-sim` and the disabled
bot with its paper trade binding. Deployment enables it once. Applying migrations
again preserves the operator's enabled/disabled setting. Runtime execution and
book reads check the configured venue and mode against the trade binding.

The image contains `/app/lighter-bot.json` and `/app/lighter-config.toml`.
The bot owns `/data/lighter/state`, its universe snapshots and liquidity captures.
Only historical bars and funding link to the shared `/data` directories. Both
venue cycles lock shared data refreshes. Simulated fills and balances in PostgreSQL
are keyed by bot ID.

The cluster runs a public price poller every 60 seconds and the daily
`aitrader-lighter-cycle` CronJob at 00:10 UTC. Each poll writes `feed.json` beside
`marks.json`; the dashboard evaluates its last successful update on every request.
Prices older than 180 seconds or a failed poll show unhealthy. Missing metadata
remains unknown. `/api/bots/crypto-lighter/book` reads this bot's book.

Run `bin/test-paper-cycle.sh` to check state isolation and failure handling.
Run `cargo test --manifest-path service/Cargo.toml -p lighter -p bot -p records -p api`
for adapter, execution routing and API checks. The ignored bot test
`lighter_paper_fill_survives_restore` reads public prices and simulates one fill
in memory; it submits no real order.

Deploy the committed image with `bin/deploy.sh`. On the first installation, run
`bot --config /app/lighter-bot.json identity migrate` with the new image before
rolling out the API, then `identity enable` and `resume --reason ... --by ...`.
Start one Job from the Lighter CronJob with `BOOTSTRAP=1` to initialize its flat
paper book. Recurring jobs keep the normal decision-lag gate.
