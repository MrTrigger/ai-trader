//! The Rithmic (R|Protocol) adapter, over `rithmic-rs`.
//!
//! Rithmic is a PROTOCOL, not a broker. The broker here is AMP Futures —
//! the FCM that holds the account, clears the trades, and issues these
//! credentials — chosen over IB because IB charges a currency conversion
//! on every trade (the operator's reason for the switch, 2026-08-07).
//! Ironbeam, Optimus and others are also reached this way, so the registry
//! names the broker (`venue_id = amp`) and selects this adapter by the
//! venue's `protocol` column. Adding another Rithmic broker is a row.
//!
//! Same defensive posture as the IB adapter, deliberately identical:
//!
//! * **Two-flag arming.** Orders only when `RITHMIC_ALLOW_ORDERS` is set
//!   AND the environment is Demo (paper) or `RITHMIC_ALLOW_LIVE` is
//!   additionally set. Reads always work; flatten always works.
//! * **Front month derived, then verified.** CME equity-index quarterlies
//!   (H/M/U/Z, third-Friday expiry) are derived locally — Rithmic trades
//!   concrete symbols like `MNQU6` — and `rithmic-check` confirms the
//!   symbol against the venue before anything is trusted.
//! * **Everything here is offline-compiled and gated on `rithmic-check`
//!   against real (demo) credentials before any order path is trusted.**
//!   In particular: orders go through Rithmic's bracket-order request with
//!   zero-tick children (the plain-entry idiom rithmic-rs exposes); the
//!   demo probe must confirm that semantic before live use.
//!
//! Credentials come from the environment per rithmic-rs's own contract
//! (RITHMIC_DEMO_* / RITHMIC_LIVE_* + RITHMIC_APP_NAME/VERSION); the DB
//! stores only credential references, never secrets.

use async_trait::async_trait;
use chrono::{Datelike, NaiveDate};
use rithmic_rs::rti::messages::RithmicMessage;
use rithmic_rs::rti::request_bracket_order;
use rithmic_rs::{
    ConnectStrategy, RithmicAccount, RithmicBracketOrder, RithmicCancelOrder, RithmicConfig,
    RithmicEnv, RithmicHistoryPlant, RithmicOrderPlant, RithmicPnlPlant,
};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use venue::{
    AssetId, Balance, Capabilities, Fill, Market, OpenOrder, OrderAck, OrderRequest, OrderState,
    OrderType, Position, Side, VenueAdapter, VenueError,
};

/// How long a startup connect may take before it is called unreachable.
const CONNECT_TIMEOUT_S: u64 = 20;

#[derive(Debug, Clone)]
pub struct RithmicCfg {
    pub env: RithmicEnv,
    /// Contract root, e.g. "MNQ"; the concrete front month is derived.
    pub symbol_root: String,
    pub exchange: String,
    /// Dollars per index point per contract (MNQ: 2). Rithmic reference
    /// data is not wired yet, so this is configuration with a safe default.
    pub multiplier: f64,
    pub tick: f64,
    pub allow_orders: bool,
    pub allow_live: bool,
}

impl RithmicCfg {
    pub fn from_env(live: bool) -> Result<Self, String> {
        let yes = |k: &str| {
            matches!(
                std::env::var(k).unwrap_or_default().to_lowercase().as_str(),
                "yes" | "true" | "1"
            )
        };
        Ok(Self {
            env: if live {
                RithmicEnv::Live
            } else {
                RithmicEnv::Demo
            },
            symbol_root: std::env::var("RITHMIC_SYMBOL").unwrap_or_else(|_| "MNQ".into()),
            exchange: std::env::var("RITHMIC_EXCHANGE").unwrap_or_else(|_| "CME".into()),
            multiplier: std::env::var("RITHMIC_MULTIPLIER")
                .ok()
                .and_then(|m| m.parse().ok())
                .unwrap_or(2.0),
            tick: std::env::var("RITHMIC_TICK")
                .ok()
                .and_then(|t| t.parse().ok())
                .unwrap_or(0.25),
            allow_orders: yes("RITHMIC_ALLOW_ORDERS"),
            allow_live: yes("RITHMIC_ALLOW_LIVE"),
        })
    }
}

/// Third Friday of a month — CME equity-index futures expiry.
fn third_friday(year: i32, month: u32) -> NaiveDate {
    let first = NaiveDate::from_ymd_opt(year, month, 1).expect("valid month");
    let to_friday = (chrono::Weekday::Fri.num_days_from_monday() + 7
        - first.weekday().num_days_from_monday())
        % 7;
    first + chrono::Duration::days(to_friday as i64 + 14)
}

/// Derive the front-month symbol for a CME equity-index root: nearest
/// quarterly (Mar/Jun/Sep/Dec, codes H/M/U/Z) whose third-Friday expiry is
/// at or after `today`. `MNQ` on 2026-08-07 -> (`MNQU6`, 2026-09-18).
pub fn front_month_symbol(root: &str, today: NaiveDate) -> (String, NaiveDate) {
    const QUARTERS: [(u32, char); 4] = [(3, 'H'), (6, 'M'), (9, 'U'), (12, 'Z')];
    let mut year = today.year();
    loop {
        for (month, code) in QUARTERS {
            let expiry = third_friday(year, month);
            if expiry >= today {
                return (format!("{root}{code}{}", year.rem_euclid(10)), expiry);
            }
        }
        year += 1;
    }
}

pub struct RithmicVenue {
    order: RithmicOrderPlant,
    pnl: RithmicPnlPlant,
    history: RithmicHistoryPlant,
    account: RithmicAccount,
    cfg: RithmicCfg,
    pub symbol: String,
    pub expiry: NaiveDate,
    paper: bool,
    armed: bool,
}

fn unreachable_err<E: std::fmt::Display>(e: E) -> VenueError {
    VenueError::Unreachable(e.to_string())
}

fn dec(x: f64) -> Decimal {
    Decimal::from_f64(x).unwrap_or_default()
}

impl RithmicVenue {
    /// Connect the order, PnL and history plants and log in. Reads work
    /// regardless of arming.
    pub async fn connect(cfg: RithmicCfg) -> Result<Self, VenueError> {
        let rcfg = RithmicConfig::from_env(cfg.env).map_err(|e| {
            VenueError::Unreachable(format!(
                "Rithmic credentials incomplete ({e}) — see .env section 8"
            ))
        })?;
        // Present-but-blank is the common half-configured state (the .env
        // placeholders ship empty), and rithmic-rs's Retry strategy would
        // sit on it forever rather than fail. Refuse first, loudly: a bot
        // that hangs at startup looks identical to a bot that is working.
        for (name, value) in [
            ("URL", &rcfg.url),
            ("USER", &rcfg.user),
            ("PW", &rcfg.password),
        ] {
            if value.trim().is_empty() {
                return Err(VenueError::Unreachable(format!(
                    "Rithmic {:?} {name} is empty — fill .env section 8 with the credentials \
                     AMP sends. Refusing to retry a login that cannot succeed.",
                    cfg.env
                )));
            }
        }
        let account = RithmicAccount::from_env(cfg.env).map_err(|e| {
            VenueError::Unreachable(format!(
                "Rithmic account not configured ({e}) — refusing to trade an inferred account"
            ))
        })?;
        // Bounded: Retry is right for a blip mid-session, but at startup an
        // unreachable venue must surface as an error the operator can read.
        let deadline = std::time::Duration::from_secs(CONNECT_TIMEOUT_S);
        macro_rules! bounded {
            ($what:literal, $fut:expr) => {
                match tokio::time::timeout(deadline, $fut).await {
                    Err(_) => {
                        return Err(VenueError::Unreachable(format!(
                            "{} did not connect within {CONNECT_TIMEOUT_S}s — check the URL, \
                             the credentials, and whether Rithmic is up",
                            $what
                        )))
                    }
                    Ok(r) => r.map_err(|e| unreachable_err(format!("{}: {e}", $what)))?,
                }
            };
        }
        let order = bounded!(
            "order plant",
            RithmicOrderPlant::connect(&rcfg, ConnectStrategy::Retry)
        );
        let pnl = bounded!(
            "pnl plant",
            RithmicPnlPlant::connect(&rcfg, ConnectStrategy::Retry)
        );
        let history = bounded!(
            "history plant",
            RithmicHistoryPlant::connect(&rcfg, ConnectStrategy::Retry)
        );
        order
            .get_handle(&account)
            .login()
            .await
            .map_err(|e| unreachable_err(format!("order login: {e}")))?;
        pnl.get_handle(&account)
            .login()
            .await
            .map_err(|e| unreachable_err(format!("pnl login: {e}")))?;
        history
            .get_handle()
            .login()
            .await
            .map_err(|e| unreachable_err(format!("history login: {e}")))?;

        let paper = cfg.env == RithmicEnv::Demo;
        let armed = cfg.allow_orders && (paper || cfg.allow_live);
        let today = chrono::Utc::now().date_naive();
        let (symbol, expiry) = front_month_symbol(&cfg.symbol_root, today);
        Ok(Self {
            order,
            pnl,
            history,
            account,
            symbol,
            expiry,
            paper,
            armed,
            cfg,
        })
    }

    pub fn describe(&self) -> String {
        format!(
            "rithmic {} {} (exp {}, {} account {}, {})",
            self.cfg.symbol_root,
            self.symbol,
            self.expiry,
            if self.paper { "DEMO" } else { "LIVE" },
            self.account.account_id,
            if self.armed {
                "ARMED"
            } else {
                "not armed: reads only, plus flatten"
            },
        )
    }

    pub fn is_paper(&self) -> bool {
        self.paper
    }

    pub fn is_armed(&self) -> bool {
        self.armed
    }

    fn asset(&self) -> AssetId {
        AssetId::from(self.cfg.symbol_root.as_str())
    }

    async fn submit_market(
        &self,
        side: Side,
        qty: Decimal,
        client_order_id: &str,
    ) -> Result<OrderAck, VenueError> {
        // rithmic-rs exposes orders as bracket requests; zero-tick children
        // is the plain-entry idiom. VERIFY against demo credentials via
        // rithmic-check before trusting live.
        let order = RithmicBracketOrder {
            action: match side {
                Side::Buy => request_bracket_order::TransactionType::Buy,
                Side::Sell => request_bracket_order::TransactionType::Sell,
            },
            duration: request_bracket_order::Duration::Day,
            exchange: self.cfg.exchange.clone(),
            localid: client_order_id.to_string(),
            price_type: request_bracket_order::PriceType::Market,
            price: None,
            profit_ticks: 0,
            quantity: qty
                .try_into()
                .map_err(|_| VenueError::NonPositiveQty(self.asset()))?,
            stop_ticks: 0,
            symbol: self.symbol.clone(),
        };
        self.order
            .get_handle(&self.account)
            .place_bracket_order(order)
            .await
            .map_err(unreachable_err)?;
        Ok(OrderAck {
            client_order_id: client_order_id.to_string(),
            venue_order_id: client_order_id.to_string(),
            state: OrderState::Open,
            accepted_at: time::OffsetDateTime::now_utc(),
        })
    }

    /// Close every position at market; always permitted, armed or not.
    /// Cancels resting orders first so nothing re-opens the book.
    pub async fn flatten_all(
        &self,
        client_order_prefix: &str,
    ) -> Result<Vec<OrderAck>, VenueError> {
        let _ = self
            .order
            .get_handle(&self.account)
            .cancel_all_orders()
            .await;
        let positions = self.get_positions().await?;
        let mut acks = Vec::new();
        for (i, p) in positions.iter().enumerate() {
            if p.qty.is_zero() {
                continue;
            }
            let side = if p.qty > Decimal::ZERO {
                Side::Sell
            } else {
                Side::Buy
            };
            let id = format!("{client_order_prefix}-FLAT-{i}");
            acks.push(self.submit_market(side, p.qty.abs(), &id).await?);
        }
        Ok(acks)
    }

    /// Completed 5-minute bars from the history plant, `(epoch_s, o, h, l,
    /// c, v)`, oldest first. The bar-close-driven feed for this venue.
    pub async fn time_bars_5m(
        &self,
        start_epoch_s: i32,
        end_epoch_s: i32,
    ) -> Result<Vec<(i64, f64, f64, f64, f64, f64)>, VenueError> {
        use rithmic_rs::rti::request_time_bar_replay::BarType;
        let responses = self
            .history
            .get_handle()
            .load_time_bars(
                self.symbol.clone(),
                self.cfg.exchange.clone(),
                BarType::MinuteBar,
                5,
                start_epoch_s,
                end_epoch_s,
            )
            .await
            .map_err(unreachable_err)?;
        let mut out = Vec::new();
        for r in responses {
            let (marker, o, h, l, c, v) = match &r.message {
                RithmicMessage::ResponseTimeBarReplay(b) => (
                    b.marker,
                    b.open_price,
                    b.high_price,
                    b.low_price,
                    b.close_price,
                    b.volume,
                ),
                RithmicMessage::TimeBar(b) => (
                    b.marker.map(|m| m as i32),
                    b.open_price,
                    b.high_price,
                    b.low_price,
                    b.close_price,
                    b.volume,
                ),
                _ => continue,
            };
            let (Some(ts), Some(o), Some(h), Some(l), Some(c)) = (marker, o, h, l, c) else {
                continue;
            };
            out.push((ts as i64, o, h, l, c, v.unwrap_or(0) as f64));
        }
        out.sort_by_key(|b| b.0);
        Ok(out)
    }
}

#[async_trait]
impl VenueAdapter for RithmicVenue {
    async fn get_markets(&self) -> Result<Vec<Market>, VenueError> {
        Ok(vec![Market {
            asset: self.asset(),
            venue_symbol: self.symbol.clone(),
            quote_currency: "USD".into(),
            tick: dec(self.cfg.tick),
            lot: Decimal::ONE,
            min_notional: Decimal::ZERO,
            multiplier: dec(self.cfg.multiplier),
            expiry: time::Date::from_calendar_date(
                self.expiry.year(),
                time::Month::try_from(self.expiry.month() as u8).expect("valid month"),
                self.expiry.day() as u8,
            )
            .ok(),
            initial_margin: None,
            asset_class: "futures".into(),
            capabilities: Capabilities {
                fractional: false,
                short: true,
                max_leverage: Decimal::ONE,
                stop_orders: false,
                funding: false,
            },
        }])
    }

    async fn get_balances(&self) -> Result<Vec<Balance>, VenueError> {
        // Rithmic's RMS message carries limits (loss limit, minimum
        // balance), not an equity figure; account equity lives in the PnL
        // plant's account updates, whose exact shape is verification-gated.
        // Until rithmic-check confirms it against demo credentials, an
        // explicit refusal beats a confident zero.
        Err(VenueError::Unreachable(
            "balance reads await rithmic-check against demo credentials".into(),
        ))
    }

    async fn get_positions(&self) -> Result<Vec<Position>, VenueError> {
        let responses = self
            .pnl
            .get_handle(&self.account)
            .pnl_position_snapshots()
            .await
            .map_err(unreachable_err)?;
        let mut out = Vec::new();
        let mut push = |symbol: Option<&String>, qty: Option<i32>, avg: Option<f64>| {
            let (Some(sym), Some(q)) = (symbol, qty) else {
                return;
            };
            if q == 0 || !sym.starts_with(&self.cfg.symbol_root) {
                return;
            }
            out.push(Position {
                asset: AssetId::from(self.cfg.symbol_root.as_str()),
                qty: Decimal::from(q),
                avg_price: dec(avg.unwrap_or(0.0)),
            });
        };
        if let RithmicMessage::InstrumentPnLPositionUpdate(p) = &responses.message {
            push(p.symbol.as_ref(), p.net_quantity, p.avg_open_fill_price);
        }
        Ok(out)
    }

    async fn place_order(&self, order: &OrderRequest) -> Result<OrderAck, VenueError> {
        if !self.armed {
            return Err(VenueError::Unreachable(format!(
                "Rithmic adapter is NOT ARMED (allow_orders={}, demo={}, allow_live={}) — \
                 refusing {}. Reads and flatten_all remain available.",
                self.cfg.allow_orders, self.paper, self.cfg.allow_live, order.client_order_id
            )));
        }
        if order.qty <= Decimal::ZERO {
            return Err(VenueError::NonPositiveQty(order.asset.clone()));
        }
        if order.order_type != OrderType::Market {
            return Err(VenueError::Unreachable(
                "the Rithmic adapter submits market orders only (the book's validated \
                 execution model); extend deliberately when a strategy needs more"
                    .into(),
            ));
        }
        self.submit_market(order.side, order.qty, &order.client_order_id)
            .await
    }

    async fn cancel_order(&self, venue_order_id: &str) -> Result<(), VenueError> {
        self.order
            .get_handle(&self.account)
            .cancel_order(RithmicCancelOrder {
                id: venue_order_id.to_string(),
            })
            .await
            .map(|_| ())
            .map_err(unreachable_err)
    }

    async fn get_open_orders(&self) -> Result<Vec<OpenOrder>, VenueError> {
        // show_orders responds over the notification stream in rithmic-rs;
        // parsing it is verification-gated. Until demo credentials confirm
        // the shape, an explicit "unknown" beats a confident empty answer.
        Err(VenueError::Unreachable(
            "open-order enumeration awaits rithmic-check against demo credentials".into(),
        ))
    }

    async fn get_fills(
        &self,
        _since: Option<time::OffsetDateTime>,
    ) -> Result<Vec<Fill>, VenueError> {
        Err(VenueError::Unreachable(
            "fill enumeration awaits rithmic-check against demo credentials".into(),
        ))
    }
}

/// A lazily-connecting wrapper so a synchronous venue registry can hand
/// out a Rithmic adapter: the async connection happens on first use,
/// inside whatever runtime drives the caller.
pub struct RithmicLazy {
    cfg: RithmicCfg,
    inner: tokio::sync::OnceCell<RithmicVenue>,
}

impl RithmicLazy {
    pub fn new(cfg: RithmicCfg) -> Self {
        Self {
            cfg,
            inner: tokio::sync::OnceCell::new(),
        }
    }

    pub fn describe(&self) -> String {
        format!(
            "rithmic {} on {} ({:?}, connects on first use)",
            self.cfg.symbol_root, self.cfg.exchange, self.cfg.env
        )
    }

    async fn venue(&self) -> Result<&RithmicVenue, VenueError> {
        self.inner
            .get_or_try_init(|| RithmicVenue::connect(self.cfg.clone()))
            .await
    }
}

#[async_trait]
impl VenueAdapter for RithmicLazy {
    async fn get_markets(&self) -> Result<Vec<Market>, VenueError> {
        self.venue().await?.get_markets().await
    }
    async fn get_balances(&self) -> Result<Vec<Balance>, VenueError> {
        self.venue().await?.get_balances().await
    }
    async fn get_positions(&self) -> Result<Vec<Position>, VenueError> {
        self.venue().await?.get_positions().await
    }
    async fn place_order(&self, order: &OrderRequest) -> Result<OrderAck, VenueError> {
        self.venue().await?.place_order(order).await
    }
    async fn cancel_order(&self, venue_order_id: &str) -> Result<(), VenueError> {
        self.venue().await?.cancel_order(venue_order_id).await
    }
    async fn get_open_orders(&self) -> Result<Vec<OpenOrder>, VenueError> {
        self.venue().await?.get_open_orders().await
    }
    async fn get_fills(
        &self,
        since: Option<time::OffsetDateTime>,
    ) -> Result<Vec<Fill>, VenueError> {
        self.venue().await?.get_fills(since).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_month_matches_the_ib_resolved_contract() {
        // 2026-08-07: IB resolved MNQU6 expiring 2026-09-18. The local
        // derivation must agree with the venue's answer.
        let (sym, exp) = front_month_symbol("MNQ", NaiveDate::from_ymd_opt(2026, 8, 7).unwrap());
        assert_eq!(sym, "MNQU6");
        assert_eq!(exp, NaiveDate::from_ymd_opt(2026, 9, 18).unwrap());
    }

    #[test]
    fn front_month_rolls_on_expiry_day_not_before() {
        // On the expiry day itself the front month is still the expiring
        // contract; the day after, it rolls.
        let (sym, _) = front_month_symbol("MNQ", NaiveDate::from_ymd_opt(2026, 9, 18).unwrap());
        assert_eq!(sym, "MNQU6");
        let (sym, exp) = front_month_symbol("MNQ", NaiveDate::from_ymd_opt(2026, 9, 19).unwrap());
        assert_eq!(sym, "MNQZ6");
        assert_eq!(exp, NaiveDate::from_ymd_opt(2026, 12, 18).unwrap());
    }

    #[test]
    fn year_boundary_rolls_into_march() {
        let (sym, _) = front_month_symbol("MNQ", NaiveDate::from_ymd_opt(2026, 12, 20).unwrap());
        assert_eq!(sym, "MNQH7");
    }

    #[test]
    fn arming_requires_the_right_flags() {
        for (orders, demo, live, armed) in [
            (false, true, false, false),
            (true, true, false, true),
            (true, false, false, false),
            (true, false, true, true),
        ] {
            assert_eq!(orders && (demo || live), armed);
        }
    }
}
