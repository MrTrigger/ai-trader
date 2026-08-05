//! The runner's operational guarantees.
//!
//! Everything here exists because of a failure that is invisible from the
//! outside. A control file that is read once looks identical to one that is read
//! every slice, until the day someone throws the kill switch mid-window. Two
//! equal slices of one order look correct in every log, until you notice the
//! venue deduplicated them and the book holds half of target. A runner that
//! never runs looks exactly like a runner with nothing to do.
//!
//! So the fake venue here derives its positions from its own fills, which means
//! reconciliation between slices is a live property rather than a formality: a
//! runner that reused stale fills would fail these tests rather than pass them
//! quietly.

use std::path::PathBuf;
use std::sync::Mutex;

use runner::{
    check_freshness, fills_for_plan, health, slice_plan, ControlFile, RunRecord, RunStore, Runner,
    RunnerError, Schedule, Timer,
};
use rust_decimal::Decimal;
use time::OffsetDateTime;
use venue::{
    derive_positions, AssetId, Balance, Capabilities, Fill, Market, OrderAck, OrderRequest,
    OrderState, Position, Side, VenueAdapter, VenueError,
};

fn dec(s: &str) -> Decimal {
    s.parse().expect("literal decimal")
}

fn t(s: &str) -> OffsetDateTime {
    OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).expect("literal time")
}

// --- the plan under test ----------------------------------------------------

/// The plan crate's own fixture, with orders and timestamps overridden.
///
/// Read from that file rather than restated here, so the two cannot drift: a
/// contract change that this crate has not absorbed should break the build, not
/// pass against a stale copy.
const FIXTURE: &str = include_str!("../../plan/tests/fixtures/plan.json");

fn plan_with(orders: serde_json::Value, as_of: &str) -> plan::Plan {
    let mut v: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture parses");
    v["orders"] = orders;
    v["as_of"] = serde_json::json!(as_of);
    v["created_at"] = serde_json::json!(as_of);
    plan::Plan::parse(&v.to_string()).expect("fixture stays valid")
}

fn order(asset: &str, side: &str, qty: &str, reason: &str) -> serde_json::Value {
    serde_json::json!({
        "asset": asset, "side": side, "qty": qty, "order_type": "market",
        "limit_price": null, "reason": reason, "est_cost_bps": null,
    })
}

/// Exits first, as the plan contract requires and the executor asserts.
fn standard_plan(as_of: &str) -> plan::Plan {
    plan_with(
        serde_json::json!([
            order("SOL", "sell", "10.0", "exit"),
            order("BTC", "buy", "0.4", "entry"),
        ]),
        as_of,
    )
}

// --- fakes ------------------------------------------------------------------

/// A venue whose positions are the fold of its own fills.
///
/// This is what makes the between-slices reconciliation test real: after slice
/// one fills, the venue genuinely holds a position, so a runner that reconciled
/// against an empty fill log would halt.
struct FakeVenue {
    fills: Mutex<Vec<Fill>>,
    seen: Mutex<Vec<OrderRequest>>,
    fail_on: Option<String>,
}

impl FakeVenue {
    fn new() -> Self {
        Self {
            fills: Mutex::new(Vec::new()),
            seen: Mutex::new(Vec::new()),
            fail_on: None,
        }
    }

    fn failing_on(mut self, asset: &str) -> Self {
        self.fail_on = Some(asset.into());
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

    fn filled_qty(&self, asset: &str) -> Decimal {
        self.fills
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f.asset.as_str() == asset)
            .map(|f| f.qty)
            .sum()
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
        Ok(derive_positions(&self.fills.lock().unwrap()))
    }

    async fn place_order(&self, o: &OrderRequest) -> Result<OrderAck, VenueError> {
        // Idempotent on client_order_id, exactly as the trait contract says. A
        // fake that ignored this would hide the slice-id collision entirely.
        if let Some(f) = self
            .fills
            .lock()
            .unwrap()
            .iter()
            .find(|f| f.client_order_id == o.client_order_id)
        {
            return Ok(OrderAck {
                venue_order_id: f.venue_order_id.clone(),
                client_order_id: o.client_order_id.clone(),
                state: OrderState::Filled,
                accepted_at: OffsetDateTime::UNIX_EPOCH,
            });
        }
        self.seen.lock().unwrap().push(o.clone());
        if self.fail_on.as_deref() == Some(o.asset.as_str()) {
            return Err(VenueError::InsufficientBalance {
                currency: "USD".into(),
                need: dec("1000"),
                available: dec("10"),
            });
        }
        let n = self.fills.lock().unwrap().len() + 1;
        let vid = format!("V{n}");
        self.fills.lock().unwrap().push(Fill {
            venue_fill_id: format!("F{n}"),
            venue_order_id: vid.clone(),
            client_order_id: o.client_order_id.clone(),
            asset: o.asset.clone(),
            side: o.side,
            qty: o.qty,
            price: dec("100"),
            fee: dec("0"),
            fee_currency: "USD".into(),
            ts: OffsetDateTime::UNIX_EPOCH,
        });
        Ok(OrderAck {
            venue_order_id: vid,
            client_order_id: o.client_order_id.clone(),
            state: OrderState::Filled,
            accepted_at: OffsetDateTime::UNIX_EPOCH,
        })
    }

    async fn cancel_order(&self, _venue_order_id: &str) -> Result<(), VenueError> {
        Ok(())
    }

    async fn get_fills(&self, _since: Option<OffsetDateTime>) -> Result<Vec<Fill>, VenueError> {
        Ok(self.fills.lock().unwrap().clone())
    }
}

/// A clock that jumps rather than waits, and can run a hook at each jump.
///
/// The hook is how an operator intervening mid-window is simulated: the test
/// writes the kill switch at the moment the runner is "waiting" for slice two.
struct TestClock {
    now: Mutex<OffsetDateTime>,
    #[allow(clippy::type_complexity)]
    on_sleep: Option<Box<dyn Fn(OffsetDateTime) + Send + Sync>>,
}

impl TestClock {
    fn at(s: &str) -> Self {
        Self {
            now: Mutex::new(t(s)),
            on_sleep: None,
        }
    }

    fn with_hook(mut self, f: impl Fn(OffsetDateTime) + Send + Sync + 'static) -> Self {
        self.on_sleep = Some(Box::new(f));
        self
    }
}

impl venue::Clock for TestClock {
    fn now(&self) -> OffsetDateTime {
        *self.now.lock().unwrap()
    }
}

#[async_trait::async_trait]
impl Timer for TestClock {
    async fn sleep_until(&self, until: OffsetDateTime) {
        if let Some(f) = &self.on_sleep {
            f(until);
        }
        let mut now = self.now.lock().unwrap();
        if until > *now {
            *now = until;
        }
    }
}

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("runner-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

fn running_controls(dir: &std::path::Path) -> PathBuf {
    let p = dir.join("controls.json");
    ControlFile::running("test", "tests", OffsetDateTime::UNIX_EPOCH)
        .write(&p)
        .expect("write controls");
    p
}

// --- controls fail closed ---------------------------------------------------

#[test]
fn an_absent_control_file_means_halted_not_running() {
    let c = ControlFile::read(&PathBuf::from("/nonexistent/controls.json"));
    assert!(
        c.kill_switch,
        "a missing control file must not enable trading"
    );
    assert_eq!(c.state(), "halted");
    assert!(
        c.reason.is_some(),
        "a halt without a reason is unactionable"
    );
}

#[test]
fn an_unparseable_control_file_means_halted_not_running() {
    let dir = tmpdir("bad-controls");
    let p = dir.join("controls.json");
    std::fs::write(&p, "{ this is not json").unwrap();
    let c = ControlFile::read(&p);
    assert!(c.kill_switch);
    assert!(c.reason.unwrap().contains("unreadable"));
}

#[test]
fn a_written_control_file_round_trips() {
    let dir = tmpdir("roundtrip");
    let p = dir.join("controls.json");
    let c = ControlFile::running("go live", "magnus", OffsetDateTime::UNIX_EPOCH);
    c.write(&p).unwrap();
    assert_eq!(ControlFile::read(&p), c);
    assert_eq!(c.state(), "running");
}

// --- slicing ----------------------------------------------------------------

#[test]
fn slices_sum_to_the_original_quantity() {
    // 0.4 over 3 does not divide evenly; the residue must not vanish.
    let plan = plan_with(
        serde_json::json!([order("BTC", "buy", "0.4", "entry")]),
        "2026-08-01T00:00:00Z",
    );
    let slices = slice_plan(
        &plan,
        Schedule {
            lag_hours: 1,
            slices: 3,
        },
    );
    let total: Decimal = slices
        .iter()
        .map(|s| s.plan.orders.first().map(|o| o.qty).unwrap_or_default())
        .sum();
    assert_eq!(total, dec("0.4"));
}

#[test]
fn the_rounding_residue_lands_on_the_last_slice() {
    let plan = plan_with(
        serde_json::json!([order("BTC", "buy", "1.0", "entry")]),
        "2026-08-01T00:00:00Z",
    );
    let slices = slice_plan(
        &plan,
        Schedule {
            lag_hours: 1,
            slices: 3,
        },
    );
    let q: Vec<Decimal> = slices.iter().map(|s| s.plan.orders[0].qty).collect();
    assert_eq!(q[0], q[1], "the leading slices are equal");
    assert!(
        q[2] > q[1],
        "the residue rides the last slice, where it cannot move the price the bulk pays"
    );
}

#[test]
fn slices_are_spaced_an_hour_apart_after_the_lag() {
    let plan = standard_plan("2026-08-01T00:00:00Z");
    let slices = slice_plan(
        &plan,
        Schedule {
            lag_hours: 1,
            slices: 2,
        },
    );
    assert_eq!(slices[0].send_at, t("2026-08-01T01:00:00Z"));
    assert_eq!(slices[1].send_at, t("2026-08-01T02:00:00Z"));
}

#[test]
fn slicing_preserves_exits_before_entries() {
    let plan = standard_plan("2026-08-01T00:00:00Z");
    for s in slice_plan(&plan, Schedule::default()) {
        let assets: Vec<&str> = s.plan.orders.iter().map(|o| o.asset.as_str()).collect();
        assert_eq!(assets, vec!["SOL", "BTC"]);
    }
}

#[test]
fn a_quantity_that_rounds_away_is_not_submitted() {
    // Eight decimal places over eight slices: each slice rounds to zero.
    let plan = plan_with(
        serde_json::json!([order("BTC", "buy", "0.00000001", "entry")]),
        "2026-08-01T00:00:00Z",
    );
    let slices = slice_plan(
        &plan,
        Schedule {
            lag_hours: 1,
            slices: 8,
        },
    );
    assert!(slices[0].plan.orders.is_empty(), "dust is not an order");
    assert_eq!(
        slices[7].plan.orders[0].qty,
        dec("0.00000001"),
        "and the whole quantity still goes out, on the last slice"
    );
}

#[test]
fn the_window_widens_with_the_account() {
    assert_eq!(Schedule::for_nav(dec("30000")).slices, 2);
    assert_eq!(Schedule::for_nav(dec("300000")).slices, 4);
    assert_eq!(Schedule::for_nav(dec("3000000")).slices, 8);
}

// --- the run ----------------------------------------------------------------

fn runner<'a>(
    venue: &'a FakeVenue,
    clock: &'a TestClock,
    store: &'a RunStore,
    controls: PathBuf,
    schedule: Schedule,
) -> Runner<'a, FakeVenue, TestClock> {
    Runner {
        venue,
        clock,
        store,
        controls_path: controls,
        schedule,
        max_plan_age_minutes: 120,
    }
}

#[tokio::test]
async fn every_slice_reaches_the_venue_with_its_own_id() {
    let dir = tmpdir("slices-submit");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new();
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let plan = standard_plan("2026-08-01T00:00:00Z");

    let rec = runner(&venue, &clock, &store, controls, Schedule::default())
        .run(&plan)
        .await
        .expect("the run completes");

    assert_eq!(rec.outcome, "executed");
    assert_eq!(rec.slices_completed, 2);
    // Two orders, two slices, four distinct submissions.
    let ids = venue.submitted_ids();
    assert_eq!(ids.len(), 4, "got {ids:?}");
    let unique: std::collections::HashSet<&String> = ids.iter().collect();
    assert_eq!(
        unique.len(),
        4,
        "equal slices must not share an id, or the venue's idempotency rule \
         silently halves the book: {ids:?}"
    );
    // And the full quantity actually arrived.
    assert_eq!(venue.filled_qty("BTC"), dec("0.4"));
    assert_eq!(venue.filled_qty("SOL"), dec("10.0"));
}

#[tokio::test]
async fn reconciliation_between_slices_uses_the_fills_slice_one_produced() {
    // The venue's positions move after slice one. If the runner reconciled
    // against a stale fill log, slice two would halt on drift.
    let dir = tmpdir("reconcile-between");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new();
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let plan = standard_plan("2026-08-01T00:00:00Z");

    let rec = runner(&venue, &clock, &store, controls, Schedule::default())
        .run(&plan)
        .await
        .expect("slice two must not see phantom drift");
    assert_eq!(rec.outcome, "executed");
    assert!(!venue.get_positions().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_kill_switch_thrown_mid_window_stops_the_remaining_slices() {
    let dir = tmpdir("kill-mid-window");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new();
    let store = RunStore::new(&dir);
    let plan = standard_plan("2026-08-01T00:00:00Z");

    // The operator throws the switch while the runner waits for slice two.
    let target = t("2026-08-01T02:00:00Z");
    let cpath = controls.clone();
    let clock = TestClock::at("2026-08-01T00:30:00Z").with_hook(move |until| {
        if until >= target {
            ControlFile {
                kill_switch: true,
                paused: false,
                reason: Some("operator stopped it".into()),
                set_by: Some("magnus".into()),
                set_at: None,
            }
            .write(&cpath)
            .unwrap();
        }
    });

    let rec = runner(
        &venue,
        &clock,
        &store,
        controls,
        Schedule {
            lag_hours: 1,
            slices: 2,
        },
    )
    .run(&plan)
    .await
    .expect("a mid-window stop is a recorded outcome, not an error");

    assert_eq!(rec.outcome, "halted");
    assert_eq!(rec.slices_completed, 1);
    assert_eq!(venue.submitted_ids().len(), 2, "only slice one went out");
    let detail = rec.detail.expect("a halt states why");
    assert!(detail.contains("operator stopped it"));
    assert!(
        detail.contains("part-way to target"),
        "the operator must be told the book is between states: {detail}"
    );
}

#[tokio::test]
async fn a_kill_switch_set_before_the_run_submits_nothing_and_is_still_recorded() {
    let dir = tmpdir("kill-before");
    let controls = dir.join("controls.json"); // never written: default is halted
    let venue = FakeVenue::new();
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let plan = standard_plan("2026-08-01T00:00:00Z");

    let err = runner(&venue, &clock, &store, controls, Schedule::default())
        .run(&plan)
        .await
        .expect_err("a halted runner does not execute");
    assert!(matches!(err, RunnerError::Execution(_)));
    assert!(venue.submitted_ids().is_empty());

    let recorded = store.recent(1).unwrap();
    assert_eq!(recorded[0].outcome, "refused");
    assert_eq!(recorded[0].orders_submitted, 0);
}

#[tokio::test]
async fn a_stale_plan_is_refused_and_the_refusal_is_recorded() {
    let dir = tmpdir("stale");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new();
    // Six hours after the plan's as_of, well past the two-hour limit.
    let clock = TestClock::at("2026-08-01T06:00:00Z");
    let store = RunStore::new(&dir);
    let plan = standard_plan("2026-08-01T00:00:00Z");

    let err = runner(&venue, &clock, &store, controls, Schedule::default())
        .run(&plan)
        .await
        .expect_err("a stale plan is refused");
    assert!(matches!(err, RunnerError::PlanTooOld { .. }));
    assert!(venue.submitted_ids().is_empty());
    assert_eq!(store.recent(1).unwrap()[0].outcome, "refused");
}

#[tokio::test]
async fn the_same_plan_is_never_executed_twice() {
    let dir = tmpdir("no-replay");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new();
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let plan = standard_plan("2026-08-01T00:00:00Z");

    runner(
        &venue,
        &clock,
        &store,
        controls.clone(),
        Schedule::default(),
    )
    .run(&plan)
    .await
    .unwrap();
    let submitted = venue.submitted_ids().len();

    let err = runner(&venue, &clock, &store, controls, Schedule::default())
        .run(&plan)
        .await
        .expect_err("a plan already executed must not run again");
    assert!(matches!(err, RunnerError::AlreadyExecuted { .. }));
    assert_eq!(
        venue.submitted_ids().len(),
        submitted,
        "nothing further reached the venue"
    );
}

#[tokio::test]
async fn a_failed_order_stops_the_run_and_names_the_slice() {
    let dir = tmpdir("order-fails");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new().failing_on("BTC");
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let plan = standard_plan("2026-08-01T00:00:00Z");

    let rec = runner(&venue, &clock, &store, controls, Schedule::default())
        .run(&plan)
        .await
        .unwrap();

    assert_eq!(rec.outcome, "partial");
    assert_eq!(
        rec.slices_completed, 1,
        "the second slice was not attempted"
    );
    let detail = rec.detail.expect("a partial run states what happened");
    assert!(detail.contains("BTC"), "got {detail}");
}

#[tokio::test]
async fn the_stored_record_keeps_each_slice_separately() {
    let dir = tmpdir("per-slice-record");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new();
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let plan = standard_plan("2026-08-01T00:00:00Z");

    runner(&venue, &clock, &store, controls, Schedule::default())
        .run(&plan)
        .await
        .unwrap();

    let back = store.recent(10).unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].slices.len(), 2);
    assert_eq!(back[0].slices[0]["index"], 1);
    assert_eq!(back[0].slices[1]["index"], 2);
    assert!(back[0].slices[0]["execution"]["reconciled"]
        .as_bool()
        .unwrap());
}

// --- flatten ----------------------------------------------------------------

#[tokio::test]
async fn flatten_closes_every_open_position_in_the_opposite_direction() {
    let dir = tmpdir("flatten");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new();
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);

    // Put a book on: one long, one short.
    runner(
        &venue,
        &clock,
        &store,
        controls.clone(),
        Schedule::default(),
    )
    .run(&standard_plan("2026-08-01T00:00:00Z"))
    .await
    .unwrap();
    assert_eq!(venue.filled_qty("BTC"), dec("0.4"));

    let rec = runner(&venue, &clock, &store, controls, Schedule::default())
        .flatten("drawdown breach", "magnus")
        .await
        .unwrap();

    assert_eq!(rec.outcome, "flattened");
    assert_eq!(rec.orders_planned, 2, "one order per open position");
    assert_eq!(rec.orders_submitted, 2);
    let open: Vec<_> = venue
        .get_positions()
        .await
        .unwrap()
        .into_iter()
        .filter(|p| !p.qty.is_zero())
        .collect();
    assert!(open.is_empty(), "still open: {open:?}");
}

#[tokio::test]
async fn flatten_works_while_the_kill_switch_is_engaged() {
    // The moment you most want to stop trading is usually the moment you most
    // want to be flat. A kill switch that also blocks the exit is a trap.
    let dir = tmpdir("flatten-killed");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new();
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);

    runner(
        &venue,
        &clock,
        &store,
        controls.clone(),
        Schedule::default(),
    )
    .run(&standard_plan("2026-08-01T00:00:00Z"))
    .await
    .unwrap();

    ControlFile {
        kill_switch: true,
        paused: false,
        reason: Some("stopped".into()),
        set_by: Some("magnus".into()),
        set_at: None,
    }
    .write(&controls)
    .unwrap();

    let rec = runner(&venue, &clock, &store, controls, Schedule::default())
        .flatten("get me out", "magnus")
        .await
        .unwrap();
    assert_eq!(rec.orders_submitted, 2);
    assert_eq!(rec.control_state, "halted", "the halt is still recorded");
}

#[tokio::test]
async fn flatten_keeps_going_after_a_failed_exit() {
    // Unlike a plan, a flatten does not stop at the first failure: every
    // remaining position is still exposure.
    let dir = tmpdir("flatten-partial");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new();
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);

    runner(
        &venue,
        &clock,
        &store,
        controls.clone(),
        Schedule::default(),
    )
    .run(&standard_plan("2026-08-01T00:00:00Z"))
    .await
    .unwrap();

    let venue = FakeVenue {
        fills: Mutex::new(venue.get_fills(None).await.unwrap()),
        seen: Mutex::new(Vec::new()),
        fail_on: Some("BTC".into()),
    };
    let rec = runner(&venue, &clock, &store, controls, Schedule::default())
        .flatten("get me out", "magnus")
        .await
        .unwrap();

    assert_eq!(rec.outcome, "partial");
    assert_eq!(rec.orders_submitted, 1, "SOL still got out");
    assert_eq!(rec.orders_skipped, 1);
    assert!(rec.detail.unwrap().contains("still open"));
}

#[tokio::test]
async fn flatten_records_who_asked_and_why() {
    let dir = tmpdir("flatten-attributed");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new();
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);

    runner(&venue, &clock, &store, controls, Schedule::default())
        .flatten("weekend risk", "magnus")
        .await
        .unwrap();

    let back = store.recent(1).unwrap();
    let detail = back[0].detail.clone().unwrap();
    assert!(detail.contains("magnus"), "got {detail}");
    assert!(detail.contains("weekend risk"), "got {detail}");
}

// --- freshness, history, health --------------------------------------------

#[test]
fn a_fresh_plan_passes_the_freshness_gate() {
    let plan = standard_plan("2026-08-01T00:00:00Z");
    assert!(check_freshness(&plan, t("2026-08-01T01:00:00Z"), 120).is_ok());
    assert!(check_freshness(&plan, t("2026-08-01T03:00:00Z"), 120).is_err());
}

#[test]
fn run_history_comes_back_most_recent_first() {
    let dir = tmpdir("history-order");
    let store = RunStore::new(&dir);
    let controls = ControlFile::default();
    for (i, when) in ["2026-08-01T00:00:00Z", "2026-08-02T00:00:00Z"]
        .iter()
        .enumerate()
    {
        let plan = standard_plan("2026-08-01T00:00:00Z");
        let mut r = RunRecord::refused(&plan, format!("run {i}"), &controls, t(when));
        r.run_id = format!("run-{i}");
        store.record(&r).unwrap();
    }
    let back = store.recent(10).unwrap();
    assert_eq!(back.len(), 2);
    assert_eq!(back[0].detail.as_deref(), Some("run 1"));
}

#[test]
fn health_is_not_ok_when_nothing_has_ever_run() {
    let dir = tmpdir("health-empty");
    let store = RunStore::new(&dir);
    let c = ControlFile::running("live", "tests", OffsetDateTime::UNIX_EPOCH);
    let h = health(&c, &store, 24, t("2026-08-01T00:00:00Z")).unwrap();
    assert!(!h.ok);
    assert!(h
        .notes
        .iter()
        .any(|n| n.contains("no run has ever been recorded")));
}

#[test]
fn health_is_not_ok_when_the_last_run_is_older_than_the_cadence() {
    let dir = tmpdir("health-stale");
    let store = RunStore::new(&dir);
    let controls = ControlFile::running("live", "tests", OffsetDateTime::UNIX_EPOCH);
    let plan = standard_plan("2026-08-01T00:00:00Z");
    let mut r = RunRecord::refused(&plan, "ok".into(), &controls, t("2026-08-01T00:00:00Z"));
    r.outcome = "executed".into();
    r.detail = None;
    store.record(&r).unwrap();

    // Two days later, on a daily cadence: a scheduler that has stopped.
    let h = health(&controls, &store, 24, t("2026-08-03T00:00:00Z")).unwrap();
    assert!(!h.ok);
    assert!(h.notes.iter().any(|n| n.contains("scheduler")));
}

#[test]
fn health_is_ok_when_a_clean_run_is_recent_and_controls_permit_trading() {
    let dir = tmpdir("health-ok");
    let store = RunStore::new(&dir);
    let controls = ControlFile::running("live", "tests", OffsetDateTime::UNIX_EPOCH);
    let plan = standard_plan("2026-08-01T00:00:00Z");
    let mut r = RunRecord::refused(&plan, "ok".into(), &controls, t("2026-08-01T00:00:00Z"));
    r.outcome = "executed".into();
    r.detail = None;
    store.record(&r).unwrap();

    let h = health(&controls, &store, 24, t("2026-08-01T06:00:00Z")).unwrap();
    assert!(h.ok, "notes: {:?}", h.notes);
    assert_eq!(h.control_state, "running");
    assert_eq!(h.hours_since_last_run, Some(6));
}

#[test]
fn a_kill_switch_makes_health_report_not_ok_with_the_reason() {
    let dir = tmpdir("health-killed");
    let store = RunStore::new(&dir);
    let c = ControlFile {
        kill_switch: true,
        paused: false,
        reason: Some("drawdown breach".into()),
        set_by: Some("magnus".into()),
        set_at: None,
    };
    let h = health(&c, &store, 24, t("2026-08-01T00:00:00Z")).unwrap();
    assert!(!h.ok);
    assert!(h.notes.iter().any(|n| n.contains("drawdown breach")));
}

#[test]
fn fills_are_attributed_to_their_plan_by_order_id() {
    let mine = Fill {
        venue_fill_id: "1".into(),
        venue_order_id: "1".into(),
        client_order_id: "5227e5a9-BTC-B-0.2".into(),
        asset: AssetId::from("BTC".to_string()),
        side: Side::Buy,
        qty: dec("0.2"),
        price: dec("100"),
        fee: dec("0"),
        fee_currency: "USD".into(),
        ts: OffsetDateTime::UNIX_EPOCH,
    };
    let theirs = Fill {
        client_order_id: "deadbeef-BTC-B-0.2".into(),
        ..mine.clone()
    };
    let all = [mine, theirs];
    let got = fills_for_plan("5227e5a9-d91b-50b0-9275-ce7ca4c3ddf8", &all);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].client_order_id, "5227e5a9-BTC-B-0.2");
}
