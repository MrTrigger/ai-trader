# Lighter paper deployment

- [x] Confirm Lighter identity, trace venue/DB/UI/cycle paths and inspect cluster.
- [x] Add public Lighter markets, marks and depth adapter; reject real orders.
- [x] Wire venue-selected paper data/feed and additive DB/account/bot identity plus dashboard.
- [x] Isolate Lighter universe, state and configuration; retain shared historical bars read-only.
- [x] Test local market filtering, simulated orders/state restore and dashboard behavior.
- [x] Build and publish the exact reviewed source; update cluster image and schedule.
- [x] Verify running image, migration, feed freshness, successful Lighter paper cycle and persisted fills.

User explicitly authorizes exchange support, DB/backend/frontend changes and
cluster deployment in paper mode. Existing Hyperliquid and IB books remain
separate. No Lighter signing, authenticated accounts, transfers or real orders.

Venue filtering changes the research universe; the paper record is a new
forward experiment, not a claim that the earlier 1.54 Sharpe survives venue limits.
The existing PaperVenue uses configured slippage and does not model funding,
queue position or partial fills; document this when interpreting paper P&L.

Use .audit/lighter-paper.tsv for the decision/deployment trail. Split new adapter
and DB/UI work into isolated worktrees; root owns botfactory/feed anddeployment.
Existing venue interface is the integration boundary, so no competing architecture
is needed. Cross-model review precedes deployment; exact image and actual cycle
verification follow deployment.
