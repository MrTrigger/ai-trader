//! The other end of the seam: a Python-written Plan, executed against a venue.
//!
//! `plan`'s round-trip test proves Rust can *parse* what Python wrote. This
//! proves the parsed thing is directly usable — that a plan's orders reach a
//! venue with no translation layer between them, which is why `venue`
//! re-exports `Side`, `OrderType` and `OrderReason` from the contract instead
//! of declaring its own.
//!
//! It reads the same committed fixture the round-trip test does. That file is
//! regenerated from the planner and diff-checked in CI, so it is the one
//! artifact in the repo guaranteed to be what Python currently emits:
//!
//!     AI_TRADER_UPDATE_FIXTURE=1 pytest tests/test_fixture.py
//!
//! This is not the executor. Sequencing (exits before entries), pause
//! handling, reconciliation and the run record are Phase 2 and live above this
//! layer. What is asserted here is only that the layer below them holds.

use paper::{PaperConfig, PaperVenue};
use plan::Plan;
use rust_decimal::Decimal;
use std::str::FromStr;
use venue::{
    Capabilities, ManualPrices, Market, OrderRequest, OrderState, SystemClock, VenueAdapter,
    VenueError,
};

const FIXTURE: &str = include_str!("../../plan/tests/fixtures/plan.json");

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn market(asset: &str, capabilities: Capabilities) -> Market {
    Market {
        asset: asset.into(),
        venue_symbol: format!("{asset}USDT"),
        quote_currency: "USD".into(),
        tick: dec("0.01"),
        lot: dec("0.00000001"),
        min_notional: dec("10"),
        multiplier: Decimal::ONE,
        expiry: None,
        initial_margin: None,
        asset_class: "crypto".into(),
        capabilities,
    }
}

/// A plan's order, as the venue takes it.
///
/// The whole body is field-for-field: no enum mapping, no sign convention to
/// reconcile, no unit conversion. The only thing added is the idempotency key.
///
/// That key is derived from the plan id and the order's position in the plan,
/// so it is the *same key* on every attempt to execute that plan. This is what
/// makes re-running a half-finished execution converge instead of double
/// filling (spec §3.2) — and it is why it is computed here rather than
/// generated fresh per attempt.
fn to_request(plan: &Plan, index: usize) -> OrderRequest {
    let order = &plan.orders[index];
    OrderRequest {
        client_order_id: format!("{}-{index}", plan.plan_id),
        asset: order.asset.clone(),
        side: order.side,
        qty: order.qty,
        order_type: order.order_type,
        limit_price: order.limit_price,
        reason: order.reason,
    }
}

fn venue_for(
    eth: Capabilities,
) -> PaperVenue<venue::MarketsWithPrices<Vec<Market>, ManualPrices>, SystemClock> {
    let prices = ManualPrices::new();
    prices.set("BTC", dec("64000.00"));
    prices.set("ETH", dec("3000.00"));

    PaperVenue::new(
        PaperConfig {
            // The fixture's own NAV. Its `nav.cash` alone would not cover the
            // BTC buy — the plan expects the ETH proceeds first. Sequencing is
            // the executor's job, not the adapter's, so this test funds the
            // account rather than asserting an ordering policy that does not
            // exist yet.
            initial_cash: dec("100000.00"),
            ..Default::default()
        },
        venue::MarketsWithPrices {
            markets: vec![market("BTC", Capabilities::spot()), market("ETH", eth)],
            prices,
        },
        SystemClock,
    )
}

fn shortable() -> Capabilities {
    Capabilities {
        short: true,
        funding: true,
        ..Capabilities::spot()
    }
}

#[tokio::test]
async fn a_python_written_plan_executes_against_the_paper_venue() {
    let plan = Plan::parse(FIXTURE).expect("the committed fixture must parse");
    assert!(plan.is_executable(), "the fixture is a live, accepted plan");
    assert_eq!(plan.orders.len(), 2);

    let venue = venue_for(shortable());
    for index in 0..plan.orders.len() {
        let ack = venue.place_order(&to_request(&plan, index)).await.unwrap();
        assert_eq!(ack.state, OrderState::Filled);
    }

    let fills = venue.get_fills(None).await.unwrap();
    assert_eq!(fills.len(), 2);

    // The BTC buy is a limit at 64000.50 with the mark at 64000: marketable,
    // and filled at its own price rather than the better mark.
    assert_eq!(fills[0].asset, "BTC");
    assert_eq!(fills[0].qty, plan.orders[0].qty);
    assert_eq!(fills[0].price, dec("64000.50"));

    // The ETH sell is a market order, so it pays slippage.
    assert_eq!(fills[1].asset, "ETH");
    assert_eq!(fills[1].qty, plan.orders[1].qty);
    assert!(fills[1].price < dec("3000.00"));

    let positions = venue.get_positions().await.unwrap();
    assert_eq!(positions.len(), 2);
    assert_eq!(positions[0].qty, dec("0.39753288"));
    assert_eq!(
        positions[1].qty,
        -dec("13.42209814"),
        "the plan's ETH target is a short (weight -0.250000)"
    );
}

#[tokio::test]
async fn re_executing_the_same_plan_changes_nothing() {
    // The crash-safe property, at plan granularity: an executor that died
    // partway and is restarted re-submits every order in the plan, and the
    // book ends up where the plan said rather than at twice it.
    let plan = Plan::parse(FIXTURE).unwrap();
    let venue = venue_for(shortable());

    for _attempt in 0..3 {
        for index in 0..plan.orders.len() {
            venue.place_order(&to_request(&plan, index)).await.unwrap();
        }
    }

    assert_eq!(venue.get_fills(None).await.unwrap().len(), 2);
    let positions = venue.get_positions().await.unwrap();
    assert_eq!(positions[0].qty, dec("0.39753288"));
    assert_eq!(positions[1].qty, -dec("13.42209814"));
}

#[tokio::test]
async fn a_venue_that_cannot_short_refuses_the_plans_short_leg() {
    // Same plan, same code, a market that declares `short: false`. The refusal
    // comes from the capability, and nothing anywhere consulted a venue name —
    // which is the property that makes Phase 6's second adapter additive.
    let plan = Plan::parse(FIXTURE).unwrap();
    let venue = venue_for(Capabilities::spot());

    venue.place_order(&to_request(&plan, 0)).await.unwrap();

    let err = venue.place_order(&to_request(&plan, 1)).await.unwrap_err();
    match err {
        VenueError::ShortNotSupported { asset, resulting } => {
            assert_eq!(asset, "ETH");
            assert_eq!(resulting, -dec("13.42209814"));
        }
        other => panic!("expected a capability refusal, got {other}"),
    }

    // And the refusal left the book alone: one fill, not a partial application
    // of a plan the risk layer cleared as a whole.
    assert_eq!(venue.get_fills(None).await.unwrap().len(), 1);
}
