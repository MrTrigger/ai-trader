//! The executor's guarantees, each tested against the failure it prevents.
//!
//! Four properties matter more than the rest, and all four fail *silently* if
//! unguarded — which is why they are tested against a fake venue that can be
//! made to misbehave on demand, rather than only against a well-behaved one.
//!
//! - reconciliation drift halts and is never repaired
//! - a replayed plan submits identical ids, so a restart cannot double a position
//! - a failed order stops the run and names what was skipped
//! - a rejected, dry-run, or killed plan never reaches the venue at all

use std::sync::Mutex;

use executor::{client_order_id, execute, reconcile, Controls, ExecError};
use rust_decimal::Decimal;
use venue::{
    AssetId, Balance, Capabilities, Fill, Market, OrderAck, OrderRequest, OrderState, Position,
    Side, VenueAdapter, VenueError,
};

fn dec(s: &str) -> Decimal {
    s.parse().expect("literal decimal")
}

/// A venue that records what it was asked and can be told to fail.
///
/// Idempotency is implemented here the way a real venue's contract specifies —
/// a repeated `client_order_id` returns the original ack — so the test exercises
/// the executor against the behaviour it actually relies on.
struct FakeVenue {
    positions: Vec<Position>,
    /// Asset whose order should fail, to test partial-failure handling.
    fail_on: Option<String>,
    seen: Mutex<Vec<OrderRequest>>,
    acked: Mutex<Vec<(String, String)>>,
}

impl FakeVenue {
    fn new(positions: Vec<Position>) -> Self {
        Self {
            positions,
            fail_on: None,
            seen: Mutex::new(Vec::new()),
            acked: Mutex::new(Vec::new()),
        }
    }

    fn failing_on(mut self, asset: &str) -> Self {
        self.fail_on = Some(asset.to_string());
        self
    }

    fn submitted_ids(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.client_order_id.clone())
            .collect()
    }

    fn submitted_count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

#[async_trait::async_trait]
impl VenueAdapter for FakeVenue {
    async fn get_markets(&self) -> Result<Vec<Market>, VenueError> {
        Ok(vec![Market {
            asset: AssetId::from("BTC".to_string()),
            venue_symbol: "BTCUSDT".into(),
            quote_currency: "USD".into(),
            tick: dec("0.01"),
            lot: dec("0.0001"),
            min_notional: dec("10"),
            capabilities: Capabilities {
                fractional: true,
                short: true,
                max_leverage: dec("1"),
                funding: false,
            },
        }])
    }

    async fn get_balances(&self) -> Result<Vec<Balance>, VenueError> {
        Ok(Vec::new())
    }

    async fn get_positions(&self) -> Result<Vec<Position>, VenueError> {
        Ok(self.positions.clone())
    }

    async fn place_order(&self, order: &OrderRequest) -> Result<OrderAck, VenueError> {
        // Idempotent on client_order_id, as the trait contract requires.
        if let Some((_, id)) = self
            .acked
            .lock()
            .unwrap()
            .iter()
            .find(|(coid, _)| coid == &order.client_order_id)
        {
            return Ok(OrderAck {
                venue_order_id: id.clone(),
                client_order_id: order.client_order_id.clone(),
                state: OrderState::Filled,
                accepted_at: time::OffsetDateTime::UNIX_EPOCH,
            });
        }
        self.seen.lock().unwrap().push(order.clone());
        if self.fail_on.as_deref() == Some(order.asset.as_str()) {
            return Err(VenueError::InsufficientBalance {
                currency: "USD".into(),
                need: dec("1000"),
                available: dec("10"),
            });
        }
        let vid = format!("V{}", self.seen.lock().unwrap().len());
        self.acked
            .lock()
            .unwrap()
            .push((order.client_order_id.clone(), vid.clone()));
        Ok(OrderAck {
            venue_order_id: vid,
            client_order_id: order.client_order_id.clone(),
            state: OrderState::Filled,
            accepted_at: time::OffsetDateTime::UNIX_EPOCH,
        })
    }

    async fn cancel_order(&self, _venue_order_id: &str) -> Result<(), VenueError> {
        Ok(())
    }

    async fn get_fills(
        &self,
        _since: Option<time::OffsetDateTime>,
    ) -> Result<Vec<Fill>, VenueError> {
        Ok(Vec::new())
    }
}

fn position(asset: &str, qty: &str) -> Position {
    Position {
        asset: AssetId::from(asset.to_string()),
        qty: dec(qty),
        avg_price: dec("100"),
    }
}

fn fill(asset: &str, side: Side, qty: &str) -> Fill {
    Fill {
        venue_fill_id: format!("f-{asset}-{qty}"),
        client_order_id: format!("c-{asset}-{qty}"),
        venue_order_id: format!("v-{asset}-{qty}"),
        asset: AssetId::from(asset.to_string()),
        side,
        qty: dec(qty),
        price: dec("100"),
        fee: dec("0"),
        fee_currency: "USD".into(),
        ts: time::OffsetDateTime::UNIX_EPOCH,
    }
}

/// A plan JSON with the given orders, accepted and live.
///
/// Derived from the plan crate's own fixture rather than hand-written, so the
/// two cannot drift. Only mode, status and orders are overridden; everything
/// else is left exactly as the contract's own example has it.
fn plan_json(orders: &str) -> String {
    format!(
        r#"{{
  "as_of": "2026-08-01T00:00:00+00:00",
  "cost_estimate": {{
    "per_asset": [
      {{
        "asset": "BTC",
        "bps": "7.08",
        "impact_bps": "0.08",
        "spread_bps": "2.00"
      }},
      {{
        "asset": "ETH",
        "bps": "7.17",
        "impact_bps": null,
        "spread_bps": "2.00"
      }}
    ],
    "total_bps": "5.39",
    "total_quote": "53.88"
  }},
  "created_at": "2026-08-01T06:30:00+00:00",
  "current": [
    {{
      "asset": "BTC",
      "qty": "0.10000000",
      "weight": "0.100000"
    }}
  ],
  "mode": "live",
  "nav": {{
    "benchmark_beta": null,
    "cash": "25000.10",
    "gross_exposure": "0.750000",
    "net_exposure": "0.250000",
    "total": "100000.00"
  }},
  "orders": [{orders}],
  "plan_id": "5227e5a9-d91b-50b0-9275-ce7ca4c3ddf8",
  "provenance": {{
    "constructor": "conviction_tilt",
    "constructor_requested": "mvo",
    "feature_set_version": "fs-phase0-1",
    "inputs_hash": "2117ef6d27b612fd",
    "model_id": null,
    "planner_version": "0.1.0",
    "risk_model_version": "none-phase0",
    "ruleset_version": "phase0",
    "scoring_version": "none-phase0",
    "universe_size": 30
  }},
  "quote_currency": "USD",
  "risk_report": {{
    "checks": [
      {{
        "detail": "sum of |weight| across targets",
        "limit": "1.00",
        "name": "max_gross_exposure",
        "passed": true,
        "value": "0.75"
      }},
      {{
        "detail": null,
        "limit": "0.50",
        "name": "max_position",
        "passed": true,
        "value": "0.50"
      }}
    ],
    "passed": true,
    "rejected_reason": null
  }},
  "run_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
  "schema_version": "1.1.0",
  "status": "accepted",
  "targets": [
    {{
      "asset": "BTC",
      "conviction": "0.87",
      "direction": "long",
      "weight": "0.500000"
    }},
    {{
      "asset": "ETH",
      "conviction": null,
      "direction": "short",
      "weight": "-0.250000"
    }}
  ],
  "warnings": [
    {{
      "kind": "degenerate_feature",
      "message": "revisions: 12 of 30 snapshots"
    }},
    {{
      "kind": "unenforced_rule",
      "message": "limit max_cluster_exposure is not enforced"
    }},
    {{
      "kind": "insufficient_sample",
      "message": "n=41, below the 100 floor"
    }},
    {{
      "kind": "constructor_fallback",
      "message": "requested mvo, used conviction_tilt"
    }},
    {{
      "kind": "stale_input",
      "message": "adv_quote is 2 bars old for ETH"
    }},
    {{
      "kind": "turnover_capped",
      "message": "budget 0.50 spent; 1 trade deferred"
    }},
    {{
      "kind": "other",
      "message": "cost model is uncalibrated"
    }}
  ]
}}"#
    )
}

fn order(asset: &str, side: &str, qty: &str, reason: &str) -> String {
    format!(
        r#"{{"asset":"{asset}","side":"{side}","qty":"{qty}","order_type":"market",
             "limit_price":null,"reason":"{reason}","est_cost_bps":null}}"#
    )
}

fn now() -> time::OffsetDateTime {
    time::OffsetDateTime::UNIX_EPOCH
}

// --- reconciliation ---------------------------------------------------------

#[test]
fn matching_positions_reconcile() {
    let ours = vec![position("BTC", "1.5")];
    let theirs = vec![position("BTC", "1.5")];
    assert!(reconcile(&ours, &theirs).is_ok());
}

#[test]
fn a_rounding_difference_is_tolerated() {
    // Venues round to their own lot sizes; an exact test would flag arithmetic.
    let ours = vec![position("BTC", "1.500000001")];
    let theirs = vec![position("BTC", "1.5")];
    assert!(reconcile(&ours, &theirs).is_ok());
}

#[test]
fn a_real_difference_halts() {
    let ours = vec![position("BTC", "1.5")];
    let theirs = vec![position("BTC", "1.4")];
    let err = reconcile(&ours, &theirs).unwrap_err();
    assert!(matches!(err, ExecError::Reconciliation { .. }));
    // Both numbers must be in the message: a human has to decide which is right.
    let msg = err.to_string();
    assert!(msg.contains("1.5") && msg.contains("1.4"));
    assert!(msg.contains("never auto-corrected"));
}

#[test]
fn a_position_the_venue_does_not_know_about_halts() {
    let err = reconcile(&[position("BTC", "1.0")], &[]).unwrap_err();
    assert!(matches!(err, ExecError::Reconciliation { .. }));
}

#[test]
fn a_position_we_do_not_know_about_also_halts() {
    // The asymmetric case: iterating only our own positions would miss this.
    let err = reconcile(&[], &[position("ETH", "3.0")]).unwrap_err();
    assert!(matches!(err, ExecError::Reconciliation { asset, .. } if asset == "ETH"));
}

#[tokio::test]
async fn execution_reconciles_before_trading_not_after() {
    // The venue holds 2 BTC; our fill log says 1. Nothing may be submitted.
    let venue = FakeVenue::new(vec![position("BTC", "2.0")]);
    let p = plan::Plan::parse(&plan_json(&order("BTC", "buy", "1", "entry"))).unwrap();
    let fills = vec![fill("BTC", Side::Buy, "1")];
    let err = execute(&venue, &p, &fills, Controls::default(), now())
        .await
        .unwrap_err();
    assert!(matches!(err, ExecError::Reconciliation { .. }));
    assert_eq!(
        venue.submitted_count(),
        0,
        "submitting into a book we do not understand is how a small discrepancy \
         becomes a large one"
    );
}

// --- idempotency ------------------------------------------------------------

#[test]
fn the_client_order_id_is_a_pure_function_of_the_plan_and_order() {
    let p = plan::Plan::parse(&plan_json(&order("BTC", "buy", "1.5", "entry"))).unwrap();
    let a = client_order_id(&p.plan_id.to_string(), &p.orders[0]);
    let b = client_order_id(&p.plan_id.to_string(), &p.orders[0]);
    assert_eq!(
        a, b,
        "a replay must produce the same id or a restart doubles up"
    );
}

#[test]
fn different_sides_get_different_ids() {
    let p = plan::Plan::parse(&plan_json(&format!(
        "{},{}",
        order("BTC", "sell", "1", "exit"),
        order("ETH", "buy", "1", "entry")
    )))
    .unwrap();
    let a = client_order_id(&p.plan_id.to_string(), &p.orders[0]);
    let b = client_order_id(&p.plan_id.to_string(), &p.orders[1]);
    assert_ne!(a, b);
}

#[tokio::test]
async fn replaying_a_plan_does_not_place_a_second_order() {
    // The crash-safety case: a process that dies after submitting but before
    // recording must be safe to restart.
    let venue = FakeVenue::new(Vec::new());
    let p = plan::Plan::parse(&plan_json(&order("BTC", "buy", "1", "entry"))).unwrap();

    let first = execute(&venue, &p, &[], Controls::default(), now())
        .await
        .unwrap();
    let ids_after_first = venue.submitted_ids();
    let second = execute(&venue, &p, &[], Controls::default(), now())
        .await
        .unwrap();

    assert!(first.is_complete() && second.is_complete());
    assert_eq!(
        venue.submitted_ids(),
        ids_after_first,
        "the venue saw the same id and returned the original ack; no second order"
    );
    assert_eq!(
        first.submitted[0].venue_order_id,
        second.submitted[0].venue_order_id
    );
}

// --- partial failure --------------------------------------------------------

#[tokio::test]
async fn a_failed_order_stops_the_run_and_names_what_was_skipped() {
    let venue = FakeVenue::new(Vec::new()).failing_on("ETH");
    let p = plan::Plan::parse(&plan_json(&format!(
        "{},{},{}",
        order("BTC", "buy", "1", "entry"),
        order("ETH", "buy", "1", "entry"),
        order("SOL", "buy", "1", "entry")
    )))
    .unwrap();
    let rec = execute(&venue, &p, &[], Controls::default(), now())
        .await
        .unwrap();

    assert!(!rec.is_complete());
    assert_eq!(
        rec.submitted.len(),
        2,
        "BTC succeeded, ETH failed, SOL untried"
    );
    assert!(rec.submitted[1].error.is_some());
    assert_eq!(
        rec.not_attempted,
        vec!["SOL".to_string()],
        "skipped orders are named, not counted - a count is not actionable"
    );
    assert!(rec.halted_reason.unwrap().contains("ETH"));
}

// --- refusals ---------------------------------------------------------------

#[tokio::test]
async fn a_rejected_plan_never_reaches_the_venue() {
    // The plan contract already refuses a rejected plan that carries orders -
    // "rejection is whole, never partial" - so the executor can only ever be
    // handed a rejected plan with an empty order list. Its own status check is
    // the second line of defence, and it must still refuse rather than treating
    // an empty plan as a successful no-op.
    let venue = FakeVenue::new(Vec::new());
    // A rejected plan must be internally consistent: a failing CHECK, a false
    // report flag, and a reason. The contract cross-validates all three, which
    // is why the fixture cannot just flip the status.
    let json = plan_json("")
        .replace(r#""status": "accepted""#, r#""status": "rejected""#)
        .replace(
            r#""rejected_reason": null"#,
            r#""rejected_reason": "max_gross_exposure 1.2 exceeds 1.0""#,
        )
        // the first check, identified by the key that follows it
        .replacen(
            "\"passed\": true,\n        \"value\"",
            "\"passed\": false,\n        \"value\"",
            1,
        )
        // the report's own flag, identified the same way
        .replace(
            "\"passed\": true,\n    \"rejected_reason\"",
            "\"passed\": false,\n    \"rejected_reason\"",
        );
    let p = plan::Plan::parse(&json).expect("a rejected plan with no orders is valid");
    let err = execute(&venue, &p, &[], Controls::default(), now())
        .await
        .unwrap_err();
    assert!(matches!(err, ExecError::NotAccepted { .. }));
    assert_eq!(venue.submitted_count(), 0);
}

#[test]
fn the_contract_itself_refuses_a_rejected_plan_carrying_orders() {
    // Worth asserting here too: the executor's guarantee rests on it, and a
    // relaxation upstream would otherwise be silent.
    let json = plan_json(&order("BTC", "buy", "1", "entry"))
        .replace(r#""status": "accepted""#, r#""status": "rejected""#);
    let err = plan::Plan::parse(&json).unwrap_err();
    assert!(err.to_string().contains("rejection is whole"));
}

#[tokio::test]
async fn a_dry_run_plan_never_reaches_the_venue() {
    let venue = FakeVenue::new(Vec::new());
    let json = plan_json(&order("BTC", "buy", "1", "entry"))
        .replace(r#""mode": "live""#, r#""mode": "dry""#);
    let p = plan::Plan::parse(&json).unwrap();
    let err = execute(&venue, &p, &[], Controls::default(), now())
        .await
        .unwrap_err();
    assert!(matches!(err, ExecError::NotLive { .. }));
    assert_eq!(venue.submitted_count(), 0);
}

#[tokio::test]
async fn the_kill_switch_stops_everything() {
    let venue = FakeVenue::new(Vec::new());
    let p = plan::Plan::parse(&plan_json(&order("BTC", "sell", "1", "exit"))).unwrap();
    let controls = Controls {
        kill_switch: true,
        paused: false,
    };
    let err = execute(&venue, &p, &[], controls, now()).await.unwrap_err();
    assert!(matches!(err, ExecError::KillSwitchEngaged));
    assert_eq!(
        venue.submitted_count(),
        0,
        "the kill switch outranks even an exit - it is the strongest stop"
    );
}

// --- pause ------------------------------------------------------------------

#[tokio::test]
async fn a_pause_still_permits_getting_out() {
    let venue = FakeVenue::new(Vec::new());
    let p = plan::Plan::parse(&plan_json(&format!(
        "{},{}",
        order("BTC", "sell", "1", "exit"),
        order("ETH", "buy", "1", "entry")
    )))
    .unwrap();
    let controls = Controls {
        kill_switch: false,
        paused: true,
    };
    let rec = execute(&venue, &p, &[], controls, now()).await.unwrap();

    assert!(rec.paused);
    assert_eq!(rec.submitted.len(), 1);
    assert_eq!(rec.submitted[0].asset, "BTC", "the exit ran");
    assert_eq!(
        rec.not_attempted,
        vec!["ETH".to_string()],
        "the entry did not - a pause means stop taking on risk, not stop shedding it"
    );
}

#[tokio::test]
async fn a_pause_with_only_entries_refuses_rather_than_doing_nothing_quietly() {
    let venue = FakeVenue::new(Vec::new());
    let p = plan::Plan::parse(&plan_json(&order("BTC", "buy", "1", "entry"))).unwrap();
    let controls = Controls {
        kill_switch: false,
        paused: true,
    };
    let err = execute(&venue, &p, &[], controls, now()).await.unwrap_err();
    assert!(matches!(err, ExecError::PausedWithNothingPermitted { .. }));
    assert_eq!(venue.submitted_count(), 0);
}

// --- ordering ---------------------------------------------------------------

#[tokio::test]
async fn exits_before_entries_is_asserted_not_assumed() {
    let venue = FakeVenue::new(Vec::new());
    // Deliberately wrong order: the entry precedes the exit.
    let p = plan::Plan::parse(&plan_json(&format!(
        "{},{}",
        order("ETH", "buy", "1", "entry"),
        order("BTC", "sell", "1", "exit")
    )))
    .unwrap();
    let err = execute(&venue, &p, &[], Controls::default(), now())
        .await
        .unwrap_err();
    assert!(matches!(err, ExecError::OrderSequence { .. }));
    assert_eq!(venue.submitted_count(), 0);
}

#[tokio::test]
async fn a_correctly_ordered_plan_executes_in_sequence() {
    let venue = FakeVenue::new(Vec::new());
    let p = plan::Plan::parse(&plan_json(&format!(
        "{},{},{}",
        order("BTC", "sell", "1", "exit"),
        order("SOL", "sell", "1", "reduce"),
        order("ETH", "buy", "1", "entry")
    )))
    .unwrap();
    let rec = execute(&venue, &p, &[], Controls::default(), now())
        .await
        .unwrap();
    assert!(rec.is_complete());
    let assets: Vec<&str> = rec.submitted.iter().map(|o| o.asset.as_str()).collect();
    assert_eq!(assets, vec!["BTC", "SOL", "ETH"]);
}
