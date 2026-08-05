//! Steps 9–10: submit a plan's orders, then verify against venue truth.
//!
//! This is the only part of the system that can move capital, and every design
//! choice here follows from that.
//!
//! # Reconciliation drift is never repaired
//!
//! Our positions come from folding our own append-only fill log. The venue's
//! come from the venue. When they disagree, one of our assumptions is wrong —
//! and the tempting response, adjusting our record to match, is the worst
//! available: it destroys the evidence of what went wrong and leaves the system
//! trading on top of a bug it has just concealed.
//!
//! So a mismatch **stops the executor**, loudly, with both numbers reported. A
//! human decides. This is [`ExecError::Reconciliation`] and it is not
//! recoverable by retrying.
//!
//! # Idempotency is the crash-safety property
//!
//! A process that dies between submitting an order and recording its
//! acknowledgement must be safe to restart. That works because
//! `client_order_id` is derived deterministically from the plan and the order —
//! never from a counter or a clock — so a replay submits the *same* id, and the
//! venue contract says an identical id returns the original ack rather than
//! placing a second order.
//!
//! The failure this prevents is doubling a position on restart, which is both
//! the most likely operational error and among the most expensive.
//!
//! # Ordering, and why exits go first
//!
//! Exits free capital before entries size against it. The planner already emits
//! orders in that order, and this module asserts it rather than assuming it: a
//! reordering upstream would otherwise silently produce entries sized off a NAV
//! that had not yet been realised.
//!
//! # What this module refuses to do
//!
//! It does not re-plan, re-price, or re-size. If a plan is stale the answer is a
//! new plan, not an executor that quietly improves on an old one — an executor
//! with an opinion is a second strategy nobody backtested.

use std::collections::BTreeMap;

use plan::{Order, Plan, Side, Status};
use rust_decimal::Decimal;
use serde::Serialize;
use time::OffsetDateTime;
use venue::{
    derive_positions, AssetId, Fill, OrderRequest, OrderType as VenueOrderType, Position,
    VenueAdapter, VenueError,
};

/// A tolerance for position comparison, in base units.
///
/// Not zero: venues round quantities to their own lot sizes, so an exact
/// equality test would flag arithmetic rather than drift. Deliberately tiny —
/// anything a human would notice must trip the check.
pub const POSITION_EPSILON: &str = "0.00000001";

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error(
        "reconciliation failed for {asset}: we derive {ours} from our fill log, the venue \
         reports {theirs}. STOPPED. One of our assumptions is wrong and trading on top of \
         it would turn a bug into a loss. This is never auto-corrected."
    )]
    Reconciliation {
        asset: String,
        ours: Decimal,
        theirs: Decimal,
    },

    #[error(
        "plan {plan_id} has status {status:?}; only an accepted plan may be executed. A \
         rejected plan breached a limit, and executing it anyway would make the risk gate \
         decorative."
    )]
    NotAccepted { plan_id: String, status: String },

    #[error(
        "plan {plan_id} is in {mode} mode, not live. Dry-run plans exist to be inspected; \
         executing one would mean the mode flag protects nothing."
    )]
    NotLive { plan_id: String, mode: String },

    #[error(
        "orders are out of sequence: an entry for {entry} precedes an exit for {exit}. \
         Exits must come first so entries size against realised capital, and this is \
         asserted rather than assumed because a reordering upstream would be silent."
    )]
    OrderSequence { entry: String, exit: String },

    #[error("the kill switch is engaged: planning continues, execution does not")]
    KillSwitchEngaged,

    #[error(
        "paused, and plan {plan_id} contains no risk-reducing orders. A pause means stop \
         taking on risk, not stop being able to shed it - so exits and reductions would \
         still have run. There were none."
    )]
    PausedWithNothingPermitted { plan_id: String },

    #[error("venue: {0}")]
    Venue(#[from] VenueError),
}

/// What one order did. Recorded whether or not it succeeded, because a run that
/// half-worked is exactly the case a human needs the detail for.
#[derive(Debug, Clone, Serialize)]
pub struct OrderOutcome {
    pub client_order_id: String,
    pub asset: String,
    pub side: String,
    pub qty: Decimal,
    pub reason: String,
    /// `None` when submission failed; the error is in `error`.
    pub venue_order_id: Option<String>,
    pub error: Option<String>,
}

/// The record of an execution attempt. Serialised alongside the plan, so a run
/// can be audited without re-deriving anything.
#[derive(Debug, Clone, Serialize)]
pub struct ExecutionRecord {
    pub plan_id: String,
    pub run_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    pub submitted: Vec<OrderOutcome>,
    /// Orders not attempted because an earlier one failed. Named rather than
    /// counted: "3 skipped" is not actionable and "we skipped these three" is.
    pub not_attempted: Vec<String>,
    pub reconciled: bool,
    /// True when a pause restricted this run to risk-reducing orders.
    pub paused: bool,
    pub halted_reason: Option<String>,
}

/// Operational overrides, passed in rather than read from a global.
///
/// An executor that reads its own kill switch from the environment cannot be
/// tested against both states, and the untested state is the one that matters.
#[derive(Debug, Clone, Copy, Default)]
pub struct Controls {
    /// Plan, but never execute. The strongest stop.
    pub kill_switch: bool,
    /// Execute only orders that reduce exposure.
    pub paused: bool,
}

impl ExecutionRecord {
    pub fn is_complete(&self) -> bool {
        self.halted_reason.is_none()
            && self.not_attempted.is_empty()
            && self.submitted.iter().all(|o| o.error.is_none())
    }
}

/// Deterministic id for one order within one plan.
///
/// The whole crash-safety story rests on this being a pure function of
/// (plan, asset, side, qty). A counter or a timestamp here would make a replay
/// submit a *different* id, the venue would treat it as a new order, and a
/// restart would double the position.
///
/// Quantity is included because two orders for the same asset and side within
/// one plan would otherwise collide — which the plan contract forbids, but an
/// id scheme that depends on that invariant holding is a worse scheme.
pub fn client_order_id(plan_id: &str, order: &Order) -> String {
    let side = match order.side {
        Side::Buy => "B",
        Side::Sell => "S",
    };
    // Plan ids are uuids; the first segment is ample to disambiguate and keeps
    // the id inside the length limits venues impose.
    let short = plan_id.split('-').next().unwrap_or(plan_id);
    format!("{short}-{}-{side}-{}", order.asset, order.qty.normalize())
}

/// Whether an order reduces exposure.
///
/// Delegates to the plan crate rather than re-deciding: that judgement governs
/// both the pause rule and the exits-first ordering, and two copies of it would
/// eventually disagree.
fn reduces_exposure(order: &Order) -> bool {
    order.reason.permitted_while_paused()
}

/// Assert exits precede entries. See [`ExecError::OrderSequence`].
fn check_sequence(orders: &[Order]) -> Result<(), ExecError> {
    let mut seen_entry: Option<&str> = None;
    for o in orders {
        if reduces_exposure(o) {
            if let Some(entry) = seen_entry {
                return Err(ExecError::OrderSequence {
                    entry: entry.to_string(),
                    exit: o.asset.clone(),
                });
            }
        } else {
            seen_entry = Some(&o.asset);
        }
    }
    Ok(())
}

fn to_request(plan_id: &str, order: &Order) -> OrderRequest {
    OrderRequest {
        client_order_id: client_order_id(plan_id, order),
        asset: AssetId::from(order.asset.clone()),
        side: order.side,
        qty: order.qty,
        order_type: match order.order_type {
            plan::OrderType::Market => VenueOrderType::Market,
            plan::OrderType::Limit => VenueOrderType::Limit,
        },
        limit_price: order.limit_price,
        reason: order.reason,
    }
}

/// Compare our fill-derived positions against the venue's.
///
/// Returns the first disagreement rather than all of them: the correct response
/// to any drift is to stop and investigate, so enumerating the rest adds noise
/// to a decision that has already been made.
pub fn reconcile(ours: &[Position], theirs: &[Position]) -> Result<(), ExecError> {
    let eps: Decimal = POSITION_EPSILON.parse().expect("epsilon is a literal");
    let mut mine: BTreeMap<&str, Decimal> = BTreeMap::new();
    for p in ours {
        *mine.entry(p.asset.as_str()).or_default() += p.qty;
    }
    let mut yours: BTreeMap<&str, Decimal> = BTreeMap::new();
    for p in theirs {
        *yours.entry(p.asset.as_str()).or_default() += p.qty;
    }
    // Union of both sides: a position we hold and the venue does not is drift,
    // and so is the reverse. Iterating only our own would miss the second.
    let assets: std::collections::BTreeSet<&str> =
        mine.keys().chain(yours.keys()).copied().collect();
    for asset in assets {
        let a = mine.get(asset).copied().unwrap_or_default();
        let b = yours.get(asset).copied().unwrap_or_default();
        if (a - b).abs() > eps {
            return Err(ExecError::Reconciliation {
                asset: asset.to_string(),
                ours: a,
                theirs: b,
            });
        }
    }
    Ok(())
}

/// Submit an accepted plan's orders, then reconcile.
///
/// Stops at the first submission failure rather than pressing on. A plan is a
/// target *state*; executing an arbitrary subset of it produces a book that
/// matches no plan at all, and the next run's diff converges from wherever it
/// actually is. Stopping early with the remainder named is recoverable; a
/// partially-applied plan is not.
pub async fn execute<V: VenueAdapter>(
    venue: &V,
    plan: &Plan,
    known_fills: &[Fill],
    controls: Controls,
    now: OffsetDateTime,
) -> Result<ExecutionRecord, ExecError> {
    let plan_id = plan.plan_id.to_string();
    let mut record = ExecutionRecord {
        plan_id: plan_id.clone(),
        run_id: plan.run_id.to_string(),
        started_at: now,
        submitted: Vec::new(),
        not_attempted: Vec::new(),
        reconciled: false,
        paused: false,
        halted_reason: None,
    };

    if controls.kill_switch {
        record.halted_reason = Some(ExecError::KillSwitchEngaged.to_string());
        return Err(ExecError::KillSwitchEngaged);
    }
    if plan.status != Status::Accepted {
        return Err(ExecError::NotAccepted {
            plan_id,
            status: format!("{:?}", plan.status),
        });
    }
    if plan.mode != plan::Mode::Live {
        return Err(ExecError::NotLive {
            plan_id,
            mode: format!("{:?}", plan.mode),
        });
    }
    check_sequence(&plan.orders)?;

    // Under a pause, only risk-reducing orders run. Filtering here rather than
    // refusing the whole plan is the point of the distinction: a paused book
    // must still be able to get out.
    let orders: Vec<&Order> = if controls.paused {
        let permitted: Vec<&Order> = plan.orders.iter().filter(|o| reduces_exposure(o)).collect();
        if permitted.is_empty() {
            return Err(ExecError::PausedWithNothingPermitted { plan_id });
        }
        record.paused = true;
        for o in plan.orders.iter().filter(|o| !reduces_exposure(o)) {
            record.not_attempted.push(o.asset.clone());
        }
        permitted
    } else {
        plan.orders.iter().collect()
    };

    // Reconcile BEFORE trading. Submitting into a book we do not understand is
    // how a small discrepancy becomes a large one.
    let theirs = venue.get_positions().await?;
    let ours = derive_positions(known_fills);
    if let Err(e) = reconcile(&ours, &theirs) {
        record.halted_reason = Some(e.to_string());
        return Err(e);
    }
    record.reconciled = true;

    let mut failed = false;
    let total = orders.len();
    for (i, order) in orders.iter().enumerate() {
        if failed {
            record.not_attempted.push(order.asset.clone());
            continue;
        }
        let req = to_request(&plan_id, order);
        let coid = req.client_order_id.clone();
        match venue.place_order(&req).await {
            Ok(ack) => record.submitted.push(OrderOutcome {
                client_order_id: coid,
                asset: order.asset.clone(),
                side: format!("{:?}", order.side),
                qty: order.qty,
                reason: format!("{:?}", order.reason),
                venue_order_id: Some(ack.venue_order_id),
                error: None,
            }),
            Err(e) => {
                failed = true;
                record.submitted.push(OrderOutcome {
                    client_order_id: coid,
                    asset: order.asset.clone(),
                    side: format!("{:?}", order.side),
                    qty: order.qty,
                    reason: format!("{:?}", order.reason),
                    venue_order_id: None,
                    error: Some(e.to_string()),
                });
                record.halted_reason = Some(format!(
                    "order {} of {} failed for {}: {e}. The remaining orders were not \
                     attempted; the next run will diff from wherever the book actually is.",
                    i + 1,
                    total,
                    order.asset
                ));
            }
        }
    }
    Ok(record)
}
