//! Operational state: when a plan may run, how it is spread over time, and what
//! is recorded afterwards.
//!
//! The executor knows how to submit one plan. This is everything around that.
//!
//! # Controls live on disk, not in memory
//!
//! A kill switch that exists only inside a running process cannot be thrown when
//! the process is wedged, which is exactly when it is needed. So controls are a
//! file, re-read before every slice. Stopping the bot is a write to that file,
//! and it works whether or not anything is currently running.
//!
//! The default when the file is absent or unparseable is **halted**. An operator
//! who deletes it, a disk that fails, or a botched deploy gets a stop rather than
//! an unsupervised bot — the failure direction that costs opportunity instead of
//! money.
//!
//! # Execution is spread over time, and the window is a setting
//!
//! Phase 1 measured both halves of this. Signal decay: Sharpe 2.22 at a one-hour
//! fill lag, 1.86 at eight hours, 0.29 at twenty-four — the collapse is at the
//! day boundary, not the minute. Impact: splitting an order into N hourly slices
//! divides each against the same per-hour liquidity, so cost falls as 1/√N.
//!
//! The two pull opposite ways, so the optimal window grows with the account: two
//! hours at $30k, eight at $3M. A small book has almost no impact to avoid and
//! pays only the decay.
//!
//! What is deliberately **not** here is an adaptive schedule. Real execution
//! reads the book — accelerating into depth, waiting out thin patches, posting
//! rather than crossing. A fixed time slice is the honest floor, and modelling
//! anything better would price in work that has not been done.
//!
//! # Every run is recorded, including the ones that did nothing
//!
//! A halted run and a run that never happened look identical in a directory of
//! successes. Both are written, with the reason, because the question after an
//! incident is always "what did it do at 14:00", and "nothing is stored" is not
//! an answer.

mod ledger;

pub use ledger::{Entry, Ledger, Reconciliation};

use std::fs;
use std::path::{Path, PathBuf};

use executor::{Controls, ExecError, ExecutionRecord};
use plan::{Order, Plan};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use venue::{Fill, VenueAdapter, VenueError};

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("io on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("malformed {what} at {path}: {detail}")]
    Malformed {
        what: &'static str,
        path: PathBuf,
        detail: String,
    },

    #[error(
        "plan {plan_id} was already executed at {when}. Running it again would submit the \
         same orders against a book that has since moved, and no id would collide because \
         the quantities differ. Produce a new plan instead."
    )]
    AlreadyExecuted { plan_id: String, when: String },

    #[error(
        "plan {plan_id} is {age_minutes} minutes old, past the {max_minutes} minute limit. \
         A stale plan was computed against prices that no longer exist; executing it means \
         trading on a view nobody currently holds."
    )]
    PlanTooOld {
        plan_id: String,
        age_minutes: i64,
        max_minutes: i64,
    },

    #[error("execution: {0}")]
    Execution(#[from] ExecError),

    #[error("venue: {0}")]
    Venue(#[from] venue::VenueError),

    /// Our record and the venue's do not line up. Carries the full comparison
    /// and a sentence naming the remedy — see [`Reconciliation::explain`].
    #[error("{}", .0.explain())]
    Reconciliation(Box<Reconciliation>),

    #[error(
        "the venue was unreachable {attempts} times in a row: {last}. Not trading on a view of \
         the account we could not confirm."
    )]
    VenueUnreachable { attempts: u32, last: String },

    #[error(
        "there is nothing to adopt. {detail} Adopting anyway would write a second baseline on \
         top of the first and double every position in our own record."
    )]
    NothingToAdopt { detail: String },
}

fn io(path: &Path) -> impl Fn(std::io::Error) -> RunnerError + '_ {
    move |source| RunnerError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// How many times a read of venue state is retried before giving up.
///
/// Reads only. A failed *read* is a question we can safely ask again; a failed
/// *write* may or may not have landed, and retrying it is how a position gets
/// doubled. Order submission keeps its idempotency contract instead.
const READ_ATTEMPTS: u32 = 3;

/// Retry a venue read while it is unreachable.
///
/// Only [`VenueError::Unreachable`] is retried. A rejection is an answer — the
/// venue has told us something true about the request — and asking again just
/// gets the same answer more slowly.
async fn read_with_retry<T, F, Fut>(mut f: F) -> Result<T, RunnerError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, VenueError>>,
{
    let mut last = String::new();
    for attempt in 1..=READ_ATTEMPTS {
        match f().await {
            Ok(v) => return Ok(v),
            Err(VenueError::Unreachable(e)) => {
                last = e;
                if attempt == READ_ATTEMPTS {
                    return Err(RunnerError::VenueUnreachable {
                        attempts: READ_ATTEMPTS,
                        last,
                    });
                }
            }
            Err(e) => return Err(RunnerError::Venue(e)),
        }
    }
    Err(RunnerError::VenueUnreachable {
        attempts: READ_ATTEMPTS,
        last,
    })
}

fn stamp(t: OffsetDateTime) -> String {
    t.format(&Rfc3339)
        .unwrap_or_else(|_| t.unix_timestamp().to_string())
}

// --- controls ---------------------------------------------------------------

/// The two switches, plus who set them and why.
///
/// `reason` and `set_by` are not decoration. The operator who finds a halted bot
/// at 3am is usually not the one who halted it, and "halted" without "why"
/// invites the worst available response, which is to clear it and see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlFile {
    /// Plan, but never execute. The strongest stop.
    #[serde(default)]
    pub kill_switch: bool,
    /// Execute only orders that reduce exposure.
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub set_by: Option<String>,
    #[serde(default)]
    pub set_at: Option<String>,
}

impl Default for ControlFile {
    /// Absent controls mean **halted**, not "trade freely".
    fn default() -> Self {
        Self {
            kill_switch: true,
            paused: false,
            reason: Some(
                "no control file found; defaulting to halted. Write one to enable trading.".into(),
            ),
            set_by: None,
            set_at: None,
        }
    }
}

impl ControlFile {
    /// Trading permitted, with a stated reason. There is no unattributed start.
    pub fn running(
        reason: impl Into<String>,
        set_by: impl Into<String>,
        at: OffsetDateTime,
    ) -> Self {
        Self {
            kill_switch: false,
            paused: false,
            reason: Some(reason.into()),
            set_by: Some(set_by.into()),
            set_at: Some(stamp(at)),
        }
    }

    pub fn as_controls(&self) -> Controls {
        Controls {
            kill_switch: self.kill_switch,
            paused: self.paused,
        }
    }

    pub fn state(&self) -> &'static str {
        match (self.kill_switch, self.paused) {
            (true, _) => "halted",
            (false, true) => "paused",
            (false, false) => "running",
        }
    }

    /// Read fresh, every time. A control read once at startup is a control that
    /// cannot be changed while it matters. Never cached, and never fails: an
    /// unreadable file resolves to halted rather than to an error a caller might
    /// treat as transient and retry through.
    pub fn read(path: &Path) -> Self {
        let Ok(text) = fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str::<ControlFile>(&text).unwrap_or_else(|e| Self {
            kill_switch: true,
            paused: false,
            reason: Some(format!(
                "control file at {} is unreadable ({e}); halted rather than guessing",
                path.display()
            )),
            set_by: None,
            set_at: None,
        })
    }

    pub fn write(&self, path: &Path) -> Result<(), RunnerError> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(io(path))?;
        }
        let text = serde_json::to_string_pretty(self).expect("ControlFile serialises");
        fs::write(path, text + "\n").map_err(io(path))
    }
}

// --- execution schedule -----------------------------------------------------

/// How a plan's orders are spread over time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    /// Hours between the signal timestamp and the first fill.
    pub lag_hours: u8,
    /// Number of equal slices, one per hour from the first fill.
    pub slices: u8,
}

impl Default for Schedule {
    /// The $30k setting: fill one hour after the signal, in two hourly slices.
    /// Both numbers came out of the phase 1 work; see the module docs.
    fn default() -> Self {
        Self {
            lag_hours: 1,
            slices: 2,
        }
    }
}

impl Schedule {
    /// The window a given NAV should execute over, from the measured capacity
    /// curve: impact falls as 1/√N while signal decay grows with the window, so
    /// the crossing point moves out as the book gets big enough to move prices.
    ///
    /// Bracketed rather than interpolated. The underlying estimate cannot
    /// distinguish three slices from four, and a smooth function would imply it
    /// can.
    pub fn for_nav(nav: Decimal) -> Self {
        let slices = if nav < Decimal::from(100_000) {
            2
        } else if nav < Decimal::from(1_000_000) {
            4
        } else {
            8
        };
        Self {
            lag_hours: 1,
            slices,
        }
    }

    pub fn slice_count(&self) -> u8 {
        self.slices.max(1)
    }

    /// When the last slice goes out.
    pub fn finishes(&self, as_of: OffsetDateTime) -> OffsetDateTime {
        as_of + time::Duration::hours((self.lag_hours as i64) + (self.slice_count() as i64) - 1)
    }
}

/// One slice: the plan with its quantities scaled down, and when to send it.
#[derive(Debug, Clone)]
pub struct PlanSlice {
    pub index: u8,
    pub of: u8,
    pub send_at: OffsetDateTime,
    pub plan: Plan,
}

/// Split a plan into equal hourly slices.
///
/// The rounding residue goes to the **last** slice. A residue left to the end is
/// a small position adjustment; the same residue at the front changes the price
/// the bulk of the order pays, which is the thing slicing exists to protect.
///
/// Order sequence is preserved, so the exits-before-entries invariant the
/// executor asserts still holds within each slice.
pub fn slice_plan(plan: &Plan, schedule: Schedule) -> Vec<PlanSlice> {
    let n = schedule.slice_count();
    let first = plan.as_of + time::Duration::hours(schedule.lag_hours as i64);
    (0..n)
        .map(|i| {
            let is_last = i + 1 == n;
            let orders: Vec<Order> = plan
                .orders
                .iter()
                .filter_map(|o| {
                    let per = (o.qty / Decimal::from(n)).round_dp(8);
                    let qty = if is_last {
                        o.qty - per * Decimal::from(n - 1)
                    } else {
                        per
                    };
                    // A quantity that rounds away is not sent: a zero is a venue
                    // error, and dust pays a minimum fee for nothing.
                    (qty > Decimal::ZERO).then(|| Order { qty, ..o.clone() })
                })
                .collect();
            let mut sliced = plan.clone();
            sliced.orders = orders;
            PlanSlice {
                index: i + 1,
                of: n,
                send_at: first + time::Duration::hours(i as i64),
                plan: sliced,
            }
        })
        .collect()
}

// --- the clock seam ---------------------------------------------------------

/// Waiting, as something a test can control.
///
/// A runner that calls `sleep` directly cannot be tested across a multi-hour
/// execution window without taking multiple hours, so the window would end up
/// untested — and it is the part where the money moves.
///
/// Extends [`venue::Clock`] rather than restating it. Two clock traits in one
/// workspace would eventually be implemented inconsistently by the same type,
/// and the resulting bug — a venue and a runner disagreeing about the time —
/// would be very hard to see.
#[async_trait::async_trait]
pub trait Timer: venue::Clock {
    /// Return at or after `t`. Returning immediately for a `t` already past is
    /// required, not merely allowed: a restart mid-window must catch up rather
    /// than stall.
    async fn sleep_until(&self, t: OffsetDateTime);
}

#[async_trait::async_trait]
impl Timer for venue::SystemClock {
    async fn sleep_until(&self, t: OffsetDateTime) {
        let d = t - venue::Clock::now(self);
        if d.is_positive() {
            tokio::time::sleep(d.unsigned_abs()).await;
        }
    }
}

// --- run history ------------------------------------------------------------

/// What one decision-and-execution cycle did. The unit an operator asks about.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: String,
    pub plan_id: Option<String>,
    pub as_of: String,
    pub recorded_at: String,
    /// `executed`, `partial`, `halted`, or `refused`.
    pub outcome: String,
    /// Present whenever the outcome is not a clean execution. The first thing
    /// anyone reads.
    pub detail: Option<String>,
    pub orders_planned: usize,
    pub orders_submitted: usize,
    pub orders_skipped: usize,
    pub slices_completed: u8,
    pub slices_planned: u8,
    pub nav: Option<String>,
    pub gross_exposure: Option<String>,
    pub net_exposure: Option<String>,
    pub control_state: String,
    /// The risk gate's own numbers, as the planner computed them: every limit,
    /// what it measured, and whether it held.
    ///
    /// Carried into the run record rather than left in the plan file, because
    /// the question "how close to the cap were we when this ran" is asked long
    /// after the plan has been superseded — and an operations view that has to
    /// go and find the plan is one that does not get looked at.
    #[serde(default)]
    pub risk_checks: Vec<plan::RiskCheck>,
    /// One entry per slice attempted, in order. Kept whole rather than merged:
    /// *which* slice failed is the first diagnostic question, and a merged
    /// summary cannot answer it.
    pub slices: Vec<serde_json::Value>,
}

impl RunRecord {
    pub fn is_clean(&self) -> bool {
        self.outcome == "executed"
    }

    /// A run refused before anything was submitted. No capital moved, and it is
    /// still written down.
    pub fn refused(
        plan: &Plan,
        detail: String,
        controls: &ControlFile,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            run_id: plan.run_id.to_string(),
            plan_id: Some(plan.plan_id.to_string()),
            as_of: stamp(plan.as_of),
            recorded_at: stamp(now),
            outcome: "refused".into(),
            detail: Some(detail),
            orders_planned: plan.orders.len(),
            orders_submitted: 0,
            orders_skipped: plan.orders.len(),
            slices_completed: 0,
            slices_planned: 0,
            nav: Some(plan.nav.total.to_string()),
            gross_exposure: Some(plan.nav.gross_exposure.to_string()),
            net_exposure: Some(plan.nav.net_exposure.to_string()),
            control_state: controls.state().into(),
            risk_checks: plan.risk_report.checks.clone(),
            slices: Vec::new(),
        }
    }
}

/// Append-only run history on disk, one file per run.
///
/// One file rather than one growing log: a half-written line in a shared log
/// corrupts the whole history, and a run that died mid-write should cost its own
/// record and no others.
pub struct RunStore {
    root: PathBuf,
}

impl RunStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn dir(&self) -> PathBuf {
        self.root.join("runs")
    }

    pub fn record(&self, run: &RunRecord) -> Result<PathBuf, RunnerError> {
        let dir = self.dir();
        fs::create_dir_all(&dir).map_err(io(&dir))?;
        // Timestamp-prefixed, so a directory listing is chronological without
        // opening anything.
        let name = format!(
            "{}-{}.json",
            run.recorded_at.replace([':', '.'], ""),
            run.run_id
        );
        let path = dir.join(name);
        let text = serde_json::to_string_pretty(run).expect("RunRecord serialises");
        fs::write(&path, text + "\n").map_err(io(&path))?;
        Ok(path)
    }

    /// Most recent first.
    pub fn recent(&self, limit: usize) -> Result<Vec<RunRecord>, RunnerError> {
        let dir = self.dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
            .map_err(io(&dir))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        paths.sort();
        paths.reverse();
        paths
            .into_iter()
            .take(limit)
            .map(|p| {
                let text = fs::read_to_string(&p).map_err(io(&p))?;
                serde_json::from_str(&text).map_err(|e| RunnerError::Malformed {
                    what: "run record",
                    path: p,
                    detail: e.to_string(),
                })
            })
            .collect()
    }

    /// When this plan was executed, if it was.
    ///
    /// Guards what idempotent order ids do not. Re-running an old plan would
    /// submit against a book that has since moved, and every id would be new
    /// because the quantities differ.
    /// A plan counts as executed only if something actually reached the venue.
    ///
    /// The guard exists to stop a double submission, so a run that halted before
    /// sending anything — a kill switch, a reconciliation stop, an unreachable
    /// venue — must leave the plan runnable. Otherwise fixing the cause and
    /// retrying is impossible, and the operator's only route back is to
    /// re-plan, which is a strictly worse thing to force at the moment
    /// something has just gone wrong.
    pub fn executed_at(&self, plan_id: &str) -> Result<Option<String>, RunnerError> {
        Ok(self.recent(500)?.into_iter().find_map(|r| {
            (r.plan_id.as_deref() == Some(plan_id) && r.orders_submitted > 0)
                .then_some(r.recorded_at)
        }))
    }
}

// --- health -----------------------------------------------------------------

/// The answer to "is this thing working right now".
///
/// Deliberately blunt: `ok` is false whenever anything is off, and `notes` says
/// what. A health check with degrees of green is a health check nobody reads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Health {
    pub ok: bool,
    pub control_state: String,
    pub control_reason: Option<String>,
    pub last_run_at: Option<String>,
    pub last_run_outcome: Option<String>,
    pub hours_since_last_run: Option<i64>,
    pub notes: Vec<String>,
}

/// Assess health from the control file and the run history.
///
/// Silence is treated as failure. A runner that stopped being scheduled writes
/// nothing at all, and a check that only inspects the last record it *has* would
/// call that healthy forever.
pub fn health(
    controls: &ControlFile,
    store: &RunStore,
    cadence_hours: i64,
    now: OffsetDateTime,
) -> Result<Health, RunnerError> {
    let recent = store.recent(1)?;
    let last = recent.first();
    let mut notes = Vec::new();
    let mut ok = true;

    if controls.kill_switch {
        ok = false;
        notes.push(format!(
            "kill switch engaged: {}",
            controls.reason.as_deref().unwrap_or("no reason recorded")
        ));
    } else if controls.paused {
        notes.push(format!(
            "paused, risk-reducing orders only: {}",
            controls.reason.as_deref().unwrap_or("no reason recorded")
        ));
    }

    let hours = last.and_then(|r| {
        OffsetDateTime::parse(&r.recorded_at, &Rfc3339)
            .ok()
            .map(|t| (now - t).whole_hours())
    });

    match (last, hours) {
        (None, _) => {
            ok = false;
            notes.push("no run has ever been recorded".into());
        }
        (Some(r), Some(h)) => {
            // A cadence and a half: one late run is a hiccup, two is a stop.
            if h > cadence_hours + cadence_hours / 2 {
                ok = false;
                notes.push(format!(
                    "last run was {h}h ago, past the {cadence_hours}h cadence - the scheduler \
                     may not be running at all"
                ));
            }
            if !r.is_clean() {
                ok = false;
                notes.push(format!(
                    "last run outcome was {}: {}",
                    r.outcome,
                    r.detail.as_deref().unwrap_or("no detail recorded")
                ));
            }
        }
        (Some(_), None) => {
            ok = false;
            notes.push("the last run's timestamp is unparseable".into());
        }
    }

    Ok(Health {
        ok,
        control_state: controls.state().into(),
        control_reason: controls.reason.clone(),
        last_run_at: last.map(|r| r.recorded_at.clone()),
        last_run_outcome: last.map(|r| r.outcome.clone()),
        hours_since_last_run: hours,
        notes,
    })
}

// --- the run itself ---------------------------------------------------------

/// Everything a run needs that is not the plan.
pub struct Runner<'a, V: VenueAdapter + ?Sized, C: Timer> {
    pub venue: &'a V,
    pub clock: &'a C,
    pub store: &'a RunStore,
    /// Our own record of what we authorised. The independent half of
    /// reconciliation — see the [`ledger`] module docs.
    pub ledger: &'a Ledger,
    pub controls_path: PathBuf,
    pub schedule: Schedule,
    /// How old a plan may be when the first slice goes out.
    pub max_plan_age_minutes: i64,
}

impl<V: VenueAdapter + ?Sized, C: Timer> Runner<'_, V, C> {
    /// Execute one plan across its slices, recording the outcome either way.
    ///
    /// The gates run before anything is submitted, and each writes a recorded
    /// refusal rather than only returning an error: a refused plan is an
    /// operational event, and one that leaves no trace is indistinguishable from
    /// a run that never happened.
    ///
    /// Controls are re-read before every slice. A kill switch thrown at minute
    /// forty of a two-hour window must stop the second slice, and controls read
    /// once at the top would not.
    pub async fn run(&self, plan: &Plan) -> Result<RunRecord, RunnerError> {
        let now = self.clock.now();
        let controls = ControlFile::read(&self.controls_path);

        if let Some(when) = self.store.executed_at(&plan.plan_id.to_string())? {
            let e = RunnerError::AlreadyExecuted {
                plan_id: plan.plan_id.to_string(),
                when,
            };
            self.store
                .record(&RunRecord::refused(plan, e.to_string(), &controls, now))?;
            return Err(e);
        }

        if let Err(e) = check_freshness(plan, now, self.max_plan_age_minutes) {
            self.store
                .record(&RunRecord::refused(plan, e.to_string(), &controls, now))?;
            return Err(e);
        }

        if controls.kill_switch {
            let detail = format!(
                "kill switch engaged: {}",
                controls.reason.as_deref().unwrap_or("no reason recorded")
            );
            self.store
                .record(&RunRecord::refused(plan, detail, &controls, now))?;
            return Err(RunnerError::Execution(ExecError::KillSwitchEngaged));
        }

        let slices = slice_plan(plan, self.schedule);
        let mut record = RunRecord {
            run_id: plan.run_id.to_string(),
            plan_id: Some(plan.plan_id.to_string()),
            as_of: stamp(plan.as_of),
            recorded_at: stamp(now),
            outcome: "executed".into(),
            detail: None,
            orders_planned: plan.orders.len(),
            orders_submitted: 0,
            orders_skipped: 0,
            slices_completed: 0,
            slices_planned: slices.len() as u8,
            nav: Some(plan.nav.total.to_string()),
            gross_exposure: Some(plan.nav.gross_exposure.to_string()),
            net_exposure: Some(plan.nav.net_exposure.to_string()),
            control_state: controls.state().into(),
            risk_checks: plan.risk_report.checks.clone(),
            slices: Vec::new(),
        };

        for slice in &slices {
            self.clock.sleep_until(slice.send_at).await;

            // Re-read: the window is long enough for a human to intervene inside
            // it, which is half the point of having a window.
            let controls = ControlFile::read(&self.controls_path);
            record.control_state = controls.state().into();
            if controls.kill_switch {
                record.outcome = "halted".into();
                record.detail = Some(format!(
                    "kill switch engaged before slice {} of {}: {}. The remaining slices were \
                     not sent, so the book is part-way to target; the next plan will diff from \
                     where it actually is.",
                    slice.index,
                    slice.of,
                    controls.reason.as_deref().unwrap_or("no reason recorded")
                ));
                record.orders_skipped += slice.plan.orders.len();
                break;
            }

            // Reconcile our ledger against the venue before every slice, not
            // just at the top of the run. The window is long enough for the
            // account to change underneath us, and the whole point of slicing
            // is that we are still deciding to trade an hour later.
            if let Err(e) = self.reconcile().await {
                record.outcome = "halted".into();
                record.detail = Some(e.to_string());
                record.orders_skipped += slice.plan.orders.len();
                break;
            }

            // Authorise the ids before sending them. Written first so that a
            // crash between the two leaves an order we asked for and never
            // sent — recoverable — rather than a fill we cannot account for,
            // which reads as an intrusion.
            let plan_id = slice.plan.plan_id.to_string();
            for o in &slice.plan.orders {
                let coid = executor::client_order_id_for_slice(&plan_id, o, slice.index);
                self.ledger
                    .authorise(&coid, o, &plan_id, slice.index, self.clock.now())?;
            }

            // The executor reconciles too, against the same venue fills. Ours
            // is the check with teeth; its is the last line before an order and
            // stays exactly as it was.
            let known = read_with_retry(|| self.venue.get_fills(None)).await?;
            let ours = self.ledger.our_positions(&known)?;
            let outcome = executor::execute_slice(
                self.venue,
                &slice.plan,
                Some(slice.index),
                &ours,
                controls.as_controls(),
                self.clock.now(),
            )
            .await;

            match outcome {
                Ok(exec) => {
                    record.orders_submitted +=
                        exec.submitted.iter().filter(|o| o.error.is_none()).count();
                    record.orders_skipped += exec.not_attempted.len();
                    record.slices_completed = slice.index;
                    let complete = exec.is_complete();
                    let halted = exec.halted_reason.clone();
                    let skipped = exec.not_attempted.len();
                    record.slices.push(slice_value(slice, &exec));
                    if !complete {
                        record.outcome = "partial".into();
                        record.detail = Some(halted.unwrap_or_else(|| {
                            format!(
                                "slice {} of {} left {skipped} order(s) unattempted",
                                slice.index, slice.of
                            )
                        }));
                        break;
                    }
                }
                Err(e) => {
                    record.outcome = "halted".into();
                    record.detail =
                        Some(format!("slice {} of {} failed: {e}", slice.index, slice.of));
                    record.orders_skipped += slice.plan.orders.len();
                    break;
                }
            }
        }

        record.recorded_at = stamp(self.clock.now());
        self.store.record(&record)?;
        Ok(record)
    }

    /// Compare our ledger against the venue, and report rather than act.
    ///
    /// Safe to call at any time — it places no orders and writes nothing. This
    /// is what `bot reconcile` runs, and what every slice runs before it
    /// submits.
    pub async fn inspect(&self) -> Result<Reconciliation, RunnerError> {
        let positions = read_with_retry(|| self.venue.get_positions()).await?;
        let fills = read_with_retry(|| self.venue.get_fills(None)).await?;
        let eps: Decimal = executor::POSITION_EPSILON
            .parse()
            .expect("epsilon is a literal");
        self.ledger.reconcile(&positions, &fills, eps)
    }

    /// [`inspect`](Self::inspect), but a disagreement is an error.
    pub async fn reconcile(&self) -> Result<Reconciliation, RunnerError> {
        let report = self.inspect().await?;
        if report.agrees {
            Ok(report)
        } else {
            Err(RunnerError::Reconciliation(Box::new(report)))
        }
    }

    /// Take the venue's current positions on as our opening state.
    ///
    /// This is the one operation that changes our record to match the venue —
    /// precisely the auto-repair the executor refuses to do — so it is manual,
    /// attributed, and recorded as a run. It exists because the alternative is
    /// worse: without it, connecting to an account that already holds anything
    /// is a permanent halt, and the system could never be pointed at a funded
    /// account at all.
    ///
    /// It refuses when there is nothing unexplained to adopt. Adopting a book
    /// we already agree about would write a second baseline on top of the
    /// first and double every position in our own record.
    pub async fn adopt(
        &self,
        reason: &str,
        by: &str,
        accept_unknown_fills: bool,
    ) -> Result<RunRecord, RunnerError> {
        let now = self.clock.now();
        let controls = ControlFile::read(&self.controls_path);
        let report = self.inspect().await?;

        if !report.unknown_fills.is_empty() && !accept_unknown_fills {
            // By default adoption would launder these into a legitimate-looking
            // opening position, so it refuses. The override exists because a
            // lost ledger produces exactly this signature and the alternative
            // would be halted forever — but it has to be asked for by name, and
            // it writes down who asked.
            return Err(RunnerError::Reconciliation(Box::new(report)));
        }
        if report.unadopted.is_empty() && report.unknown_fills.is_empty() {
            return Err(RunnerError::NothingToAdopt {
                detail: report.explain(),
            });
        }

        // Acknowledge first. If this is interrupted half-way the fills stay
        // alarming, which is the direction to fail in.
        let mut acknowledged = Vec::new();
        if accept_unknown_fills {
            for f in &report.unknown_fills {
                let e = Entry::Acknowledged {
                    venue_fill_id: f.venue_fill_id.clone(),
                    client_order_id: f.client_order_id.clone(),
                    asset: f.asset.clone(),
                    qty: f.qty.parse().unwrap_or_default(),
                    fill_at: f.at.clone(),
                    at: stamp(now),
                    by: by.to_string(),
                    reason: reason.to_string(),
                };
                self.ledger.append(&e)?;
                acknowledged.push(e);
            }
        }

        // Re-derive what still needs a baseline: acknowledging the unknown
        // fills has already changed the answer, and adopting against the stale
        // view would write a baseline for a position now explained twice.
        let after = self.inspect().await?;
        let positions = read_with_retry(|| self.venue.get_positions()).await?;
        let adopting: Vec<_> = positions
            .into_iter()
            .filter(|p| after.unadopted.iter().any(|a| a == &p.asset))
            .collect();
        let written = self.ledger.adopt(&adopting, by, reason, now)?;

        let record = RunRecord {
            run_id: format!("adopt-{}", stamp(now)),
            plan_id: None,
            as_of: stamp(now),
            recorded_at: stamp(now),
            outcome: "adopted".into(),
            detail: Some(format!(
                "{by} adopted {} pre-existing position(s) as an opening baseline{}. Reason: {reason}",
                written.len(),
                if acknowledged.is_empty() {
                    String::new()
                } else {
                    format!(
                        ", and accepted {} venue fill(s) we never authorised as our own",
                        acknowledged.len()
                    )
                },
            )),
            orders_planned: 0,
            orders_submitted: 0,
            orders_skipped: 0,
            slices_completed: 0,
            slices_planned: 0,
            nav: None,
            gross_exposure: None,
            net_exposure: None,
            control_state: controls.state().into(),
            risk_checks: Vec::new(),
            slices: vec![serde_json::json!({
                "adopted": written,
                "acknowledged": acknowledged,
            })],
        };
        self.store.record(&record)?;
        Ok(record)
    }

    /// Close every open position at market, now.
    ///
    /// This is the control the kill switch does not give you. A kill switch
    /// stops the system taking *new* risk; it does nothing about the risk
    /// already on the book, and the moment an operator most wants to stop
    /// trading is usually the moment they most want to be flat.
    ///
    /// It deliberately ignores the kill switch and the pause, because both mean
    /// "take no more risk" and this only ever sheds it. What it will not do is
    /// run without a named human and a stated reason: an automatic flatten is a
    /// second strategy, and the one thing worse than a stuck position is a
    /// system that liquidates the book on its own reasoning.
    ///
    /// It reads positions from the **venue**, not from a plan. Flattening
    /// against our own view of the book would leave behind exactly the position
    /// our view was wrong about.
    ///
    /// Resting orders are cancelled first, then positions closed. Both halves
    /// are required for "flat" to mean anything.
    pub async fn flatten(&self, reason: &str, by: &str) -> Result<RunRecord, RunnerError> {
        let now = self.clock.now();
        let controls = ControlFile::read(&self.controls_path);

        // Resting orders go first. A flatten that closed every position and
        // left the working orders alive would let one fill an hour later and
        // re-open a book somebody had just decided to be out of — and every
        // record would say the flatten worked.
        let resting = read_with_retry(|| self.venue.get_open_orders()).await?;
        let mut cancelled = Vec::new();
        let mut cancel_failures = 0usize;
        for o in &resting {
            match self.venue.cancel_order(&o.venue_order_id).await {
                Ok(()) => cancelled.push(serde_json::json!({
                    "asset": o.asset, "qty": o.qty.normalize().to_string(),
                    "venue_order_id": o.venue_order_id, "error": serde_json::Value::Null,
                })),
                Err(e) => {
                    cancel_failures += 1;
                    cancelled.push(serde_json::json!({
                        "asset": o.asset, "qty": o.qty.normalize().to_string(),
                        "venue_order_id": o.venue_order_id, "error": e.to_string(),
                    }));
                }
            }
        }

        // Positions are read *after* cancelling: a cancel releases inventory a
        // resting sell had claimed, and sizing the exit off the earlier view
        // would leave part of the book behind.
        let positions = read_with_retry(|| self.venue.get_positions()).await?;
        let open: Vec<_> = positions.iter().filter(|p| !p.qty.is_zero()).collect();

        let mut record = RunRecord {
            run_id: format!("flatten-{}", stamp(now)),
            plan_id: None,
            as_of: stamp(now),
            recorded_at: stamp(now),
            outcome: "flattened".into(),
            detail: Some(format!("flatten requested by {by}: {reason}")),
            orders_planned: open.len() + resting.len(),
            orders_submitted: 0,
            orders_skipped: cancel_failures,
            slices_completed: 0,
            slices_planned: 0,
            nav: None,
            gross_exposure: None,
            net_exposure: None,
            control_state: controls.state().into(),
            risk_checks: Vec::new(),
            slices: Vec::new(),
        };

        let mut outcomes = Vec::new();
        for p in open {
            // Opposite side, full size. No slicing: a flatten is an emergency,
            // and paying impact to get out is the trade being made.
            let side = if p.qty.is_sign_positive() {
                venue::Side::Sell
            } else {
                venue::Side::Buy
            };
            let qty = p.qty.abs();
            let req = venue::OrderRequest {
                // Deterministic in asset and size, so a retry after a crash
                // resubmits the same id rather than doubling the exit.
                client_order_id: format!("FLAT-{}-{}", p.asset.as_str(), qty.normalize()),
                asset: p.asset.clone(),
                side,
                qty,
                order_type: venue::OrderType::Market,
                limit_price: None,
                reason: plan::OrderReason::Exit,
            };
            // Written before the order goes out, exactly as in a planned run.
            self.ledger.authorise_exit(
                &req.client_order_id,
                &req.asset,
                req.side,
                req.qty,
                self.clock.now(),
            )?;

            match self.venue.place_order(&req).await {
                Ok(ack) => {
                    record.orders_submitted += 1;
                    outcomes.push(serde_json::json!({
                        "asset": p.asset.as_str(), "qty": qty.to_string(),
                        "venue_order_id": ack.venue_order_id, "error": serde_json::Value::Null,
                    }));
                }
                Err(e) => {
                    // Unlike a plan, a flatten does NOT stop at the first
                    // failure. Every remaining position is still exposure, and
                    // one venue rejection is no reason to leave the rest on.
                    record.orders_skipped += 1;
                    record.outcome = "partial".into();
                    outcomes.push(serde_json::json!({
                        "asset": p.asset.as_str(), "qty": qty.to_string(),
                        "venue_order_id": serde_json::Value::Null, "error": e.to_string(),
                    }));
                }
            }
        }

        if cancel_failures > 0 {
            record.outcome = "partial".into();
        }
        if record.orders_skipped > 0 {
            let exits_failed = record.orders_skipped - cancel_failures;
            let mut what = Vec::new();
            if exits_failed > 0 {
                what.push(format!("{exits_failed} position(s) are still open"));
            }
            if cancel_failures > 0 {
                what.push(format!(
                    "{cancel_failures} resting order(s) could not be cancelled and may still fill"
                ));
            }
            record.detail = Some(format!(
                "flatten requested by {by}: {reason}. THE ACCOUNT IS NOT FLAT: {}.",
                what.join("; ")
            ));
        }
        record
            .slices
            .push(serde_json::json!({"cancelled": cancelled, "exits": outcomes}));
        record.recorded_at = stamp(self.clock.now());
        self.store.record(&record)?;
        Ok(record)
    }
}

fn slice_value(slice: &PlanSlice, exec: &ExecutionRecord) -> serde_json::Value {
    serde_json::json!({
        "index": slice.index,
        "of": slice.of,
        "send_at": stamp(slice.send_at),
        "execution": serde_json::to_value(exec).unwrap_or(serde_json::Value::Null),
    })
}

/// Refuse a plan that has gone stale.
///
/// A plan is a view about prices at a moment. Executing an old one means trading
/// on a view nobody currently holds, and the wider the gap the less it resembles
/// the strategy that was tested.
pub fn check_freshness(
    plan: &Plan,
    now: OffsetDateTime,
    max_minutes: i64,
) -> Result<(), RunnerError> {
    let age = (now - plan.as_of).whole_minutes();
    if age > max_minutes {
        return Err(RunnerError::PlanTooOld {
            plan_id: plan.plan_id.to_string(),
            age_minutes: age,
            max_minutes,
        });
    }
    Ok(())
}

/// Fills belonging to a plan, by the id the executor stamps on every order.
///
/// Attribution runs off the client order id rather than a timestamp window,
/// because two runs can overlap and a time-based split would assign the overlap
/// to whichever one is asked first.
pub fn fills_for_plan<'f>(plan_id: &str, fills: &'f [Fill]) -> Vec<&'f Fill> {
    let short = plan_id.split('-').next().unwrap_or(plan_id);
    let prefix = format!("{short}-");
    fills
        .iter()
        .filter(|f| f.client_order_id.starts_with(&prefix))
        .collect()
}
