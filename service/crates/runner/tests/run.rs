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
    check_decision_lag, check_freshness, fills_for_plan, health, slice_plan, ControlFile, Ledger,
    RunRecord, RunStore, Runner, RunnerError, Schedule, Timer,
};
use rust_decimal::Decimal;
use time::OffsetDateTime;
use venue::{
    derive_positions, AssetId, Balance, Capabilities, Fill, Market, OpenOrder, OrderAck,
    OrderRequest, OrderState, Position, Side, VenueAdapter, VenueError,
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

/// A plan whose decision moment and write time differ, which is the normal case
/// the two gates had been unable to tell apart.
fn plan_decided_at_written_at(as_of: &str, created_at: &str) -> plan::Plan {
    let mut v: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture parses");
    v["as_of"] = serde_json::json!(as_of);
    v["created_at"] = serde_json::json!(created_at);
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
    /// Positions the account already held, whose fills predate anything
    /// `get_fills` will return. This is what reconnecting to a funded venue
    /// actually looks like: a real fill API is windowed, so the history that
    /// explains an old position is simply not available.
    preexisting: Vec<Position>,
    /// Reads fail with `Unreachable` this many times before working.
    unreachable_for: Mutex<u32>,
    /// Orders the venue is holding but has not filled.
    resting: Mutex<Vec<OpenOrder>>,
    /// Cancel requests fail for this order id.
    uncancellable: Option<String>,
}

impl FakeVenue {
    fn new() -> Self {
        Self {
            fills: Mutex::new(Vec::new()),
            seen: Mutex::new(Vec::new()),
            fail_on: None,
            preexisting: Vec::new(),
            unreachable_for: Mutex::new(0),
            resting: Mutex::new(Vec::new()),
            uncancellable: None,
        }
    }

    /// An order the venue is holding. A flatten that ignored these would leave
    /// one live to fill later and re-open the book.
    fn resting_order(self, id: &str, asset: &str, qty: &str) -> Self {
        self.resting.lock().unwrap().push(OpenOrder {
            venue_order_id: id.into(),
            // The runner under test is bot "testbot"; flatten only touches
            // orders carrying its prefix (identity step 1 scoping).
            client_order_id: format!("testbot-{id}"),
            asset: asset.into(),
            side: Side::Buy,
            qty: dec(qty),
            limit_price: dec("50"),
            reserved: dec("100"),
            reserved_currency: "USD".into(),
        });
        self
    }

    fn failing_on(mut self, asset: &str) -> Self {
        self.fail_on = Some(asset.into());
        self
    }

    fn holding(mut self, asset: &str, qty: &str) -> Self {
        self.preexisting.push(Position {
            asset: asset.into(),
            qty: dec(qty),
            avg_price: dec("100"),
        });
        self
    }

    fn uncancellable(mut self, id: &str) -> Self {
        self.uncancellable = Some(id.into());
        self
    }

    fn unreachable_for(self, n: u32) -> Self {
        *self.unreachable_for.lock().unwrap() = n;
        self
    }

    /// A fill the venue reports that we never asked for.
    fn inject_foreign_fill(&self, asset: &str, qty: &str) {
        self.fills.lock().unwrap().push(Fill {
            venue_fill_id: "FOREIGN".into(),
            venue_order_id: "FOREIGN".into(),
            client_order_id: "not-one-of-ours".into(),
            asset: asset.into(),
            side: Side::Buy,
            qty: dec(qty),
            price: dec("100"),
            fee: dec("0"),
            fee_currency: "USD".into(),
            ts: OffsetDateTime::UNIX_EPOCH,
        });
    }

    fn flaky(&self) -> Result<(), VenueError> {
        let mut n = self.unreachable_for.lock().unwrap();
        if *n > 0 {
            *n -= 1;
            return Err(VenueError::Unreachable("connection reset".into()));
        }
        Ok(())
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
            multiplier: Decimal::ONE,
            expiry: None,
            initial_margin: None,
            asset_class: "crypto".into(),
            capabilities: Capabilities {
                stop_orders: false,
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
        self.flaky()?;
        let mut out = derive_positions(&self.fills.lock().unwrap());
        for p in &self.preexisting {
            match out.iter_mut().find(|q| q.asset == p.asset) {
                Some(q) => q.qty += p.qty,
                None => out.push(p.clone()),
            }
        }
        Ok(out)
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

    async fn cancel_order(&self, id: &str) -> Result<(), VenueError> {
        if self.uncancellable.as_deref() == Some(id) {
            return Err(VenueError::Unreachable("cancel rejected".into()));
        }
        let mut resting = self.resting.lock().unwrap();
        match resting.iter().position(|o| o.venue_order_id == id) {
            Some(i) => {
                resting.remove(i);
                Ok(())
            }
            // Cancelling something already gone means our view is wrong, and
            // the trait says that is an error rather than a no-op.
            None => Err(VenueError::UnknownOrder(id.to_string())),
        }
    }

    async fn get_open_orders(&self) -> Result<Vec<OpenOrder>, VenueError> {
        self.flaky()?;
        Ok(self.resting.lock().unwrap().clone())
    }

    async fn get_fills(&self, _since: Option<OffsetDateTime>) -> Result<Vec<Fill>, VenueError> {
        self.flaky()?;
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

#[allow(clippy::too_many_arguments)]
fn runner<'a>(
    venue: &'a FakeVenue,
    clock: &'a TestClock,
    store: &'a RunStore,
    ledger: &'a Ledger,
    controls: PathBuf,
    schedule: Schedule,
) -> Runner<'a, FakeVenue, TestClock> {
    Runner {
        bot_id: "testbot".into(),
        venue,
        clock,
        store,
        ledger,
        controls: runner::ControlStore::File(controls),
        schedule,
        max_plan_age_minutes: 120,
        // Wide enough that fixtures dated at their own `as_of` are not caught by a
        // gate they are not testing; the lag gate has its own tests below.
        max_decision_lag_minutes: 10_000,
        accept_decision_lag: false,
    }
}

#[tokio::test]
async fn every_slice_reaches_the_venue_with_its_own_id() {
    let dir = tmpdir("slices-submit");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new();
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let ledger = Ledger::open(&dir);
    let plan = standard_plan("2026-08-01T00:00:00Z");

    let rec = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    )
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
    let ledger = Ledger::open(&dir);
    let plan = standard_plan("2026-08-01T00:00:00Z");

    let rec = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    )
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
    let ledger = Ledger::open(&dir);
    let plan = standard_plan("2026-08-01T00:00:00Z");

    // The operator throws the switch while the runner waits for slice two.
    let target = t("2026-08-01T02:00:00Z");
    let cpath = controls.clone();
    let clock = TestClock::at("2026-08-01T00:30:00Z").with_hook(move |until| {
        if until >= target {
            ControlFile {
                bot_id: None,
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
        &ledger,
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
    let ledger = Ledger::open(&dir);
    let plan = standard_plan("2026-08-01T00:00:00Z");

    let err = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    )
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
    let ledger = Ledger::open(&dir);
    let plan = standard_plan("2026-08-01T00:00:00Z");

    let err = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    )
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
    let ledger = Ledger::open(&dir);
    let plan = standard_plan("2026-08-01T00:00:00Z");

    runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls.clone(),
        Schedule::default(),
    )
    .run(&plan)
    .await
    .unwrap();
    let submitted = venue.submitted_ids().len();

    let err = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    )
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
    let ledger = Ledger::open(&dir);
    let plan = standard_plan("2026-08-01T00:00:00Z");

    let rec = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    )
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
    let ledger = Ledger::open(&dir);
    let plan = standard_plan("2026-08-01T00:00:00Z");

    runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    )
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
    let ledger = Ledger::open(&dir);

    // Put a book on: one long, one short.
    runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls.clone(),
        Schedule::default(),
    )
    .run(&standard_plan("2026-08-01T00:00:00Z"))
    .await
    .unwrap();
    assert_eq!(venue.filled_qty("BTC"), dec("0.4"));

    let rec = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    )
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
    let ledger = Ledger::open(&dir);

    runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls.clone(),
        Schedule::default(),
    )
    .run(&standard_plan("2026-08-01T00:00:00Z"))
    .await
    .unwrap();

    ControlFile {
        bot_id: None,
        kill_switch: true,
        paused: false,
        reason: Some("stopped".into()),
        set_by: Some("magnus".into()),
        set_at: None,
    }
    .write(&controls)
    .unwrap();

    let rec = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    )
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
    let ledger = Ledger::open(&dir);

    runner(
        &venue,
        &clock,
        &store,
        &ledger,
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
        preexisting: Vec::new(),
        unreachable_for: Mutex::new(0),
        resting: Mutex::new(Vec::new()),
        uncancellable: None,
    };
    let rec = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    )
    .flatten("get me out", "magnus")
    .await
    .unwrap();

    assert_eq!(rec.outcome, "partial");
    assert_eq!(rec.orders_submitted, 1, "SOL still got out");
    assert_eq!(rec.orders_skipped, 1);
    let detail = rec.detail.unwrap();
    assert!(detail.contains("NOT FLAT"), "got {detail}");
    assert!(
        detail.contains("1 position(s) are still open"),
        "got {detail}"
    );
}

#[tokio::test]
async fn flatten_records_who_asked_and_why() {
    let dir = tmpdir("flatten-attributed");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new();
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let ledger = Ledger::open(&dir);

    runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    )
    .flatten("weekend risk", "magnus")
    .await
    .unwrap();

    let back = store.recent(1).unwrap();
    let detail = back[0].detail.clone().unwrap();
    assert!(detail.contains("magnus"), "got {detail}");
    assert!(detail.contains("weekend risk"), "got {detail}");
}

/// The id the ledger authorises must be the id that goes out.
///
/// Rounding is unavoidable - slicing divides quantities and every venue refuses
/// anything off its lot grid - and the client order id is derived from the
/// quantity. Round after authorising and the two diverge: the ledger holds an
/// id the venue never saw, the venue reports a fill under an id the ledger
/// never wrote, and reconciliation calls our own arithmetic an intrusion and
/// halts the account. It did exactly that once; this is the guard.
#[tokio::test]
async fn an_off_grid_quantity_is_rounded_before_it_is_authorised() {
    let dir = tmpdir("lotgrid");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new();
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let ledger = Ledger::open(&dir);

    // FakeVenue's BTC lot is 0.0001, and this asks for eight decimal places.
    let plan = plan_with(
        serde_json::json!([order("BTC", "buy", "0.40007777", "entry")]),
        "2026-08-01T00:00:00Z",
    );

    let outcome = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    )
    .run(&plan)
    .await
    .expect("an off-grid quantity is rounded, not refused");
    assert_eq!(outcome.outcome, "executed", "detail: {:?}", outcome.detail);

    // Every id that went out carries a quantity on the venue's 0.0001 grid.
    // The plan is sliced first, so the division happens before the rounding -
    // which is the case that made this necessary in the first place.
    let sent = venue.submitted_ids();
    assert!(!sent.is_empty(), "something must have been sent");
    for id in &sent {
        let qty: Decimal = id
            .split('-')
            .rev()
            .nth(1)
            .expect("the id carries a quantity")
            .parse()
            .expect("that quantity parses");
        assert!(
            (qty / dec("0.0001")).fract().is_zero(),
            "{id} is off the 0.0001 lot grid"
        );
        // Down, never up: rounding up would trade more than was authorised.
        assert!(qty <= dec("0.40007777"), "{id} rounded away from zero");
    }

    // And the ledger holds that same id, so nothing reads as unauthorised.
    let known = venue.get_fills(None).await.expect("fills");
    let theirs = venue.get_positions().await.expect("positions");
    let report = ledger
        .reconcile(&theirs, &known, dec("0.00000001"))
        .expect("ledger readable");
    assert!(
        report.unknown_fills.is_empty(),
        "the ledger must have authorised exactly what was sent, got {:?}",
        report.unknown_fills
    );
    assert!(report.agrees, "our record and the venue must agree");
}

// --- freshness, history, health --------------------------------------------

#[test]
fn a_fresh_plan_passes_the_freshness_gate() {
    let plan = standard_plan("2026-08-01T00:00:00Z");
    assert!(check_freshness(&plan, t("2026-08-01T01:00:00Z"), 120).is_ok());
    assert!(check_freshness(&plan, t("2026-08-01T03:00:00Z"), 120).is_err());
}

/// Freshness asks how long the file sat on disk, and nothing else.
///
/// It used to measure from `as_of`, which for a daily book is midnight UTC. That
/// made a plan built at 09:00 for today's decision instantly nine hours "stale",
/// and the whole pipeline unexecutable outside a two-hour window after midnight.
#[test]
fn freshness_measures_the_write_time_not_the_decision_date() {
    // Decided at midnight, written at 09:00, executed at 09:30: half an hour old.
    let plan = plan_decided_at_written_at("2026-08-01T00:00:00Z", "2026-08-01T09:00:00Z");
    assert!(check_freshness(&plan, t("2026-08-01T09:30:00Z"), 120).is_ok());
    // ...and four hours after it was written, it is not.
    assert!(check_freshness(&plan, t("2026-08-01T13:00:00Z"), 120).is_err());
}

/// Decision lag asks the other question: how far behind its own decision the
/// fill is. A freshly written plan can still describe a decision the market has
/// left behind, and that is what the Phase 1 lag study measured.
#[test]
fn decision_lag_is_refused_even_when_the_plan_was_just_written() {
    let plan = plan_decided_at_written_at("2026-08-01T00:00:00Z", "2026-08-01T09:00:00Z");
    let now = t("2026-08-01T09:05:00Z");

    // Written five minutes ago, so the freshness gate is happy...
    assert!(check_freshness(&plan, now, 120).is_ok());
    // ...but the decision is 545 minutes behind the fill.
    let err = check_decision_lag(&plan, now, 360, false).expect_err("545 > 360");
    assert!(matches!(err, RunnerError::DecisionLagTooLarge { .. }));

    // Inside the limit it passes.
    assert!(check_decision_lag(&plan, now, 600, false).is_ok());
}

#[test]
fn the_decision_lag_override_lets_a_paper_run_through() {
    let plan = plan_decided_at_written_at("2026-08-01T00:00:00Z", "2026-08-01T09:00:00Z");
    let now = t("2026-08-01T09:05:00Z");
    assert!(check_decision_lag(&plan, now, 360, false).is_err());
    assert!(check_decision_lag(&plan, now, 360, true).is_ok());
}

/// The override is a paper-mode flag, and the refusal is recorded either way.
#[tokio::test]
async fn a_run_past_the_decision_lag_limit_is_refused_and_recorded() {
    let dir = tmpdir("declag");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new();
    let clock = TestClock::at("2026-08-01T09:05:00Z");
    let store = RunStore::new(&dir);
    let ledger = Ledger::open(&dir);
    let mut v: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture parses");
    v["orders"] = serde_json::json!([order("BTC", "buy", "0.4", "entry")]);
    v["as_of"] = serde_json::json!("2026-08-01T00:00:00Z");
    v["created_at"] = serde_json::json!("2026-08-01T09:00:00Z");
    let plan = plan::Plan::parse(&v.to_string()).expect("fixture stays valid");

    let mut r = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    );
    r.max_decision_lag_minutes = 360;

    let err = r
        .run(&plan)
        .await
        .expect_err("545 minutes of lag is refused");
    assert!(matches!(err, RunnerError::DecisionLagTooLarge { .. }));
    assert!(venue.submitted_ids().is_empty(), "nothing may go out");
    assert_eq!(store.recent(1).unwrap()[0].outcome, "refused");
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
        bot_id: None,
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

// --- reconnecting to an account that already has a book ---------------------
//
// Every test here is a situation the system meets on its first day against a
// real venue, and each one has exactly two acceptable outcomes: work, or stop
// with a sentence that names the next command. Silently absorbing the venue's
// version is never one of them.

#[tokio::test]
async fn a_venue_holding_positions_we_have_no_record_of_stops_the_run() {
    let dir = tmpdir("unadopted");
    let controls = running_controls(&dir);
    // The account was funded and traded before this bot ever ran.
    let venue = FakeVenue::new().holding("SOL", "25");
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let ledger = Ledger::open(&dir);

    let rec = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    )
    .run(&standard_plan("2026-08-01T00:00:00Z"))
    .await
    .unwrap();

    assert_eq!(rec.outcome, "halted");
    assert!(venue.submitted_ids().is_empty(), "nothing was sent");
    let detail = rec.detail.expect("a halt says why");
    assert!(detail.contains("SOL"), "names the position: {detail}");
    assert!(
        detail.contains("bot adopt"),
        "and names the command that fixes it: {detail}"
    );
}

#[tokio::test]
async fn adopting_takes_the_position_on_and_the_next_run_proceeds() {
    let dir = tmpdir("adopt-then-run");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new().holding("SOL", "25");
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let ledger = Ledger::open(&dir);
    let r = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    );

    let adopted = r
        .adopt("first connection to the live account", "magnus", false)
        .await
        .unwrap();
    assert_eq!(adopted.outcome, "adopted");
    assert!(adopted.detail.unwrap().contains("magnus"));

    // Our record now explains the book, so reconciliation passes.
    assert!(r.inspect().await.unwrap().agrees);

    // The plan sells 10 SOL out of the 25 we adopted; it goes through.
    let rec = r.run(&standard_plan("2026-08-01T00:00:00Z")).await.unwrap();
    assert_eq!(rec.outcome, "executed", "{:?}", rec.detail);
    assert_eq!(venue.filled_qty("SOL"), dec("10.0"));
}

#[tokio::test]
async fn adopting_is_attributed_and_recorded() {
    let dir = tmpdir("adopt-recorded");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new().holding("SOL", "25");
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let ledger = Ledger::open(&dir);

    runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    )
    .adopt(
        "legacy holding, checked against the venue UI",
        "magnus",
        false,
    )
    .await
    .unwrap();

    let back = store.recent(1).unwrap();
    assert_eq!(back[0].outcome, "adopted");
    let entries = ledger.read().unwrap();
    assert_eq!(entries.len(), 1);
    match &entries[0] {
        runner::Entry::Baseline {
            asset,
            qty,
            by,
            reason,
            ..
        } => {
            assert_eq!(asset, "SOL");
            assert_eq!(*qty, dec("25"));
            assert_eq!(by, "magnus");
            assert!(reason.contains("legacy holding"));
        }
        other => panic!("expected a baseline, got {other:?}"),
    }
}

#[tokio::test]
async fn adopting_twice_is_refused_rather_than_doubling_the_book() {
    let dir = tmpdir("adopt-twice");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new().holding("SOL", "25");
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let ledger = Ledger::open(&dir);
    let r = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    );

    r.adopt("first", "magnus", false).await.unwrap();
    let err = r
        .adopt("again", "magnus", false)
        .await
        .expect_err("there is nothing left to adopt");
    assert!(matches!(err, RunnerError::NothingToAdopt { .. }));
    assert_eq!(ledger.read().unwrap().len(), 1, "no second baseline");
}

#[tokio::test]
async fn a_fill_we_never_authorised_stops_everything() {
    // The most serious thing this system can observe: the venue filled an order
    // under an id our ledger has never seen. Another process on the account, a
    // stale order from an earlier deployment, or a stolen key.
    let dir = tmpdir("foreign-fill");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new();
    venue.inject_foreign_fill("DOGE", "1000");
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let ledger = Ledger::open(&dir);

    let rec = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    )
    .run(&standard_plan("2026-08-01T00:00:00Z"))
    .await
    .unwrap();

    assert_eq!(rec.outcome, "halted");
    assert!(venue.submitted_ids().is_empty());
    let detail = rec.detail.unwrap();
    assert!(detail.contains("never authorised"), "got {detail}");
    assert!(detail.contains("not-one-of-ours"), "names the id: {detail}");
}

#[tokio::test]
async fn adopting_will_not_launder_a_fill_we_never_authorised() {
    // Adoption is the sanctioned way to absorb the venue's view, which makes it
    // exactly the wrong tool for an unexplained fill: it would turn evidence of
    // an intrusion into a legitimate-looking opening position.
    let dir = tmpdir("adopt-launder");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new();
    venue.inject_foreign_fill("DOGE", "1000");
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let ledger = Ledger::open(&dir);

    let err = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    )
    .adopt("just make it go away", "magnus", false)
    .await
    .expect_err("adoption must refuse this");
    assert!(err.to_string().contains("never authorised"), "{err}");
    assert!(ledger.read().unwrap().is_empty(), "nothing was written");
}

#[tokio::test]
async fn order_ids_are_in_our_ledger_before_the_venue_sees_them() {
    // The crash-safety ordering. If we submitted first and recorded after, a
    // process that died in between would restart, find a fill it could not
    // account for, and halt as if the account had been compromised.
    let dir = tmpdir("write-ahead");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new();
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let ledger = Ledger::open(&dir);

    runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    )
    .run(&standard_plan("2026-08-01T00:00:00Z"))
    .await
    .unwrap();

    let authorised: Vec<String> = ledger
        .read()
        .unwrap()
        .into_iter()
        .filter_map(|e| match e {
            runner::Entry::Submitted {
                client_order_id, ..
            } => Some(client_order_id),
            _ => None,
        })
        .collect();
    for id in venue.submitted_ids() {
        assert!(
            authorised.contains(&id),
            "the venue saw {id}, which our ledger never authorised"
        );
    }
    assert_eq!(authorised.len(), 4, "two orders over two slices");
}

#[tokio::test]
async fn a_venue_that_flaps_is_retried_rather_than_failed() {
    let dir = tmpdir("flaky-venue");
    let controls = running_controls(&dir);
    // Two failed reads, then it comes back.
    let venue = FakeVenue::new().unreachable_for(2);
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let ledger = Ledger::open(&dir);

    let rec = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    )
    .run(&standard_plan("2026-08-01T00:00:00Z"))
    .await
    .expect("a couple of dropped reads is not a reason to stop");
    assert_eq!(rec.outcome, "executed", "{:?}", rec.detail);
}

#[tokio::test]
async fn a_venue_that_stays_down_stops_rather_than_guessing() {
    let dir = tmpdir("venue-down");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new().unreachable_for(99);
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let ledger = Ledger::open(&dir);

    let rec = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    )
    .run(&standard_plan("2026-08-01T00:00:00Z"))
    .await
    .unwrap();
    assert_eq!(rec.outcome, "halted");
    assert!(
        rec.detail.unwrap().contains("unreachable"),
        "a venue we cannot read is a recorded halt, not a silent one"
    );
    assert!(
        venue.submitted_ids().is_empty(),
        "we never trade on an account state we could not read"
    );
}

#[tokio::test]
async fn reconciliation_runs_before_every_slice_not_just_the_first() {
    // The execution window is hours long. An account that changes underneath us
    // at minute forty must stop slice two.
    let dir = tmpdir("check-each-slice");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new();
    let store = RunStore::new(&dir);
    let ledger = Ledger::open(&dir);

    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let r = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    );
    // Nothing wrong yet, so slice one goes out. The foreign fill lands while we
    // are waiting for slice two.
    let handle = async {
        venue.inject_foreign_fill("DOGE", "1");
        r.run(&standard_plan("2026-08-01T00:00:00Z")).await
    };
    let rec = handle.await.unwrap();
    assert_eq!(rec.outcome, "halted");
    assert_eq!(rec.slices_completed, 0, "it never got as far as slice one");
}

#[test]
fn a_truncated_ledger_line_is_an_error_rather_than_a_silent_gap() {
    // Half a line is what a crash mid-append leaves. Skipping it would quietly
    // drop an authorised order id and turn its fill into a phantom intrusion.
    let dir = tmpdir("torn-ledger");
    let ledger = Ledger::open(&dir);
    let plan = standard_plan("2026-08-01T00:00:00Z");
    ledger
        .authorise(
            "a-1",
            &plan.orders[0],
            "plan",
            1,
            OffsetDateTime::UNIX_EPOCH,
        )
        .unwrap();
    let mut text = std::fs::read_to_string(ledger.file_path().unwrap()).unwrap();
    text.push_str("{\"kind\":\"submitted\",\"client_order_i");
    std::fs::write(ledger.file_path().unwrap(), text).unwrap();

    let err = ledger.read().expect_err("a torn line must not be skipped");
    assert!(matches!(err, RunnerError::Malformed { .. }), "{err}");
}

#[tokio::test]
async fn a_lost_ledger_can_be_recovered_by_a_human_who_says_so() {
    // The dead end this exists to remove. A wiped disk against a venue that
    // still reports our old fills is, from in here, indistinguishable from
    // someone else trading the account - so the default is a halt. But without
    // a way out, "halted" would be the permanent state of a bot that lost a
    // file, and nobody would ever be able to reconnect one.
    let dir = tmpdir("lost-ledger");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new();
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let ledger = Ledger::open(&dir);

    // Trade normally, then lose the ledger.
    runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls.clone(),
        Schedule::default(),
    )
    .run(&standard_plan("2026-08-01T00:00:00Z"))
    .await
    .unwrap();
    std::fs::remove_file(ledger.file_path().unwrap()).unwrap();

    let r = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    );
    let before = r.inspect().await.unwrap();
    assert!(!before.agrees);
    assert!(
        !before.unknown_fills.is_empty(),
        "our own fills now look foreign"
    );
    assert!(
        before.explain().contains("--accept-unknown-fills"),
        "and the message names the way out: {}",
        before.explain()
    );

    // Refused without the explicit acknowledgement.
    assert!(r.adopt("recovering", "magnus", false).await.is_err());

    let rec = r
        .adopt(
            "restored from backup; checked the account by hand",
            "magnus",
            true,
        )
        .await
        .unwrap();
    assert_eq!(rec.outcome, "adopted");

    // Reconciliation is clean again, and the evidence is kept.
    let after = r.inspect().await.unwrap();
    assert!(after.agrees, "{:?}", after);
    let acknowledged: Vec<_> = ledger
        .read()
        .unwrap()
        .into_iter()
        .filter(|e| matches!(e, runner::Entry::Acknowledged { .. }))
        .collect();
    assert!(
        !acknowledged.is_empty(),
        "the fills are recorded, not erased"
    );
    match &acknowledged[0] {
        runner::Entry::Acknowledged { by, reason, .. } => {
            assert_eq!(by, "magnus");
            assert!(reason.contains("restored from backup"));
        }
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn an_acknowledged_fill_is_not_counted_twice() {
    // Acknowledging says "stop being alarmed"; the baseline written alongside
    // it already accounts for the position. Folding the fill in as well would
    // double our own record of the book.
    let dir = tmpdir("no-double-count");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new();
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let ledger = Ledger::open(&dir);

    runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls.clone(),
        Schedule::default(),
    )
    .run(&standard_plan("2026-08-01T00:00:00Z"))
    .await
    .unwrap();
    std::fs::remove_file(ledger.file_path().unwrap()).unwrap();

    let r = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    );
    r.adopt("recovered", "magnus", true).await.unwrap();

    let after = r.inspect().await.unwrap();
    assert!(after.agrees, "{:?}", after.disagreements);
    let ours: Vec<_> = after.ours.iter().map(|p| (&p.asset, &p.qty)).collect();
    let theirs: Vec<_> = after.theirs.iter().map(|p| (&p.asset, &p.qty)).collect();
    assert_eq!(
        ours, theirs,
        "our record must equal the venue's, not double it"
    );
}

#[tokio::test]
async fn a_run_that_submitted_nothing_leaves_the_plan_runnable() {
    // The replay guard exists to stop a double submission. A halt before
    // anything was sent - a kill switch, a reconciliation stop, an unreachable
    // venue - has nothing to double, and blocking the retry would force a
    // re-plan at the exact moment something has just gone wrong.
    let dir = tmpdir("retry-after-halt");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new().holding("SOL", "25");
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let ledger = Ledger::open(&dir);
    let plan = standard_plan("2026-08-01T00:00:00Z");

    let r = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    );
    let halted = r.run(&plan).await.unwrap();
    assert_eq!(halted.outcome, "halted");
    assert_eq!(halted.orders_submitted, 0);

    // Fix the cause, then run the same plan again.
    r.adopt("checked the account", "magnus", false)
        .await
        .unwrap();
    let rec = r.run(&plan).await.expect("the same plan may be retried");
    assert_eq!(rec.outcome, "executed", "{:?}", rec.detail);
}

#[tokio::test]
async fn a_run_that_did_submit_is_never_replayed() {
    let dir = tmpdir("no-replay-after-submit");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new();
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let ledger = Ledger::open(&dir);
    let plan = standard_plan("2026-08-01T00:00:00Z");

    let r = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    );
    r.run(&plan).await.unwrap();
    let err = r.run(&plan).await.expect_err("orders already went out");
    assert!(matches!(err, RunnerError::AlreadyExecuted { .. }));
}

#[tokio::test]
async fn flatten_leaves_a_sibling_bots_orders_alone() {
    // Identity step 1: on a shared account another bot's resting orders are
    // not ours to cancel — the old venue-wide flatten liquidated the
    // sibling's book, which is the hazard this scoping removes.
    let dir = tmpdir("flatten-scoped");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new().resting_order("V-SIBLING", "BTC", "0.3");
    // overwrite the auto prefix: this order belongs to some OTHER bot
    venue.resting.lock().unwrap()[0].client_order_id = "otherbot-V-SIBLING".into();
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let ledger = Ledger::open(&dir);

    let rec = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    )
    .flatten("get me out", "magnus")
    .await
    .unwrap();

    assert_eq!(rec.outcome, "flattened");
    assert_eq!(
        venue.get_open_orders().await.unwrap().len(),
        1,
        "the sibling bot's order must survive our flatten"
    );
}

#[tokio::test]
async fn flatten_cancels_resting_orders_before_closing_positions() {
    // The gap this closes: a flatten that only sold the positions left a
    // working buy alive, which could fill an hour later and re-open a book
    // somebody had just decided to be out of - and every record would say the
    // flatten succeeded.
    let dir = tmpdir("flatten-cancels");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new().resting_order("V-REST", "BTC", "0.3");
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let ledger = Ledger::open(&dir);

    runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls.clone(),
        Schedule::default(),
    )
    .run(&standard_plan("2026-08-01T00:00:00Z"))
    .await
    .unwrap();
    assert_eq!(venue.get_open_orders().await.unwrap().len(), 1);

    let rec = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    )
    .flatten("get me out", "magnus")
    .await
    .unwrap();

    assert_eq!(rec.outcome, "flattened");
    assert!(
        venue.get_open_orders().await.unwrap().is_empty(),
        "a live order after a flatten is a position waiting to happen"
    );
    let open: Vec<_> = venue
        .get_positions()
        .await
        .unwrap()
        .into_iter()
        .filter(|p| !p.qty.is_zero())
        .collect();
    assert!(open.is_empty(), "still holding: {open:?}");
    assert_eq!(rec.slices[0]["cancelled"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_cancel_that_fails_makes_the_flatten_say_the_account_is_not_flat() {
    // Half a flatten is the dangerous outcome, because it looks like a whole
    // one. The record has to say so in words an operator will not skim past.
    let dir = tmpdir("flatten-cancel-fails");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new()
        .resting_order("V-STUCK", "BTC", "0.3")
        .uncancellable("V-STUCK");
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let ledger = Ledger::open(&dir);

    let rec = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    )
    .flatten("get me out", "magnus")
    .await
    .unwrap();

    assert_eq!(rec.outcome, "partial");
    let detail = rec.detail.unwrap();
    assert!(detail.contains("NOT FLAT"), "got {detail}");
    assert!(detail.contains("may still fill"), "got {detail}");
}

#[tokio::test]
async fn a_flatten_does_not_make_the_system_accuse_itself() {
    // The flatten path originates its own orders rather than taking them from a
    // plan, and it skipped the ledger. Its fills then came back under ids the
    // ledger had never seen, so every use of the emergency exit left the system
    // reporting an intrusion and refusing to trade.
    let dir = tmpdir("flatten-self-accuse");
    let controls = running_controls(&dir);
    let venue = FakeVenue::new();
    let clock = TestClock::at("2026-08-01T00:30:00Z");
    let store = RunStore::new(&dir);
    let ledger = Ledger::open(&dir);
    let r = runner(
        &venue,
        &clock,
        &store,
        &ledger,
        controls,
        Schedule::default(),
    );

    r.run(&standard_plan("2026-08-01T00:00:00Z")).await.unwrap();
    r.flatten("weekend risk", "magnus").await.unwrap();

    let after = r.inspect().await.unwrap();
    assert!(
        after.unknown_fills.is_empty(),
        "the flatten's own fills are ours: {:?}",
        after.unknown_fills
    );
    assert!(after.agrees, "{:?}", after.disagreements);
}
