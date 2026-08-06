//! Interactive Brokers venue adapter over `rust-ibapi` (the `ibapi` crate,
//! v3: protobuf wire, needs a current IB Gateway).
//!
//! Port of the defensive posture proven in the Python ib_async executor:
//!
//! * **Two-flag arming.** Orders are permitted only when `allow_orders` is
//!   set AND the account is paper (IB paper accounts start with 'D') or
//!   `allow_live` is additionally set. An unarmed adapter still answers
//!   every read — you can watch, reconcile and probe without being able to
//!   place anything.
//! * **Flatten is always permitted.** [`IbVenue::flatten_all`] closes every
//!   position at market even when unarmed: the emergency exit must never
//!   be behind the same gate as the thing that caused the emergency.
//! * **Front month resolved, never assumed.** The concrete contract comes
//!   from `contract_details` — nearest expiry at or after today — and the
//!   chosen expiry is visible in [`IbVenue::describe`].
//!
//! Everything here is offline-compiled and gated on `ib-check` against the
//! operator's Gateway before any order path is trusted.

use async_trait::async_trait;
use ibapi::contracts::Contract;
use ibapi::orders::{Action, ExecutionFilter, Executions, Order};
use ibapi::prelude::*;
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use time::OffsetDateTime;
use venue::{
    AssetId, Balance, Capabilities, Fill, Market, OpenOrder, OrderAck, OrderRequest, OrderState,
    OrderType, Position, Side, VenueAdapter, VenueError,
};

/// How long a bounded read (positions, balances, fills) may take before we
/// call the venue unreachable. Generous: the Gateway answers in ms.
const READ_TIMEOUT_S: u64 = 15;

#[derive(Debug, Clone)]
pub struct IbConfig {
    pub host: String,
    pub port: u16,
    pub client_id: i32,
    /// Required: the account this bot is bound to. Never inferred — trading
    /// "whatever account the Gateway is logged into" is how a paper bot
    /// meets a live account.
    pub account: String,
    pub symbol: String,
    pub allow_orders: bool,
    pub allow_live: bool,
}

impl IbConfig {
    /// From the environment, per .env section 7. `live` selects the
    /// IB_LIVE_* block; default is the paper block.
    pub fn from_env(live: bool) -> Result<Self, String> {
        let pick = |paper: &str, live_k: &str| {
            std::env::var(if live { live_k } else { paper }).unwrap_or_default()
        };
        let host = {
            let h = pick("IB_PAPER_HOST", "IB_LIVE_HOST");
            if h.is_empty() {
                "127.0.0.1".into()
            } else {
                h
            }
        };
        let port: u16 = {
            let p = pick("IB_PAPER_PORT", "IB_LIVE_PORT");
            if p.is_empty() {
                if live {
                    4001
                } else {
                    4002
                }
            } else {
                p.parse().map_err(|e| format!("bad IB port {p:?}: {e}"))?
            }
        };
        let account = pick("IB_PAPER_ACCOUNT", "IB_LIVE_ACCOUNT");
        if account.is_empty() {
            return Err(
                "IB account not configured (IB_PAPER_ACCOUNT / IB_LIVE_ACCOUNT in .env) — \
                 refusing to trade whichever account the Gateway happens to hold"
                    .into(),
            );
        }
        let yes = |k: &str| {
            matches!(
                std::env::var(k).unwrap_or_default().to_lowercase().as_str(),
                "yes" | "true" | "1"
            )
        };
        Ok(Self {
            host,
            port,
            client_id: 9,
            account,
            symbol: std::env::var("IB_SYMBOL").unwrap_or_else(|_| "MNQ".into()),
            allow_orders: yes("IB_ALLOW_ORDERS"),
            allow_live: yes("IB_ALLOW_LIVE"),
        })
    }
}

pub struct IbVenue {
    client: Client,
    cfg: IbConfig,
    /// The resolved front-month contract.
    contract: Contract,
    expiry: Option<time::Date>,
    multiplier: Decimal,
    tick: Decimal,
    paper: bool,
    armed: bool,
}

fn unreachable_err<E: std::fmt::Display>(e: E) -> VenueError {
    VenueError::Unreachable(e.to_string())
}

fn dec(x: f64) -> Decimal {
    Decimal::from_f64(x).unwrap_or_default()
}

/// Parse IB's `YYYYMMDD` (or `YYYYMM`) expiry strings.
fn parse_expiry(s: &str) -> Option<time::Date> {
    let s = s.trim();
    let (y, m, d) = if s.len() >= 8 {
        (s.get(0..4)?, s.get(4..6)?, s.get(6..8)?)
    } else if s.len() >= 6 {
        (s.get(0..4)?, s.get(4..6)?, "28")
    } else {
        return None;
    };
    time::Date::from_calendar_date(
        y.parse().ok()?,
        time::Month::try_from(m.parse::<u8>().ok()?).ok()?,
        d.parse().ok()?,
    )
    .ok()
}

impl IbVenue {
    /// Connect, verify the configured account, resolve the front month, and
    /// decide the arming state. Read paths work regardless of arming.
    pub async fn connect(cfg: IbConfig) -> Result<Self, VenueError> {
        let addr = format!("{}:{}", cfg.host, cfg.port);
        let client = Client::connect(&addr, cfg.client_id).await.map_err(|e| {
            VenueError::Unreachable(format!(
                "cannot connect to IB Gateway at {addr}: {e}. Checklist: Gateway \
                 running? API enabled? correct port (4001 live / 4002 paper)?"
            ))
        })?;

        let accounts = client.managed_accounts().await.map_err(unreachable_err)?;
        if !accounts.iter().any(|a| a == &cfg.account) {
            return Err(VenueError::Unreachable(format!(
                "the Gateway does not hold account {:?} (it holds {:?}) — wrong Gateway \
                 or wrong .env",
                cfg.account, accounts
            )));
        }
        let paper = cfg.account.starts_with('D');
        let armed = cfg.allow_orders && (paper || cfg.allow_live);

        // Resolve the front month: every listed expiry, keep the nearest one
        // at or after today.
        let probe = ibapi::contracts::ContractBuilder::futures(
            cfg.symbol.as_str(),
            "CME",
            "USD",
        )
        .build()
        .map_err(|e| VenueError::Unreachable(format!("contract probe: {e}")))?;
        let details = client
            .contract_details(&probe)
            .await
            .map_err(unreachable_err)?;
        let today = OffsetDateTime::now_utc().date();
        let mut best: Option<(time::Date, ibapi::contracts::ContractDetails)> = None;
        for d in details {
            let Some(exp) = parse_expiry(&d.contract.last_trade_date_or_contract_month) else {
                continue;
            };
            if exp < today {
                continue;
            }
            if best.as_ref().map(|(b, _)| exp < *b).unwrap_or(true) {
                best = Some((exp, d));
            }
        }
        let (expiry, front) = best.ok_or_else(|| {
            VenueError::Unreachable(format!(
                "no unexpired {} contract on CME — data permissions or symbol wrong",
                cfg.symbol
            ))
        })?;
        let multiplier: Decimal = front
            .contract
            .multiplier
            .parse()
            .unwrap_or_else(|_| Decimal::from(2));
        let tick = dec(front.min_tick);

        Ok(Self {
            client,
            contract: front.contract.clone(),
            expiry: Some(expiry),
            multiplier,
            tick,
            paper,
            armed,
            cfg,
        })
    }

    pub fn describe(&self) -> String {
        format!(
            "ib {} {} ({}, {} account {}, {})",
            self.cfg.symbol,
            self.contract.local_symbol,
            self.expiry
                .map(|e| e.to_string())
                .unwrap_or_else(|| "?".into()),
            if self.paper { "PAPER" } else { "LIVE" },
            self.cfg.account,
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

    pub fn server_version(&self) -> i32 {
        self.client.server_version()
    }

    /// The connected client and resolved contract, for callers that need
    /// venue facilities beyond the trait (the live bar feed).
    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn contract(&self) -> &Contract {
        &self.contract
    }

    pub fn contract_description(&self) -> String {
        format!(
            "{} {}",
            self.contract.local_symbol, self.contract.last_trade_date_or_contract_month
        )
    }

    fn asset(&self) -> AssetId {
        AssetId::from(self.cfg.symbol.as_str())
    }

    async fn submit_market(
        &self,
        side: Side,
        qty: Decimal,
        client_order_id: &str,
    ) -> Result<OrderAck, VenueError> {
        let order_id = self.client.next_order_id();
        let order = Order {
            order_id,
            action: match side {
                Side::Buy => Action::Buy,
                Side::Sell => Action::Sell,
            },
            total_quantity: qty
                .try_into()
                .map_err(|_| VenueError::NonPositiveQty(self.asset()))?,
            order_type: "MKT".into(),
            order_ref: client_order_id.to_string(),
            ..Default::default()
        };
        self.client
            .submit_order(order_id, &self.contract, &order)
            .await
            .map_err(unreachable_err)?;
        Ok(OrderAck {
            client_order_id: client_order_id.to_string(),
            venue_order_id: order_id.to_string(),
            state: OrderState::Open,
            accepted_at: OffsetDateTime::now_utc(),
        })
    }

    /// Close every position on this contract's account at market. Always
    /// permitted, armed or not — the emergency exit never sits behind the
    /// gate that caused the emergency. Returns the acks it submitted.
    pub async fn flatten_all(
        &self,
        client_order_prefix: &str,
    ) -> Result<Vec<OrderAck>, VenueError> {
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
}

#[async_trait]
impl VenueAdapter for IbVenue {
    async fn get_markets(&self) -> Result<Vec<Market>, VenueError> {
        Ok(vec![Market {
            asset: self.asset(),
            venue_symbol: self.contract.local_symbol.clone(),
            quote_currency: "USD".into(),
            tick: self.tick,
            lot: Decimal::ONE,
            min_notional: Decimal::ZERO,
            multiplier: self.multiplier,
            expiry: self.expiry,
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
        let group = ibapi::accounts::types::AccountGroup::from("All");
        let sub = self
            .client
            .account_summary(&group, &["NetLiquidation", "TotalCashValue"])
            .await
            .map_err(unreachable_err)?;
        let mut stream = sub.filter_data();
        let mut net_liq: Option<Decimal> = None;
        let mut cash: Option<Decimal> = None;
        let read = async {
            while let Some(item) = stream.next().await {
                match item {
                    Ok(AccountSummaryResult::Summary(s)) => {
                        if s.account == self.cfg.account {
                            let v: f64 = s.value.parse().unwrap_or(0.0);
                            match s.tag.as_str() {
                                "NetLiquidation" => net_liq = Some(dec(v)),
                                "TotalCashValue" => cash = Some(dec(v)),
                                _ => {}
                            }
                        }
                    }
                    Ok(AccountSummaryResult::End) => break,
                    Err(e) => return Err(unreachable_err(e)),
                }
            }
            Ok(())
        };
        tokio::time::timeout(std::time::Duration::from_secs(READ_TIMEOUT_S), read)
            .await
            .map_err(|_| VenueError::Unreachable("account summary timed out".into()))??;
        let total = net_liq.or(cash).unwrap_or_default();
        Ok(vec![Balance {
            currency: "USD".into(),
            total,
            available: cash.unwrap_or(total),
        }])
    }

    async fn get_positions(&self) -> Result<Vec<Position>, VenueError> {
        let sub = self.client.positions().await.map_err(unreachable_err)?;
        let mut stream = sub.filter_data();
        let mut out = Vec::new();
        let read = async {
            while let Some(item) = stream.next().await {
                match item {
                    Ok(PositionUpdate::Position(p)) => {
                        if p.account == self.cfg.account
                            && p.contract.symbol == self.contract.symbol
                            && p.position != 0.0
                        {
                            // IB's average_cost for futures includes the
                            // multiplier; positions are priced in points.
                            let mult: f64 = self.multiplier.try_into().unwrap_or(1.0);
                            out.push(Position {
                                asset: AssetId::from(p.contract.symbol.to_string().as_str()),
                                qty: dec(p.position),
                                avg_price: dec(p.average_cost / mult.max(1.0)),
                            });
                        }
                    }
                    Ok(PositionUpdate::PositionEnd) => break,
                    Err(e) => return Err(unreachable_err(e)),
                }
            }
            Ok(())
        };
        tokio::time::timeout(std::time::Duration::from_secs(READ_TIMEOUT_S), read)
            .await
            .map_err(|_| VenueError::Unreachable("positions timed out".into()))??;
        Ok(out)
    }

    async fn place_order(&self, order: &OrderRequest) -> Result<OrderAck, VenueError> {
        if !self.armed {
            return Err(VenueError::Unreachable(format!(
                "IB adapter is NOT ARMED (allow_orders={}, paper={}, allow_live={}) — \
                 refusing {}. Reads and flatten_all remain available.",
                self.cfg.allow_orders, self.paper, self.cfg.allow_live, order.client_order_id
            )));
        }
        if order.qty <= Decimal::ZERO {
            return Err(VenueError::NonPositiveQty(order.asset.clone()));
        }
        match order.order_type {
            OrderType::Market => {}
            _ => {
                return Err(VenueError::Unreachable(
                    "the IB adapter submits market orders only (the book's validated \
                     execution model); extend deliberately when a strategy needs more"
                        .into(),
                ))
            }
        }
        self.submit_market(order.side, order.qty, &order.client_order_id)
            .await
    }

    async fn cancel_order(&self, venue_order_id: &str) -> Result<(), VenueError> {
        let id: i32 = venue_order_id
            .parse()
            .map_err(|_| VenueError::UnknownOrder(venue_order_id.to_string()))?;
        let sub = self
            .client
            .cancel_order(id, "")
            .await
            .map_err(unreachable_err)?;
        drop(sub);
        Ok(())
    }

    async fn get_open_orders(&self) -> Result<Vec<OpenOrder>, VenueError> {
        let sub = self.client.open_orders().await.map_err(unreachable_err)?;
        let mut stream = sub.filter_data();
        let mut out = Vec::new();
        let read = async {
            while let Some(item) = stream.next().await {
                match item {
                    Ok(Orders::OrderData(o)) => {
                        if o.contract.symbol == self.contract.symbol {
                            out.push(OpenOrder {
                                venue_order_id: o.order.order_id.to_string(),
                                client_order_id: o.order.order_ref.clone(),
                                asset: AssetId::from(o.contract.symbol.to_string().as_str()),
                                side: match o.order.action {
                                    Action::Buy => Side::Buy,
                                    _ => Side::Sell,
                                },
                                qty: dec(o.order.total_quantity),
                                limit_price: dec(o.order.limit_price.unwrap_or(0.0)),
                                reserved: Decimal::ZERO,
                                reserved_currency: "USD".into(),
                            });
                        }
                    }
                    Ok(Orders::OrderStatus(_)) => {}
                    Err(e) => return Err(unreachable_err(e)),
                }
            }
            Ok(())
        };
        // The stream ends after the venue's end marker; the timeout is the
        // backstop for a Gateway that never sends one.
        tokio::time::timeout(std::time::Duration::from_secs(READ_TIMEOUT_S), read)
            .await
            .map_err(|_| VenueError::Unreachable("open orders timed out".into()))??;
        Ok(out)
    }

    async fn get_fills(&self, since: Option<OffsetDateTime>) -> Result<Vec<Fill>, VenueError> {
        let filter = ExecutionFilter {
            account_code: self.cfg.account.clone(),
            ..Default::default()
        };
        let sub = self
            .client
            .executions(filter)
            .await
            .map_err(unreachable_err)?;
        let mut stream = sub.filter_data();
        let mut out = Vec::new();
        let read = async {
            while let Some(item) = stream.next().await {
                match item {
                    Ok(Executions::ExecutionData(e)) => {
                        let x = &e.execution;
                        let ts = parse_ib_time(&x.time).unwrap_or(OffsetDateTime::UNIX_EPOCH);
                        if let Some(s) = since {
                            if ts < s {
                                continue;
                            }
                        }
                        let side_txt = format!("{:?}", x.side).to_uppercase();
                        out.push(Fill {
                            venue_fill_id: x.execution_id.clone(),
                            venue_order_id: x.order_id.to_string(),
                            client_order_id: x.order_reference.clone(),
                            asset: AssetId::from(e.contract.symbol.to_string().as_str()),
                            side: if side_txt.contains("BUY") || side_txt.contains("BOT") {
                                Side::Buy
                            } else {
                                Side::Sell
                            },
                            qty: dec(x.shares),
                            price: dec(x.price),
                            fee: Decimal::ZERO, // commission arrives separately
                            fee_currency: "USD".into(),
                            ts,
                        });
                    }
                    Ok(Executions::CommissionReport(_)) => {}
                    Err(e) => return Err(unreachable_err(e)),
                }
            }
            Ok(())
        };
        tokio::time::timeout(std::time::Duration::from_secs(READ_TIMEOUT_S), read)
            .await
            .map_err(|_| VenueError::Unreachable("executions timed out".into()))??;
        out.sort_by_key(|f| f.ts);
        Ok(out)
    }
}

/// A lazily-connecting wrapper so a synchronous venue registry can hand
/// out an IB adapter: the async connection happens on first use, inside
/// whatever runtime is actually driving the caller. All arming decisions
/// are still made at connect time, from the same config.
pub struct IbLazy {
    cfg: IbConfig,
    inner: tokio::sync::OnceCell<IbVenue>,
}

impl IbLazy {
    pub fn new(cfg: IbConfig) -> Self {
        Self {
            cfg,
            inner: tokio::sync::OnceCell::new(),
        }
    }

    pub fn describe(&self) -> String {
        format!(
            "ib {} via {}:{} account {} (connects on first use)",
            self.cfg.symbol, self.cfg.host, self.cfg.port, self.cfg.account
        )
    }

    async fn venue(&self) -> Result<&IbVenue, VenueError> {
        self.inner
            .get_or_try_init(|| IbVenue::connect(self.cfg.clone()))
            .await
    }
}

#[async_trait]
impl VenueAdapter for IbLazy {
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
    async fn get_fills(&self, since: Option<OffsetDateTime>) -> Result<Vec<Fill>, VenueError> {
        self.venue().await?.get_fills(since).await
    }
}

/// IB execution times: "YYYYMMDD  HH:MM:SS" in the operator's TZ, or
/// "YYYYMMDD-HH:MM:SS" with zone suffixes in newer builds. Best-effort UTC.
fn parse_ib_time(s: &str) -> Option<OffsetDateTime> {
    let cleaned: String = s.chars().filter(|c| c.is_ascii_digit()).take(14).collect();
    if cleaned.len() < 14 {
        return None;
    }
    let fmt = time::macros::format_description!("[year][month][day][hour][minute][second]");
    time::PrimitiveDateTime::parse(&cleaned, &fmt)
        .ok()
        .map(|d| d.assume_utc())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_parsing() {
        assert_eq!(
            parse_expiry("20260918"),
            time::Date::from_calendar_date(2026, time::Month::September, 18).ok()
        );
        assert!(parse_expiry("202609").is_some());
        assert!(parse_expiry("junk").is_none());
    }

    #[test]
    fn ib_time_parsing() {
        assert!(parse_ib_time("20260806  15:30:00").is_some());
        assert!(parse_ib_time("20260806-15:30:00 US/Eastern").is_some());
        assert!(parse_ib_time("nonsense").is_none());
    }

    #[test]
    fn arming_requires_the_right_flags() {
        // The rule itself, stated as data: (allow_orders, paper, allow_live) -> armed.
        for (orders, paper, live, armed) in [
            (false, true, false, false),
            (true, true, false, true),
            (true, false, false, false),
            (true, false, true, true),
            (false, false, true, false),
        ] {
            assert_eq!(orders && (paper || live), armed);
        }
    }
}
