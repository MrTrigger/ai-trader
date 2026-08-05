//! What the `paper` venue promises, asserted.
//!
//! Two themes run through these tests, and both are deliberate:
//!
//! **Every rounding goes against the account.** Slippage, tick rounding and
//! fee rounding are each checked for direction, not just for magnitude. A
//! paper venue that quietly flatters the strategy invalidates Phase 2 rather
//! than failing it, which is the worse outcome — you would not know.
//!
//! **Every refusal is a refusal, not an adjustment.** An off-lot quantity is
//! rejected, never rounded onto the grid; a plan the risk layer cleared is not
//! something the adapter may edit on its way out.

use paper::{PaperConfig, PaperVenue};
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::Arc;
use time::{Duration, OffsetDateTime};
use venue::{
    Capabilities, Clock, ManualClock, ManualPrices, Market, OrderReason, OrderRequest, OrderState,
    OrderType, Side, VenueAdapter, VenueError,
};

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

/// BTC is plain spot. ETH allows shorting — the two capability worlds, so a
/// test can show the engine adapting to the declaration rather than to a name.
fn markets() -> Vec<Market> {
    vec![
        Market {
            asset: "BTC".into(),
            venue_symbol: "BTCUSDT".into(),
            quote_currency: "USD".into(),
            tick: dec("0.01"),
            lot: dec("0.00000001"),
            min_notional: dec("10"),
            capabilities: Capabilities::spot(),
        },
        Market {
            asset: "ETH".into(),
            venue_symbol: "ETH-USD-PERP".into(),
            quote_currency: "USD".into(),
            tick: dec("0.01"),
            lot: dec("0.00000001"),
            min_notional: dec("10"),
            capabilities: Capabilities {
                short: true,
                funding: true,
                ..Capabilities::spot()
            },
        },
    ]
}

struct Harness {
    venue: PaperVenue<Arc<ManualPrices>, Arc<ManualClock>>,
    prices: Arc<ManualPrices>,
    clock: Arc<ManualClock>,
}

fn harness(cash: &str) -> Harness {
    harness_with(PaperConfig {
        initial_cash: dec(cash),
        ..Default::default()
    })
}

fn harness_with(config: PaperConfig) -> Harness {
    let prices = Arc::new(ManualPrices::new());
    prices.set("BTC", dec("60000"));
    prices.set("ETH", dec("3000"));

    let clock = Arc::new(ManualClock::new(
        OffsetDateTime::from_unix_timestamp(1_780_000_000).unwrap(),
    ));

    Harness {
        venue: PaperVenue::new(config, markets(), prices.clone(), clock.clone()),
        prices,
        clock,
    }
}

fn order(id: &str, asset: &str, side: Side, qty: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: id.into(),
        asset: asset.into(),
        side,
        qty: dec(qty),
        order_type: OrderType::Market,
        limit_price: None,
        reason: OrderReason::Entry,
    }
}

fn limit(id: &str, asset: &str, side: Side, qty: &str, price: &str) -> OrderRequest {
    OrderRequest {
        order_type: OrderType::Limit,
        limit_price: Some(dec(price)),
        ..order(id, asset, side, qty)
    }
}

async fn cash_of(h: &Harness) -> Decimal {
    h.venue
        .get_balances()
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.currency == "USD")
        .unwrap()
        .total
}

async fn available_of(h: &Harness, currency: &str) -> Decimal {
    h.venue
        .get_balances()
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.currency == currency)
        .map(|b| b.available)
        .unwrap_or(Decimal::ZERO)
}

// --- filling ---------------------------------------------------------------

#[tokio::test]
async fn a_market_buy_fills_and_the_money_adds_up() {
    let h = harness("100000");
    let ack = h
        .venue
        .place_order(&order("c1", "BTC", Side::Buy, "0.5"))
        .await
        .unwrap();
    assert_eq!(ack.state, OrderState::Filled);

    let fills = h.venue.get_fills(None).await.unwrap();
    assert_eq!(fills.len(), 1);
    // 60000 marked up by 5bps of slippage.
    assert_eq!(fills[0].price, dec("60030.00"));
    assert_eq!(fills[0].notional(), dec("30015.000"));
    // 10bps taker on 30015.
    assert_eq!(fills[0].fee, dec("30.015"));
    assert_eq!(fills[0].fee_currency, "USD");

    // Cash is the fold of the log, not a number kept alongside it.
    assert_eq!(
        cash_of(&h).await,
        dec("100000") - dec("30015.000") - dec("30.015")
    );

    let positions = h.venue.get_positions().await.unwrap();
    assert_eq!(positions.len(), 1);
    assert_eq!(positions[0].qty, dec("0.5"));
    assert_eq!(positions[0].avg_price, dec("60030.00"));
}

#[tokio::test]
async fn slippage_moves_the_price_against_the_account_both_ways() {
    let h = harness("100000");
    h.venue
        .place_order(&order("buy", "BTC", Side::Buy, "1"))
        .await
        .unwrap();
    h.venue
        .place_order(&order("sell", "BTC", Side::Sell, "1"))
        .await
        .unwrap();

    let fills = h.venue.get_fills(None).await.unwrap();
    assert!(fills[0].price > dec("60000"), "a buy pays up");
    assert!(fills[1].price < dec("60000"), "a sell gets less");
    // And the round trip loses money at a flat price, which is the point.
    assert!(cash_of(&h).await < dec("100000"));
}

#[tokio::test]
async fn a_limit_order_rests_until_the_mark_reaches_it() {
    let h = harness("100000");
    let ack = h
        .venue
        .place_order(&limit("c1", "BTC", Side::Buy, "0.5", "55000.00"))
        .await
        .unwrap();
    assert_eq!(ack.state, OrderState::Open);
    assert!(h.venue.get_fills(None).await.unwrap().is_empty());

    // Still above the limit: still nothing.
    h.prices.set("BTC", dec("56000"));
    h.venue.poll().await.unwrap();
    assert!(h.venue.get_fills(None).await.unwrap().is_empty());

    h.prices.set("BTC", dec("54000"));
    let fills = h.venue.get_fills(None).await.unwrap();
    assert_eq!(fills.len(), 1);
    assert_eq!(
        fills[0].price,
        dec("55000.00"),
        "fills at its own price - a paper venue handing out price improvement is inventing edge"
    );
    // Resting, so it paid the maker rate: 5bps of 27500.
    assert_eq!(fills[0].fee, dec("13.75"));
}

#[tokio::test]
async fn a_marketable_limit_fills_at_its_limit_not_at_the_better_mark() {
    let h = harness("100000");
    // Willing to pay 61000 when the mark is 60000. A real book would fill this
    // near 60000; this one charges the full 61000, which is the pessimistic
    // reading and the one that cannot flatter a backtest comparison.
    let ack = h
        .venue
        .place_order(&limit("c1", "BTC", Side::Buy, "0.5", "61000.00"))
        .await
        .unwrap();
    assert_eq!(ack.state, OrderState::Filled);
    assert_eq!(
        h.venue.get_fills(None).await.unwrap()[0].price,
        dec("61000.00")
    );
}

#[tokio::test]
async fn fee_rounding_never_favours_the_account() {
    // 2dp quote and a 1bp fee, sized so the exact fee is 1.5001. Ordinary
    // rounding gives 1.50 and hands the account a tenth of a cent it did not
    // earn; this must give 1.51.
    let h = harness_with(PaperConfig {
        initial_cash: dec("100000"),
        quote_decimals: 2,
        slippage_bps: Decimal::ZERO,
        maker_fee_bps: Decimal::ONE,
        taker_fee_bps: Decimal::ONE,
        ..Default::default()
    });
    h.venue
        .place_order(&limit("c1", "BTC", Side::Buy, "0.25", "60004.00"))
        .await
        .unwrap();
    let fills = h.venue.get_fills(None).await.unwrap();
    assert_eq!(fills[0].notional(), dec("15001.0000"));
    assert_eq!(fills[0].fee, dec("1.51"), "1.5001 rounds up, never down");
}

// --- balances and reservations ---------------------------------------------

#[tokio::test]
async fn resting_orders_claim_the_cash_they_will_need() {
    let h = harness("100000");
    h.venue
        .place_order(&limit("c1", "BTC", Side::Buy, "1", "55000.00"))
        .await
        .unwrap();

    assert_eq!(
        cash_of(&h).await,
        dec("100000"),
        "nothing has been spent yet"
    );
    // 55000 notional plus the worse of the two fee rates.
    assert_eq!(
        available_of(&h, "USD").await,
        dec("100000") - dec("55000") - dec("55")
    );

    // A second one of the same size does not fit, even though `total` would cover it.
    let err = h
        .venue
        .place_order(&limit("c2", "BTC", Side::Buy, "1", "55000.00"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, VenueError::InsufficientBalance { .. }),
        "two resting buys must not both be affordable out of the same cash, got {err}"
    );
}

#[tokio::test]
async fn cancelling_releases_the_claim() {
    let h = harness("100000");
    let ack = h
        .venue
        .place_order(&limit("c1", "BTC", Side::Buy, "1", "55000.00"))
        .await
        .unwrap();
    assert!(available_of(&h, "USD").await < dec("100000"));

    h.venue.cancel_order(&ack.venue_order_id).await.unwrap();
    assert_eq!(available_of(&h, "USD").await, dec("100000"));
    assert!(h.venue.get_fills(None).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_resting_sell_claims_the_inventory() {
    let h = harness("100000");
    h.venue
        .place_order(&order("buy", "BTC", Side::Buy, "1"))
        .await
        .unwrap();
    h.venue
        .place_order(&limit("rest", "BTC", Side::Sell, "1", "70000.00"))
        .await
        .unwrap();

    assert_eq!(available_of(&h, "BTC").await, Decimal::ZERO);
    let err = h
        .venue
        .place_order(&order("again", "BTC", Side::Sell, "1"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, VenueError::InsufficientBalance { .. }),
        "got {err}"
    );
}

#[tokio::test]
async fn positions_are_derived_from_fills_and_a_round_trip_leaves_nothing() {
    let h = harness("100000");
    h.venue
        .place_order(&order("a", "BTC", Side::Buy, "0.1"))
        .await
        .unwrap();
    h.venue
        .place_order(&order("b", "BTC", Side::Buy, "0.2"))
        .await
        .unwrap();
    h.venue
        .place_order(&order("c", "BTC", Side::Sell, "0.3"))
        .await
        .unwrap();

    assert!(
        h.venue.get_positions().await.unwrap().is_empty(),
        "no dust left behind - this is the test that fails under f64"
    );
    // The fill log still has all three. History is never edited (spec 0.7).
    assert_eq!(h.venue.get_fills(None).await.unwrap().len(), 3);
    // Only cash remains as a balance; the zero BTC row is not reported.
    let balances = h.venue.get_balances().await.unwrap();
    assert_eq!(balances.len(), 1);
    assert_eq!(balances[0].currency, "USD");
}

// --- failing closed --------------------------------------------------------

#[tokio::test]
async fn an_off_lot_quantity_is_refused_not_rounded() {
    let h = harness("100000");
    let err = h
        .venue
        .place_order(&order("c1", "BTC", Side::Buy, "0.000000001"))
        .await
        .unwrap_err();
    match err {
        VenueError::LotSize { qty, lot, .. } => {
            assert_eq!(qty, dec("0.000000001"));
            assert_eq!(lot, dec("0.00000001"));
        }
        other => panic!("expected a lot-size refusal, got {other}"),
    }
    assert!(h.venue.get_fills(None).await.unwrap().is_empty());
}

#[tokio::test]
async fn an_off_tick_limit_price_is_refused() {
    let h = harness("100000");
    let err = h
        .venue
        .place_order(&limit("c1", "BTC", Side::Buy, "1", "60000.005"))
        .await
        .unwrap_err();
    assert!(matches!(err, VenueError::TickSize { .. }), "got {err}");
}

#[tokio::test]
async fn an_order_below_the_venue_minimum_is_refused() {
    let h = harness("100000");
    // 0.0001 BTC at 60000 is 6 USD, under the 10 USD floor.
    let err = h
        .venue
        .place_order(&order("c1", "BTC", Side::Buy, "0.0001"))
        .await
        .unwrap_err();
    match err {
        VenueError::BelowMinNotional {
            notional,
            min_notional,
            ..
        } => {
            assert_eq!(notional, dec("6.0000"));
            assert_eq!(min_notional, dec("10"));
        }
        other => panic!("expected a min-notional refusal, got {other}"),
    }
}

#[tokio::test]
async fn a_zero_quantity_is_refused() {
    let h = harness("100000");
    let err = h
        .venue
        .place_order(&order("c1", "BTC", Side::Buy, "0"))
        .await
        .unwrap_err();
    assert!(matches!(err, VenueError::NonPositiveQty(_)), "got {err}");
}

#[tokio::test]
async fn an_unknown_asset_is_refused() {
    let h = harness("100000");
    let err = h
        .venue
        .place_order(&order("c1", "DOGE", Side::Buy, "1"))
        .await
        .unwrap_err();
    assert!(matches!(err, VenueError::UnknownMarket(_)), "got {err}");
}

#[tokio::test]
async fn a_market_order_with_no_price_is_refused_rather_than_guessed() {
    let h = harness("100000");
    h.prices.clear("BTC");
    let err = h
        .venue
        .place_order(&order("c1", "BTC", Side::Buy, "1"))
        .await
        .unwrap_err();
    assert!(matches!(err, VenueError::NoPrice(_)), "got {err}");
}

#[tokio::test]
async fn insufficient_cash_is_refused() {
    let h = harness("1000");
    let err = h
        .venue
        .place_order(&order("c1", "BTC", Side::Buy, "1"))
        .await
        .unwrap_err();
    match err {
        VenueError::InsufficientBalance {
            currency,
            available,
            ..
        } => {
            assert_eq!(currency, "USD");
            assert_eq!(available, dec("1000"));
        }
        other => panic!("expected an affordability refusal, got {other}"),
    }
}

#[tokio::test]
async fn the_short_capability_decides_and_the_venue_name_does_not() {
    let h = harness("100000");

    // BTC is declared spot: selling what is not held would go short.
    let err = h
        .venue
        .place_order(&order("c1", "BTC", Side::Sell, "1"))
        .await
        .unwrap_err();
    match err {
        VenueError::ShortNotSupported { asset, resulting } => {
            assert_eq!(asset, "BTC");
            assert_eq!(resulting, dec("-1"));
        }
        other => panic!("expected a capability refusal, got {other}"),
    }

    // ETH declares short. Same code path, opposite answer.
    h.venue
        .place_order(&order("c2", "ETH", Side::Sell, "1"))
        .await
        .unwrap();
    let positions = h.venue.get_positions().await.unwrap();
    assert_eq!(positions[0].asset, "ETH");
    assert_eq!(positions[0].qty, dec("-1"));
}

#[tokio::test]
async fn cancelling_an_order_that_already_filled_is_an_error() {
    let h = harness("100000");
    let ack = h
        .venue
        .place_order(&limit("c1", "BTC", Side::Buy, "0.5", "55000.00"))
        .await
        .unwrap();
    h.prices.set("BTC", dec("54000"));
    h.venue.poll().await.unwrap();

    let err = h.venue.cancel_order(&ack.venue_order_id).await.unwrap_err();
    assert!(
        matches!(err, VenueError::UnknownOrder(_)),
        "cancelling something already gone means our view of the venue is wrong, got {err}"
    );
}

// --- idempotency -----------------------------------------------------------

#[tokio::test]
async fn a_replayed_order_id_does_not_place_a_second_order() {
    // The crash-safety property: an executor that dies after the venue
    // accepted but before it recorded the ack re-sends, and converges.
    let h = harness("100000");
    let req = order("c1", "BTC", Side::Buy, "0.5");

    let first = h.venue.place_order(&req).await.unwrap();
    let second = h.venue.place_order(&req).await.unwrap();

    assert_eq!(first.venue_order_id, second.venue_order_id);
    assert_eq!(h.venue.get_fills(None).await.unwrap().len(), 1);
    assert_eq!(h.venue.get_positions().await.unwrap()[0].qty, dec("0.5"));
}

#[tokio::test]
async fn a_replay_is_answered_even_once_the_account_could_no_longer_afford_it() {
    // The answer to "did you take this order" is fixed once given. Re-judging
    // a replay against a book that has moved is how a retry turns into a
    // spurious rejection of an order that is already working.
    let h = harness("70000");
    let req = limit("c1", "BTC", Side::Buy, "1", "55000.00");
    let first = h.venue.place_order(&req).await.unwrap();
    assert_eq!(first.state, OrderState::Open);

    // The reservation now leaves nowhere near enough for the same order again,
    // as a fresh id demonstrates...
    let mut fresh = req.clone();
    fresh.client_order_id = "c2".into();
    assert!(matches!(
        h.venue.place_order(&fresh).await,
        Err(VenueError::InsufficientBalance { .. })
    ));

    // ...but the replay is answered from what was already decided.
    let replay = h.venue.place_order(&req).await.unwrap();
    assert_eq!(replay.venue_order_id, first.venue_order_id);
    assert_eq!(replay.state, OrderState::Open);
    assert_eq!(h.venue.get_fills(None).await.unwrap().len(), 0);
}

#[tokio::test]
async fn reusing_an_order_id_for_a_different_order_is_an_error() {
    let h = harness("100000");
    h.venue
        .place_order(&order("c1", "BTC", Side::Buy, "0.5"))
        .await
        .unwrap();
    let err = h
        .venue
        .place_order(&order("c1", "BTC", Side::Buy, "0.6"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, VenueError::ClientOrderIdReused { .. }),
        "an id identifies one order and is not a way to amend it, got {err}"
    );
    assert_eq!(h.venue.get_fills(None).await.unwrap().len(), 1);
}

// --- the fill log ----------------------------------------------------------

#[tokio::test]
async fn fills_are_returned_oldest_first_and_filter_by_since() {
    let h = harness("100000");
    h.venue
        .place_order(&order("a", "BTC", Side::Buy, "0.1"))
        .await
        .unwrap();

    h.clock.advance(Duration::hours(1));
    let cutoff = h.clock.now();
    h.venue
        .place_order(&order("b", "BTC", Side::Buy, "0.1"))
        .await
        .unwrap();

    let all = h.venue.get_fills(None).await.unwrap();
    assert_eq!(all.len(), 2);
    assert!(all[0].ts < all[1].ts);
    assert!(all.windows(2).all(|w| w[0].ts <= w[1].ts));

    let recent = h.venue.get_fills(Some(cutoff)).await.unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].client_order_id, "b");
}

#[tokio::test]
async fn every_fill_carries_a_distinct_id_and_its_order() {
    let h = harness("100000");
    h.venue
        .place_order(&order("a", "BTC", Side::Buy, "0.1"))
        .await
        .unwrap();
    h.venue
        .place_order(&order("b", "ETH", Side::Sell, "1"))
        .await
        .unwrap();

    let fills = h.venue.get_fills(None).await.unwrap();
    assert_ne!(fills[0].venue_fill_id, fills[1].venue_fill_id);
    assert_ne!(fills[0].venue_order_id, fills[1].venue_order_id);
    assert_eq!(fills[0].client_order_id, "a");
    assert_eq!(fills[1].client_order_id, "b");
    assert!(
        fills.iter().all(|f| f.qty > Decimal::ZERO),
        "qty is unsigned; side carries direction"
    );
    assert!(
        fills.iter().all(|f| f.fee > Decimal::ZERO),
        "a fee is a cost"
    );
}

#[tokio::test]
async fn the_venue_is_usable_behind_a_trait_object() {
    // The engine holds `dyn VenueAdapter` and never learns which venue it has.
    // If this stops compiling, adding the second venue stops being additive.
    let h = harness("100000");
    let adapter: &dyn VenueAdapter = &h.venue;
    adapter
        .place_order(&order("c1", "BTC", Side::Buy, "0.5"))
        .await
        .unwrap();
    assert_eq!(adapter.get_positions().await.unwrap().len(), 1);
    assert_eq!(adapter.get_markets().await.unwrap().len(), 2);
}

// --- surviving a restart ---------------------------------------------------

#[tokio::test]
async fn a_snapshot_restores_the_book_the_cash_and_the_idempotency_map() {
    // The fill log is the only stored truth here. A restart that lost it would
    // reconcile a real intention against a book that had forgotten everything.
    let h = harness("100000");
    h.venue
        .place_order(&order("r1", "BTC", Side::Buy, "0.5"))
        .await
        .unwrap();
    let before_pos = h.venue.get_positions().await.unwrap();
    let before_cash = h.venue.get_balances().await.unwrap();
    let snapshot = h.venue.snapshot().await;

    let back = PaperVenue::restore(
        PaperConfig {
            initial_cash: dec("100000"),
            ..Default::default()
        },
        markets(),
        h.prices.clone(),
        h.clock.clone(),
        &snapshot,
    )
    .expect("a snapshot this venue wrote is one it can read");

    assert_eq!(back.get_positions().await.unwrap(), before_pos);
    assert_eq!(back.get_balances().await.unwrap(), before_cash);
    assert_eq!(back.get_fills(None).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_replayed_order_after_a_restart_does_not_place_a_second_one() {
    // The idempotency map is part of the state for exactly this reason: a
    // process that died between submitting and recording must be safe to
    // restart, and a restored venue that had forgotten the order would fill it
    // twice.
    let h = harness("100000");
    let req = order("r2", "BTC", Side::Buy, "0.5");
    let first = h.venue.place_order(&req).await.unwrap();
    let snapshot = h.venue.snapshot().await;

    let back = PaperVenue::restore(
        PaperConfig {
            initial_cash: dec("100000"),
            ..Default::default()
        },
        markets(),
        h.prices.clone(),
        h.clock.clone(),
        &snapshot,
    )
    .unwrap();

    let again = back.place_order(&req).await.unwrap();
    assert_eq!(again.venue_order_id, first.venue_order_id);
    assert_eq!(
        back.get_fills(None).await.unwrap().len(),
        1,
        "the replay returned the original ack rather than doubling the position"
    );
}

#[tokio::test]
async fn a_restored_venue_keeps_resting_orders_waiting() {
    let h = harness("100000");
    h.venue
        .place_order(&OrderRequest {
            order_type: OrderType::Limit,
            limit_price: Some(dec("50000")),
            ..order("r3", "BTC", Side::Buy, "0.5")
        })
        .await
        .unwrap();
    assert!(h.venue.get_fills(None).await.unwrap().is_empty());

    let back = PaperVenue::restore(
        PaperConfig {
            initial_cash: dec("100000"),
            ..Default::default()
        },
        markets(),
        h.prices.clone(),
        h.clock.clone(),
        &h.venue.snapshot().await,
    )
    .unwrap();

    // The mark reaches the limit only after the restart; the order is still
    // there to be filled.
    h.prices.set("BTC", dec("49000"));
    h.clock.advance(Duration::minutes(1));
    assert_eq!(back.get_fills(None).await.unwrap().len(), 1);
}
