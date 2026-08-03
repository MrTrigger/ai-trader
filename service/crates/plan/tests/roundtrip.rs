//! The cross-language contract test.
//!
//! Python writes `tests/fixtures/plan.json`; this parses it and checks the
//! values survived. A failure here is a contract break between the planner and
//! the executor, not a unit-test nit — the two halves of the system disagree
//! about what a plan is, and the correct response is to stop shipping until
//! they don't.
//!
//! The fixture is regenerated from the Python side:
//!
//!     AI_TRADER_UPDATE_FIXTURE=1 pytest tests/test_fixture.py

use plan::{Mode, OrderReason, OrderType, Plan, PlanError, Side, Status, WarningKind};
use rust_decimal::Decimal;
use std::str::FromStr;

const FIXTURE: &str = include_str!("fixtures/plan.json");

fn fixture() -> Plan {
    Plan::parse(FIXTURE).expect("the committed fixture must parse")
}

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

#[test]
fn parses_the_python_fixture() {
    let p = fixture();
    assert_eq!(p.schema_version, "1.1.0");
    assert_eq!(p.mode, Mode::Live);
    assert_eq!(p.status, Status::Accepted);
    assert_eq!(p.quote_currency, "USD");
}

#[test]
fn decimals_survive_exactly() {
    let p = fixture();
    // 25000.10 is exact in base 10 and not in binary. Carried as an f64 this is
    // where the two languages would first disagree.
    assert_eq!(p.nav.cash, dec("25000.10"));
    assert_eq!(p.nav.total, dec("100000.00"));
    // 8dp, to catch a narrower decimal type downstream.
    assert_eq!(p.orders[0].qty, dec("0.39753288"));
    // Trailing zeros are preserved: 0.750000 is not 0.75 to a scale-aware type.
    assert_eq!(p.nav.gross_exposure.scale(), 6);
}

#[test]
fn negative_weights_and_nulls_round_trip() {
    let p = fixture();
    assert_eq!(p.targets[1].weight, dec("-0.250000"));
    assert!(p.targets[1].conviction.is_none());
    assert!(p.nav.benchmark_beta.is_none());
    // A market order carries no limit price, and that is a null, not a zero.
    assert_eq!(p.orders[1].order_type, OrderType::Market);
    assert!(p.orders[1].limit_price.is_none());
}

#[test]
fn limit_orders_carry_a_price() {
    let p = fixture();
    assert_eq!(p.orders[0].order_type, OrderType::Limit);
    assert_eq!(p.orders[0].limit_price, Some(dec("64000.50")));
}

#[test]
fn enums_map_across() {
    let p = fixture();
    assert_eq!(p.orders[0].side, Side::Buy);
    assert_eq!(p.orders[0].reason, OrderReason::Increase);
    assert_eq!(p.orders[1].side, Side::Sell);
    assert_eq!(p.orders[1].reason, OrderReason::Entry);
}

#[test]
fn every_warning_kind_is_representable() {
    // The Python fixture asserts it uses every kind the schema declares. If a
    // kind is added there and not here, this fails to deserialise — which is
    // the whole point of pairing the two tests.
    let p = fixture();
    let kinds: Vec<WarningKind> = p.warnings.iter().map(|w| w.kind).collect();
    for expected in [
        WarningKind::DegenerateFeature,
        WarningKind::UnenforcedRule,
        WarningKind::InsufficientSample,
        WarningKind::ConstructorFallback,
        WarningKind::StaleInput,
        WarningKind::TurnoverCapped,
        WarningKind::Other,
    ] {
        assert!(kinds.contains(&expected), "fixture is missing {expected:?}");
    }
}

#[test]
fn a_recorded_fallback_is_visible() {
    let p = fixture();
    assert!(p.provenance.fell_back());
    assert_eq!(p.provenance.constructor, "conviction_tilt");
    assert_eq!(p.provenance.constructor_requested, "mvo");
}

#[test]
fn timestamps_parse_as_utc() {
    let p = fixture();
    assert_eq!(p.as_of.unix_timestamp(), 1_785_888_000);
    assert!(p.created_at > p.as_of, "created_at is wall clock, as_of is the horizon");
}

// --- failing closed --------------------------------------------------------

#[test]
fn unknown_field_is_refused() {
    // The planner knowing something this executor does not is a reason to stop,
    // not to proceed on the parts we recognise.
    let doc = FIXTURE.replacen('{', r#"{"surprise": 1,"#, 1);
    assert!(matches!(Plan::parse(&doc), Err(PlanError::Malformed(_))));
}

#[test]
fn unknown_major_version_is_refused() {
    let doc = FIXTURE.replace(r#""schema_version": "1.1.0""#, r#""schema_version": "2.0.0""#);
    assert!(matches!(
        Plan::parse(&doc),
        Err(PlanError::UnsupportedVersion { .. })
    ));
}

#[test]
fn a_newer_minor_version_still_parses() {
    // Minor bumps are additive. A field we do not know about would still trip
    // deny_unknown_fields, which is the correct place for that to fail.
    let doc = FIXTURE.replace(r#""schema_version": "1.1.0""#, r#""schema_version": "1.9.0""#);
    assert!(Plan::parse(&doc).is_ok());
}

#[test]
fn a_float_where_a_decimal_belongs_is_refused() {
    let doc = FIXTURE.replace(r#""total": "100000.00""#, r#""total": 100000.0"#);
    assert!(matches!(Plan::parse(&doc), Err(PlanError::Malformed(_))));
}

#[test]
fn a_rejected_plan_carrying_orders_is_refused() {
    let doc = FIXTURE.replace(r#""status": "accepted""#, r#""status": "rejected""#);
    match Plan::parse(&doc) {
        Err(PlanError::Invariant(msg)) => assert!(msg.contains("rejection is whole")),
        other => panic!("expected an invariant violation, got {other:?}"),
    }
}

#[test]
fn a_negative_quantity_is_refused() {
    let doc = FIXTURE.replace(r#""qty": "0.39753288""#, r#""qty": "-0.39753288""#);
    // The schema's positive_decimal pattern rejects the sign before serde sees
    // it as a number, so either error is a correct refusal.
    assert!(Plan::parse(&doc).is_err());
}

// --- executability ---------------------------------------------------------

#[test]
fn a_live_accepted_plan_is_executable() {
    assert!(fixture().is_executable());
}

#[test]
fn a_dry_plan_is_never_executable() {
    // Enforced on the type so no call site can forget.
    let doc = FIXTURE.replace(r#""mode": "live""#, r#""mode": "dry""#);
    let p = Plan::parse(&doc).unwrap();
    assert!(!p.is_executable());
}

#[test]
fn only_risk_reducing_orders_may_run_while_paused() {
    assert!(OrderReason::Exit.permitted_while_paused());
    assert!(OrderReason::Reduce.permitted_while_paused());
    assert!(!OrderReason::Entry.permitted_while_paused());
    assert!(!OrderReason::Increase.permitted_while_paused());
    assert!(!OrderReason::Rebalance.permitted_while_paused());
}
