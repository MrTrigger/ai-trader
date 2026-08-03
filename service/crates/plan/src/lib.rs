//! The Plan contract, as the executor sees it.
//!
//! This crate parses plans. It deliberately cannot construct one: the planner
//! decides and the executor executes, and a type that can build a plan in this
//! process would be a way for the executing half to invent work for itself.
//!
//! Three properties mirror the Python side, and each exists because of a
//! specific way the seam could rot:
//!
//! **Decimals arrive as strings.** A JSON number is an `f64` on both sides and
//! the two languages need not round one identically. `rust_decimal` parses the
//! exact digits Python emitted.
//!
//! **Unknown fields are rejected.** `deny_unknown_fields` everywhere. A field
//! this build does not understand means the planner knows something this
//! executor does not, and guessing is worse than stopping (design spec 0.3).
//!
//! **Unknown schema versions are refused, not best-effort parsed.** A major
//! version this build was not written against is a hard error before any field
//! is read.

use rust_decimal::Decimal;
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

/// The schema major version this build understands.
///
/// Bumping this is a deliberate act: it asserts the types below were reviewed
/// against the new schema. It is not a knob for making a parse error go away.
pub const SUPPORTED_MAJOR: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("malformed plan: {0}")]
    Malformed(#[from] serde_json::Error),

    #[error(
        "unsupported schema version {found}: this build understands major {SUPPORTED_MAJOR}. \
         Refusing to guess at a plan written by a different planner."
    )]
    UnsupportedVersion { found: String },

    #[error("schema_version {0:?} is not semver")]
    UnparsableVersion(String),

    #[error("invariant violated: {0}")]
    Invariant(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum Mode {
    Dry,
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum Status {
    Accepted,
    Rejected,
    Superseded,
    Executing,
    Executed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum Direction {
    Long,
    Short,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum OrderType {
    Market,
    Limit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum OrderReason {
    Entry,
    Exit,
    Increase,
    Reduce,
    Rebalance,
}

impl OrderReason {
    /// Whether this order may run while trading is paused.
    ///
    /// Only the two that reduce exposure. A pause means stop taking on risk,
    /// not stop being able to shed it.
    pub fn permitted_while_paused(self) -> bool {
        matches!(self, OrderReason::Exit | OrderReason::Reduce)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum WarningKind {
    DegenerateFeature,
    UnenforcedRule,
    InsufficientSample,
    ConstructorFallback,
    StaleInput,
    TurnoverCapped,
    Other,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Nav {
    #[serde(with = "rust_decimal::serde::str")]
    pub total: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub cash: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub gross_exposure: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub net_exposure: Decimal,
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub benchmark_beta: Option<Decimal>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub planner_version: String,
    pub feature_set_version: String,
    pub scoring_version: String,
    pub risk_model_version: String,
    pub ruleset_version: String,
    /// The constructor that actually produced the weights.
    pub constructor: String,
    /// The one that was asked for. Differs when a fallback fired.
    pub constructor_requested: String,
    #[serde(default)]
    pub model_id: Option<String>,
    pub inputs_hash: String,
    pub universe_size: u32,
}

impl Provenance {
    pub fn fell_back(&self) -> bool {
        self.constructor != self.constructor_requested
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub asset: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub weight: Decimal,
    pub direction: Direction,
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub conviction: Option<Decimal>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurrentPosition {
    pub asset: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub qty: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub weight: Decimal,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Order {
    pub asset: String,
    pub side: Side,
    /// Absolute base-asset quantity, always positive. Direction is `side`.
    #[serde(with = "rust_decimal::serde::str")]
    pub qty: Decimal,
    pub order_type: OrderType,
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub limit_price: Option<Decimal>,
    pub reason: OrderReason,
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub est_cost_bps: Option<Decimal>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskCheck {
    pub name: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub limit: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub value: Decimal,
    pub passed: bool,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskReport {
    pub passed: bool,
    pub checks: Vec<RiskCheck>,
    #[serde(default)]
    pub rejected_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetCost {
    pub asset: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub bps: Decimal,
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub spread_bps: Option<Decimal>,
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub impact_bps: Option<Decimal>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CostEstimate {
    #[serde(with = "rust_decimal::serde::str")]
    pub total_bps: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub total_quote: Decimal,
    #[serde(default)]
    pub per_asset: Vec<AssetCost>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Warning {
    pub kind: WarningKind,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub schema_version: String,
    pub plan_id: Uuid,
    pub run_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// The newest bar close this plan was allowed to see.
    #[serde(with = "time::serde::rfc3339")]
    pub as_of: OffsetDateTime,
    pub mode: Mode,
    pub status: Status,
    pub quote_currency: String,
    pub nav: Nav,
    pub provenance: Provenance,
    pub targets: Vec<Target>,
    pub current: Vec<CurrentPosition>,
    pub orders: Vec<Order>,
    pub risk_report: RiskReport,
    pub cost_estimate: CostEstimate,
    pub warnings: Vec<Warning>,
}

impl Plan {
    /// Parse and check. Fails closed on anything unrecognised.
    pub fn parse(json: &str) -> Result<Self, PlanError> {
        let plan: Plan = serde_json::from_str(json)?;
        plan.check_version()?;
        plan.check_invariants()?;
        Ok(plan)
    }

    fn check_version(&self) -> Result<(), PlanError> {
        let major = self
            .schema_version
            .split('.')
            .next()
            .and_then(|m| m.parse::<u32>().ok())
            .ok_or_else(|| PlanError::UnparsableVersion(self.schema_version.clone()))?;

        if major != SUPPORTED_MAJOR {
            return Err(PlanError::UnsupportedVersion {
                found: self.schema_version.clone(),
            });
        }
        Ok(())
    }

    /// Invariants the executor relies on, re-checked here.
    ///
    /// The schema encodes these too. They are checked again because this
    /// process is the one that can move money, and a guarantee that only holds
    /// if an upstream validator ran is not a guarantee.
    fn check_invariants(&self) -> Result<(), PlanError> {
        if self.status == Status::Rejected && !self.orders.is_empty() {
            return Err(PlanError::Invariant(format!(
                "rejected plan carries {} orders; rejection is whole, never partial",
                self.orders.len()
            )));
        }
        if self.status == Status::Accepted && !self.risk_report.passed {
            return Err(PlanError::Invariant(
                "accepted plan whose risk report did not pass".into(),
            ));
        }
        if self.risk_report.passed != self.risk_report.checks.iter().all(|c| c.passed) {
            return Err(PlanError::Invariant(
                "risk_report.passed disagrees with its own checks".into(),
            ));
        }
        for o in &self.orders {
            if o.qty <= Decimal::ZERO {
                return Err(PlanError::Invariant(format!(
                    "order for {} has non-positive qty {}; direction belongs to `side`",
                    o.asset, o.qty
                )));
            }
            if o.order_type == OrderType::Limit && o.limit_price.is_none() {
                return Err(PlanError::Invariant(format!(
                    "limit order for {} has no limit price",
                    o.asset
                )));
            }
        }
        Ok(())
    }

    /// Whether this plan is eligible for execution at all.
    ///
    /// A dry plan never is - that is what `dry` means, and enforcing it here
    /// rather than at the call site means no caller can forget.
    pub fn is_executable(&self) -> bool {
        self.mode == Mode::Live && self.status == Status::Accepted
    }

    /// Disclosures that must be surfaced before any number derived from this plan.
    pub fn disclosures(&self) -> &[Warning] {
        &self.warnings
    }
}
