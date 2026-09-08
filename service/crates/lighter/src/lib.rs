//! Public Lighter market data. All account and order methods are unsupported.
//!
//! Clones share caches and REST pacing. Marks expire after 10 seconds,
//! metadata after 60 seconds. Expired cache entries never mask refresh errors.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rust_decimal::Decimal;
use serde::Deserialize;
use time::OffsetDateTime;
use tokio::sync::Mutex;
use tokio::time::Instant;
use venue::{
    Balance, BookLevel, Capabilities, Fill, Market, OpenOrder, OrderAck, OrderBook, OrderRequest,
    Position, PriceSource, VenueAdapter, VenueError,
};

pub const MAINNET: &str = "https://mainnet.zklighter.elliot.ai";
const MARK_TTL: Duration = Duration::from_secs(10);
const MARKET_TTL: Duration = Duration::from_secs(60);
const REQUEST_SPACING: Duration = Duration::from_millis(1300);
const BOOK_LIMIT: usize = 100;

#[derive(Clone)]
pub struct Lighter {
    inner: Arc<Inner>,
}

struct Inner {
    base: reqwest::Url,
    http: reqwest::Client,
    next_request: Mutex<Instant>,
    snapshot: Mutex<Option<Snapshot>>,
}

struct Snapshot {
    fetched_at: Instant,
    markets: BTreeMap<String, MarketInfo>,
    marks: BTreeMap<String, Decimal>,
}

#[derive(Clone)]
struct MarketInfo {
    market: Market,
    id: u16,
}

#[derive(Deserialize)]
struct Details {
    code: u16,
    #[serde(default)]
    order_book_details: Vec<MarketDetail>,
}

#[derive(Deserialize)]
struct MarketDetail {
    symbol: String,
    market_id: u16,
    market_type: String,
    status: String,
    quote_asset_id: u16,
    supported_size_decimals: u32,
    supported_price_decimals: u32,
    min_base_amount: String,
    min_quote_amount: String,
    min_initial_margin_fraction: u32,
    multiplier: Option<String>,
    mark_price: Option<String>,
    index_price: Option<String>,
}

#[derive(Deserialize)]
struct Orders {
    code: u16,
    #[serde(default)]
    asks: Vec<RestingOrder>,
    #[serde(default)]
    bids: Vec<RestingOrder>,
}

#[derive(Deserialize)]
struct RestingOrder {
    price: String,
    remaining_base_amount: String,
}

fn invalid(message: impl Into<String>) -> VenueError {
    VenueError::Unreachable(format!("Lighter: {}", message.into()))
}

fn positive(text: &str, name: &str) -> Result<Decimal, VenueError> {
    let value = Decimal::from_str(text).map_err(|e| invalid(format!("invalid {name}: {e}")))?;
    if value <= Decimal::ZERO {
        return Err(invalid(format!("{name} must be positive")));
    }
    Ok(value)
}

fn snapshot(details: Details) -> Result<Snapshot, VenueError> {
    if details.code != 200 {
        return Err(invalid(format!("details API code {}", details.code)));
    }
    let mut markets = BTreeMap::new();
    let mut marks = BTreeMap::new();
    let mut ids = BTreeSet::new();
    for detail in details.order_book_details {
        if detail.status == "inactive" || detail.market_type == "spot" {
            continue;
        }
        if detail.status != "active" || detail.market_type != "perp" {
            return Err(invalid("unrecognized market status or type"));
        }
        if detail.symbol.is_empty() || !detail.symbol.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(invalid("invalid native market symbol"));
        }
        if detail.market_id > i16::MAX as u16 || detail.quote_asset_id != 0 {
            return Err(invalid(format!(
                "unsupported market id or quote asset for {}",
                detail.symbol
            )));
        }
        if detail.supported_size_decimals > 28 || detail.supported_price_decimals > 28 {
            return Err(invalid(format!(
                "decimal precision exceeds 28 for {}",
                detail.symbol
            )));
        }
        let lot = Decimal::new(1, detail.supported_size_decimals);
        let tick = Decimal::new(1, detail.supported_price_decimals);
        let minimum_base = positive(&detail.min_base_amount, "min_base_amount")?;
        if minimum_base % lot != Decimal::ZERO {
            return Err(invalid("min_base_amount is off the size grid"));
        }
        let min_notional = positive(&detail.min_quote_amount, "min_quote_amount")?;
        if !(1..=10_000).contains(&detail.min_initial_margin_fraction) {
            return Err(invalid("invalid min_initial_margin_fraction"));
        }
        let multiplier = detail
            .multiplier
            .as_deref()
            .map(|value| positive(value, "multiplier"))
            .transpose()?
            .unwrap_or(Decimal::ONE);
        // Missing mark may use the official index, never the last trade.
        if let Some(value) = detail
            .mark_price
            .as_deref()
            .or(detail.index_price.as_deref())
        {
            marks.insert(detail.symbol.clone(), positive(value, "mark/index price")?);
        }
        let market = Market {
            asset: detail.symbol.clone(),
            venue_symbol: detail.symbol.clone(),
            quote_currency: "USDC".into(),
            tick,
            lot,
            min_notional,
            multiplier,
            expiry: None,
            initial_margin: None,
            asset_class: "crypto".into(),
            capabilities: Capabilities {
                fractional: lot < Decimal::ONE,
                short: true,
                funding: true,
                stop_orders: false,
                max_leverage: Decimal::from(10_000)
                    / Decimal::from(detail.min_initial_margin_fraction),
            },
        };
        if !ids.insert(detail.market_id)
            || markets
                .insert(
                    detail.symbol,
                    MarketInfo {
                        market,
                        id: detail.market_id,
                    },
                )
                .is_some()
        {
            return Err(invalid("duplicate active market id or native symbol"));
        }
    }
    if markets.is_empty() {
        return Err(invalid("no active perpetual markets"));
    }
    Ok(Snapshot {
        fetched_at: Instant::now(),
        markets,
        marks,
    })
}

fn order_book(orders: Orders, market: &Market) -> Result<OrderBook, VenueError> {
    if orders.code != 200 {
        return Err(invalid(format!("order book API code {}", orders.code)));
    }
    let levels = |orders: Vec<RestingOrder>, reverse: bool| -> Result<Vec<BookLevel>, VenueError> {
        if orders.len() > BOOK_LIMIT {
            return Err(invalid("order book exceeded requested depth"));
        }
        let mut levels = BTreeMap::<Decimal, Decimal>::new();
        for order in orders {
            let price = positive(&order.price, "book price")?;
            let qty = positive(&order.remaining_base_amount, "remaining_base_amount")?;
            if !market.price_on_grid(price) || !market.qty_on_grid(qty) {
                return Err(invalid(
                    "order book price or quantity is off the market grid",
                ));
            }
            let total = levels.entry(price).or_default();
            *total = total
                .checked_add(qty)
                .ok_or_else(|| invalid("order book quantity overflow"))?;
        }
        let mut out: Vec<_> = levels
            .into_iter()
            .map(|(price, qty)| BookLevel { price, qty })
            .collect();
        if reverse {
            out.reverse();
        }
        Ok(out)
    };
    let book = OrderBook {
        bids: levels(orders.bids, true)?,
        asks: levels(orders.asks, false)?,
    };
    let (Some(bid), Some(ask)) = (book.best_bid(), book.best_ask()) else {
        return Err(invalid("order book is missing a side"));
    };
    if bid >= ask {
        return Err(invalid("crossed or locked order book"));
    }
    Ok(book)
}

impl Lighter {
    pub fn new(base: &str) -> Result<Self, VenueError> {
        let mut base =
            reqwest::Url::parse(base).map_err(|e| invalid(format!("invalid base URL: {e}")))?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || base.path() != "/"
        {
            return Err(invalid(
                "base URL must be an HTTP(S) origin without credentials, path, query or fragment",
            ));
        }
        base.set_path("/api/v1/");
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| invalid(e.to_string()))?;
        Ok(Self {
            inner: Arc::new(Inner {
                base,
                http,
                next_request: Mutex::new(Instant::now()),
                snapshot: Mutex::new(None),
            }),
        })
    }

    async fn pace(&self) {
        let mut next = self.inner.next_request.lock().await;
        tokio::time::sleep_until(*next).await;
        *next = Instant::now() + REQUEST_SPACING;
    }

    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        endpoint: &str,
        query: &[(&str, String)],
    ) -> Result<T, VenueError> {
        self.pace().await;
        let url = self
            .inner
            .base
            .join(endpoint)
            .map_err(|e| invalid(e.to_string()))?;
        let response = self
            .inner
            .http
            .get(url)
            .query(query)
            .send()
            .await
            .map_err(|e| invalid(e.to_string()))?;
        if matches!(response.status().as_u16(), 429 | 405) {
            let seconds = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(60)
                .max(60);
            let mut next = self.inner.next_request.lock().await;
            *next = (*next).max(Instant::now() + Duration::from_secs(seconds.min(3600)));
        }
        if !response.status().is_success() {
            return Err(invalid(format!(
                "{endpoint} returned HTTP {}",
                response.status()
            )));
        }
        response
            .json()
            .await
            .map_err(|e| invalid(format!("invalid {endpoint} response: {e}")))
    }

    async fn refresh(
        &self,
        cached: &mut Option<Snapshot>,
        ttl: Duration,
    ) -> Result<(), VenueError> {
        if cached
            .as_ref()
            .is_some_and(|snapshot| snapshot.fetched_at.elapsed() < ttl)
        {
            return Ok(());
        }
        // Clear expired data before I/O, so no caller can mistake it for fresh
        // state after a network failure or malformed refresh response.
        *cached = None;
        let details = self.get("orderBookDetails", &[]).await?;
        *cached = Some(snapshot(details)?);
        Ok(())
    }

    /// All active-perpetual marks from one cached bulk response. Missing marks
    /// fail the batch; neither old marks nor last-trade prices fill the gaps.
    pub async fn marks(&self) -> Result<BTreeMap<String, Decimal>, VenueError> {
        let mut cached = self.inner.snapshot.lock().await;
        self.refresh(&mut cached, MARK_TTL).await?;
        let snapshot = cached
            .as_ref()
            .expect("successful refresh installs a snapshot");
        for asset in snapshot.markets.keys() {
            if !snapshot.marks.contains_key(asset) {
                return Err(VenueError::NoPrice(asset.clone()));
            }
        }
        Ok(snapshot.marks.clone())
    }

    async fn market(&self, asset: &str) -> Result<MarketInfo, VenueError> {
        let mut cached = self.inner.snapshot.lock().await;
        self.refresh(&mut cached, MARKET_TTL).await?;
        cached
            .as_ref()
            .expect("successful refresh installs a snapshot")
            .markets
            .get(asset)
            .cloned()
            .ok_or_else(|| VenueError::UnknownMarket(asset.into()))
    }
}

#[async_trait]
impl PriceSource for Lighter {
    async fn mark_price(&self, asset: &str) -> Result<Decimal, VenueError> {
        let mut cached = self.inner.snapshot.lock().await;
        self.refresh(&mut cached, MARK_TTL).await?;
        let snapshot = cached
            .as_ref()
            .expect("successful refresh installs a snapshot");
        if !snapshot.markets.contains_key(asset) {
            return Err(VenueError::UnknownMarket(asset.into()));
        }
        snapshot
            .marks
            .get(asset)
            .copied()
            .ok_or_else(|| VenueError::NoPrice(asset.into()))
    }
}

#[async_trait]
impl VenueAdapter for Lighter {
    async fn get_markets(&self) -> Result<Vec<Market>, VenueError> {
        let mut cached = self.inner.snapshot.lock().await;
        self.refresh(&mut cached, MARKET_TTL).await?;
        Ok(cached
            .as_ref()
            .expect("successful refresh installs a snapshot")
            .markets
            .values()
            .map(|info| info.market.clone())
            .collect())
    }
    async fn get_order_book(&self, asset: &str) -> Result<OrderBook, VenueError> {
        let market = self.market(asset).await?;
        let orders = self
            .get(
                "orderBookOrders",
                &[
                    ("market_id", market.id.to_string()),
                    ("limit", BOOK_LIMIT.to_string()),
                ],
            )
            .await?;
        order_book(orders, &market.market)
    }
    async fn get_balances(&self) -> Result<Vec<Balance>, VenueError> {
        Err(VenueError::Unsupported("Lighter account reads"))
    }
    async fn get_positions(&self) -> Result<Vec<Position>, VenueError> {
        Err(VenueError::Unsupported("Lighter account reads"))
    }
    async fn get_open_orders(&self) -> Result<Vec<OpenOrder>, VenueError> {
        Err(VenueError::Unsupported("Lighter account reads"))
    }
    async fn get_fills(&self, _since: Option<OffsetDateTime>) -> Result<Vec<Fill>, VenueError> {
        Err(VenueError::Unsupported("Lighter account reads"))
    }
    async fn place_order(&self, _order: &OrderRequest) -> Result<OrderAck, VenueError> {
        Err(VenueError::Unsupported("Lighter order submission"))
    }
    async fn cancel_order(&self, _venue_order_id: &str) -> Result<(), VenueError> {
        Err(VenueError::Unsupported("Lighter order cancellation"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const DETAILS: &str = include_str!("../tests/fixtures/details.json");
    const AAVE_BOOK: &str = include_str!("../tests/fixtures/aave-orders.json");

    async fn mock(responses: Vec<(u16, String)>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let mut requests = Vec::new();
            for (status, body) in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    let mut buffer = [0; 1024];
                    let count = socket.read(&mut buffer).await.unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&buffer[..count]);
                }
                requests.push(String::from_utf8(request).unwrap());
                let response = format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                socket.write_all(response.as_bytes()).await.unwrap();
            }
            requests
        });
        (base, handle)
    }

    fn parsed(value: serde_json::Value) -> Result<Snapshot, VenueError> {
        snapshot(serde_json::from_value(value).map_err(|e| invalid(e.to_string()))?)
    }

    #[test]
    fn captured_metadata_preserves_native_units_and_only_active_perps() {
        let mut value: serde_json::Value = serde_json::from_str(DETAILS).unwrap();
        for row in value["order_book_details"].as_array_mut().unwrap() {
            row["last_trade_price"] = 1.into();
        }
        let snapshot = parsed(value).unwrap();
        assert_eq!(snapshot.markets.len(), 3);
        assert!(!snapshot.markets.contains_key("MKR"));
        assert!(!snapshot.markets.contains_key("PEPE"));
        let btc = &snapshot.markets["BTC"].market;
        assert_eq!(btc.quote_currency, "USDC");
        assert_eq!(btc.lot, Decimal::new(1, 5));
        assert_eq!(btc.tick, Decimal::new(1, 1));
        assert_eq!(btc.capabilities.max_leverage, Decimal::from(50));
        assert_eq!(btc.min_notional, Decimal::from(10));
        assert_eq!(snapshot.marks["BTC"], Decimal::new(786355, 1));
        assert_eq!(snapshot.markets["1000PEPE"].market.lot, Decimal::ONE);
        assert_eq!(snapshot.markets["1000PEPE"].market.asset, "1000PEPE");
    }

    #[test]
    fn malformed_metadata_is_rejected_and_only_absent_mark_uses_index() {
        for (key, value) in [
            ("supported_size_decimals", serde_json::json!(29)),
            ("min_quote_amount", serde_json::json!("-1")),
            ("min_base_amount", serde_json::json!("0.000001")),
            ("mark_price", serde_json::json!("0")),
            ("mark_price", serde_json::json!("NaN")),
            ("min_initial_margin_fraction", serde_json::json!(0)),
            ("quote_asset_id", serde_json::json!(1)),
        ] {
            let mut body: serde_json::Value = serde_json::from_str(DETAILS).unwrap();
            let btc = body["order_book_details"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|row| row["symbol"] == "BTC")
                .unwrap();
            btc[key] = value;
            assert!(parsed(body).is_err(), "{key}");
        }
        let mut body: serde_json::Value = serde_json::from_str(DETAILS).unwrap();
        let btc = body["order_book_details"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|row| row["symbol"] == "BTC")
            .unwrap();
        btc.as_object_mut().unwrap().remove("mark_price");
        assert_eq!(parsed(body).unwrap().marks["BTC"], Decimal::new(786689, 1));
        let mut body: serde_json::Value = serde_json::from_str(DETAILS).unwrap();
        let duplicate = body["order_book_details"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["symbol"] == "BTC")
            .unwrap()
            .clone();
        body["order_book_details"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        assert!(parsed(body).is_err());
    }

    #[test]
    fn captured_orders_aggregate_remaining_size_and_reject_bad_books() {
        let metadata = parsed(serde_json::from_str(DETAILS).unwrap()).unwrap();
        let market = &metadata.markets["AAVE"].market;
        let mut body: serde_json::Value = serde_json::from_str(AAVE_BOOK).unwrap();
        body["asks"][1]["price"] = body["asks"][0]["price"].clone();
        let book = order_book(serde_json::from_value(body.clone()).unwrap(), market).unwrap();
        assert_eq!(book.asks[0].qty, Decimal::new(17875, 3));
        assert_eq!(book.asks.len(), 3);
        assert!(book
            .bids
            .windows(2)
            .all(|pair| pair[0].price > pair[1].price));
        assert!(book
            .asks
            .windows(2)
            .all(|pair| pair[0].price < pair[1].price));
        for (key, value) in [
            ("price", "0"),
            ("price", "130.0001"),
            ("remaining_base_amount", "-1"),
            ("remaining_base_amount", "0.0001"),
        ] {
            let mut bad = body.clone();
            bad["asks"][0][key] = value.into();
            assert!(order_book(serde_json::from_value(bad).unwrap(), market).is_err());
        }
        let mut crossed = body.clone();
        crossed["bids"][0]["price"] = body["asks"][0]["price"].clone();
        assert!(order_book(serde_json::from_value(crossed).unwrap(), market).is_err());
        body["asks"] = serde_json::json!([]);
        assert!(order_book(serde_json::from_value(body).unwrap(), market).is_err());
    }

    #[tokio::test]
    async fn cloned_clients_share_bulk_cache_and_only_issue_public_gets() {
        let (base, server) = mock(vec![(200, DETAILS.into()), (200, AAVE_BOOK.into())]).await;
        let client = Lighter::new(&base).unwrap();
        let clone = client.clone();
        assert_eq!(client.get_markets().await.unwrap().len(), 3);
        assert_eq!(
            clone.mark_price("BTC").await.unwrap(),
            Decimal::new(786355, 1)
        );
        assert_eq!(clone.marks().await.unwrap().len(), 3);
        assert!(matches!(
            clone.mark_price("PEPE").await,
            Err(VenueError::UnknownMarket(_))
        ));
        assert!(
            client
                .get_order_book("AAVE")
                .await
                .unwrap()
                .spread_bps()
                .unwrap()
                > Decimal::ZERO
        );
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with("GET /api/v1/orderBookDetails "));
        assert!(requests[1].starts_with("GET /api/v1/orderBookOrders?market_id=27&limit=100 "));
        assert!(requests
            .iter()
            .all(|request| !request.to_lowercase().contains("authorization:")));
    }

    #[tokio::test]
    async fn expired_mark_cache_cannot_hide_a_refresh_failure() {
        let (base, server) = mock(vec![(200, DETAILS.into()), (503, "{}".into())]).await;
        let client = Lighter::new(&base).unwrap();
        client.mark_price("BTC").await.unwrap();
        client
            .inner
            .snapshot
            .lock()
            .await
            .as_mut()
            .unwrap()
            .fetched_at = Instant::now() - MARK_TTL;
        assert!(client.clone().marks().await.is_err());
        assert!(client.inner.snapshot.lock().await.is_none());
        assert_eq!(server.await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn missing_official_prices_never_use_last_trade_or_omit_from_bulk_silently() {
        let mut body: serde_json::Value = serde_json::from_str(DETAILS).unwrap();
        let btc = body["order_book_details"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|row| row["symbol"] == "BTC")
            .unwrap();
        btc.as_object_mut().unwrap().remove("mark_price");
        btc.as_object_mut().unwrap().remove("index_price");
        btc["last_trade_price"] = 90000.into();
        let (base, server) = mock(vec![(200, body.to_string())]).await;
        let client = Lighter::new(&base).unwrap();
        assert_eq!(client.get_markets().await.unwrap().len(), 3);
        assert!(matches!(
            client.mark_price("BTC").await,
            Err(VenueError::NoPrice(_))
        ));
        assert!(matches!(client.marks().await, Err(VenueError::NoPrice(_))));
        assert_eq!(server.await.unwrap().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn pacing_is_shared_across_clones() {
        let client = Lighter::new("http://localhost").unwrap();
        let start = Instant::now();
        client.pace().await;
        client.clone().pace().await;
        assert!(start.elapsed() >= REQUEST_SPACING);
        client.pace().await;
        assert!(start.elapsed() >= REQUEST_SPACING * 2);
    }

    #[tokio::test]
    async fn account_reads_and_all_order_methods_are_unsupported_without_network() {
        let client = Lighter::new("http://127.0.0.1:1").unwrap();
        assert!(matches!(
            client.get_balances().await,
            Err(VenueError::Unsupported(_))
        ));
        assert!(matches!(
            client.get_positions().await,
            Err(VenueError::Unsupported(_))
        ));
        assert!(matches!(
            client.get_open_orders().await,
            Err(VenueError::Unsupported(_))
        ));
        assert!(matches!(
            client.get_fills(None).await,
            Err(VenueError::Unsupported(_))
        ));
        assert!(matches!(
            client.cancel_order("id").await,
            Err(VenueError::Unsupported(_))
        ));
        let order = OrderRequest {
            client_order_id: "paper-only".into(),
            asset: "BTC".into(),
            side: venue::Side::Buy,
            qty: Decimal::ONE,
            order_type: venue::OrderType::Market,
            limit_price: None,
            reason: venue::OrderReason::Entry,
        };
        assert!(matches!(
            client.place_order(&order).await,
            Err(VenueError::Unsupported(_))
        ));
    }

    #[test]
    fn invalid_base_urls_are_rejected() {
        for base in [
            "not a URL",
            "ftp://localhost",
            "https://user:secret@localhost",
            "https://localhost/api/v1",
            "https://localhost?key=x",
        ] {
            assert!(Lighter::new(base).is_err());
        }
    }

    #[tokio::test]
    #[ignore = "reads public Lighter mainnet market data"]
    async fn live_smoke() {
        let client = Lighter::new(MAINNET).unwrap();
        let markets = client.get_markets().await.unwrap();
        assert!(markets.iter().any(|market| market.asset == "BTC"));
        let marks = client.marks().await.unwrap();
        let mark = client.mark_price("BTC").await.unwrap();
        assert_eq!(mark, marks["BTC"]);
        let book = client.get_order_book("BTC").await.unwrap();
        assert!(book.best_bid().unwrap() < book.best_ask().unwrap());
        eprintln!(
            "{} active perps; BTC mark {mark}; bid {} ask {}",
            markets.len(),
            book.best_bid().unwrap(),
            book.best_ask().unwrap()
        );
        assert!(matches!(
            client.cancel_order("not-submitted").await,
            Err(VenueError::Unsupported(_))
        ));
    }
}
