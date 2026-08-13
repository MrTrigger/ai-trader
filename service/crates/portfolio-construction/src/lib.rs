//! Strategy-neutral, direction-quota-free portfolio construction.
//!
//! Alpha models decide which signed candidates clear their economic hurdle.
//! Preliminary sizing turns those candidates into absolute weight proposals.
//! The allocator only reduces proposals to satisfy maximum budgets and limits:
//! it never creates a side, fills unused budget, or redistributes a cap excess.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use time::Date;

const EPSILON: f64 = 1e-12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Long,
    Short,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SizingMethod {
    #[default]
    Equal,
    Conviction,
    InverseVolatility,
    EdgeVolatility,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RankingMethod {
    #[default]
    Edge,
    EdgeVolatility,
}

impl std::str::FromStr for RankingMethod {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "edge" => Ok(Self::Edge),
            "edge_volatility" | "edge-volatility" | "edge_vol" => Ok(Self::EdgeVolatility),
            other => Err(format!("unknown ranking method {other:?}")),
        }
    }
}

impl std::str::FromStr for SizingMethod {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "equal" => Ok(Self::Equal),
            "conviction" => Ok(Self::Conviction),
            "inverse_volatility" | "inverse-volatility" | "inverse_vol" => {
                Ok(Self::InverseVolatility)
            }
            "edge_volatility" | "edge-volatility" | "edge_vol" => Ok(Self::EdgeVolatility),
            other => Err(format!("unknown sizing method {other:?}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: String,
    pub direction: Direction,
    pub edge: f64,
    pub volatility: f64,
}

/// Fixed calibration anchors make preliminary weights independent of which
/// other candidates happen to be present. That is what prevents a normalizer
/// from silently turning a maximum portfolio budget into a spending quota.
#[derive(Debug, Clone, Copy)]
pub struct SizingConfig {
    pub method: SizingMethod,
    pub unit_abs_weight: f64,
    pub min_abs_weight: f64,
    pub max_abs_weight: f64,
    pub reference_edge: f64,
    pub reference_volatility: f64,
}

#[derive(Debug, Clone)]
pub struct Proposal {
    pub id: String,
    pub direction: Direction,
    pub proposed_abs_weight: f64,
    pub min_abs_weight: f64,
    pub max_abs_weight: f64,
}

/// Maximum exposure allowed for an allocation. `target_net` records the
/// direction layer's requested net when the budget came from `(G, N)`; it is
/// diagnostic context, not an instruction to manufacture exposure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Budget {
    pub max_gross: f64,
    pub max_long: f64,
    pub max_short: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_net: Option<f64>,
}

impl Budget {
    /// A direction-free gross ceiling. Either sleeve may use the available
    /// gross, but the two sleeves together may not exceed `max_gross`.
    pub fn gross_only(max_gross: f64) -> Result<Self, String> {
        let budget = Self {
            max_gross,
            max_long: max_gross,
            max_short: max_gross,
            target_net: None,
        };
        budget.validate()?;
        Ok(budget)
    }

    /// Convert gross and net targets into independent maximum sleeve budgets:
    /// `long=(gross+net)/2`, `short=(gross-net)/2`.
    pub fn from_gross_net(gross: f64, net: f64) -> Result<Self, String> {
        if !net.is_finite() || net.abs() > gross + EPSILON {
            return Err("net target must be finite and satisfy |net| <= gross".into());
        }
        let budget = Self {
            max_gross: gross,
            max_long: (gross + net) / 2.0,
            max_short: (gross - net) / 2.0,
            target_net: Some(net),
        };
        budget.validate()?;
        Ok(budget)
    }

    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("maximum gross", self.max_gross),
            ("maximum long", self.max_long),
            ("maximum short", self.max_short),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(format!("{name} must be finite and non-negative"));
            }
        }
        if self.max_long > self.max_gross + EPSILON || self.max_short > self.max_gross + EPSILON {
            return Err("each sleeve maximum must not exceed maximum gross".into());
        }
        if let Some(target) = self.target_net {
            if !target.is_finite() || target.abs() > self.max_gross + EPSILON {
                return Err("net target must be finite and satisfy |net| <= gross".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrendSide {
    Long,
    Short,
    #[default]
    Neutral,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketRegime {
    StrongDown,
    Down,
    #[default]
    Neutral,
    Up,
    StrongUp,
}

/// Slow, generic market-direction policy. The caller owns market feature
/// calculation; this shared type only turns a bounded score and volatility
/// estimate into stateful maximum exposure budgets.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DirectionConfig {
    pub enter_threshold: f64,
    pub exit_threshold: f64,
    pub strong_threshold: f64,
    pub neutral_gross: f64,
    pub trend_gross: f64,
    pub strong_gross: f64,
    pub trend_abs_net: f64,
    pub strong_abs_net: f64,
    pub annual_volatility_target: f64,
    pub max_change_per_session: f64,
}

impl DirectionConfig {
    /// Conservative, symmetric starting policy. These values are a declared
    /// baseline for ablation, not fitted evidence.
    pub fn baseline(max_gross: f64) -> Result<Self, String> {
        let config = Self {
            enter_threshold: 0.4,
            exit_threshold: 0.0,
            strong_threshold: 0.8,
            neutral_gross: 0.30 * max_gross,
            trend_gross: 0.70 * max_gross,
            strong_gross: max_gross,
            trend_abs_net: 0.35 * max_gross,
            strong_abs_net: 0.50 * max_gross,
            annual_volatility_target: 0.18,
            max_change_per_session: 0.05 * max_gross,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("enter threshold", self.enter_threshold),
            ("exit threshold", self.exit_threshold),
            ("strong threshold", self.strong_threshold),
            ("neutral gross", self.neutral_gross),
            ("trend gross", self.trend_gross),
            ("strong gross", self.strong_gross),
            ("trend absolute net", self.trend_abs_net),
            ("strong absolute net", self.strong_abs_net),
            ("annual volatility target", self.annual_volatility_target),
            ("maximum session change", self.max_change_per_session),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(format!("{name} must be finite and non-negative"));
            }
        }
        if self.exit_threshold >= self.enter_threshold
            || self.enter_threshold > self.strong_threshold
            || self.strong_threshold > 1.0
        {
            return Err("direction thresholds must satisfy exit < enter <= strong <= 1".into());
        }
        if self.neutral_gross > self.trend_gross + EPSILON
            || self.trend_gross > self.strong_gross + EPSILON
        {
            return Err("direction gross budgets must be non-decreasing by strength".into());
        }
        if self.trend_abs_net > self.trend_gross + EPSILON
            || self.strong_abs_net > self.strong_gross + EPSILON
            || self.trend_abs_net > self.strong_abs_net + EPSILON
        {
            return Err("direction net budgets are inconsistent with gross budgets".into());
        }
        if self.annual_volatility_target <= 0.0 {
            return Err("annual volatility target must be positive".into());
        }
        if self.strong_gross > 0.0 && self.max_change_per_session <= 0.0 {
            return Err("maximum session change must be positive for a non-zero policy".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct DirectionState {
    pub side: TrendSide,
    pub current_gross: f64,
    pub current_net: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DirectionDecision {
    pub regime: MarketRegime,
    pub score: f64,
    pub annualised_volatility: f64,
    pub volatility_scale: f64,
    pub desired_gross: f64,
    pub desired_net: f64,
    pub budget: Budget,
}

impl DirectionState {
    /// Advance exactly one market session. Exposure reductions caused by the
    /// regime/volatility ceiling apply immediately; increases and net-direction
    /// changes are rate-limited to avoid portfolio-wide churn on regime flicker.
    pub fn update(
        &mut self,
        score: f64,
        annualised_volatility: f64,
        config: &DirectionConfig,
    ) -> Result<DirectionDecision, String> {
        config.validate()?;
        if !score.is_finite() || !(-1.0..=1.0).contains(&score) {
            return Err("direction score must be finite and in [-1, 1]".into());
        }
        if !annualised_volatility.is_finite() || annualised_volatility < 0.0 {
            return Err("annualised volatility must be finite and non-negative".into());
        }

        self.side = match self.side {
            TrendSide::Neutral if score >= config.enter_threshold => TrendSide::Long,
            TrendSide::Neutral if score <= -config.enter_threshold => TrendSide::Short,
            TrendSide::Long if score <= -config.enter_threshold => TrendSide::Short,
            TrendSide::Long if score <= config.exit_threshold => TrendSide::Neutral,
            TrendSide::Short if score >= config.enter_threshold => TrendSide::Long,
            TrendSide::Short if score >= -config.exit_threshold => TrendSide::Neutral,
            side => side,
        };

        let regime = match (self.side, score.abs() >= config.strong_threshold) {
            (TrendSide::Long, true) => MarketRegime::StrongUp,
            (TrendSide::Long, false) => MarketRegime::Up,
            (TrendSide::Short, true) => MarketRegime::StrongDown,
            (TrendSide::Short, false) => MarketRegime::Down,
            (TrendSide::Neutral, _) => MarketRegime::Neutral,
        };
        let (regime_gross, regime_abs_net) = match regime {
            MarketRegime::StrongDown | MarketRegime::StrongUp => {
                (config.strong_gross, config.strong_abs_net)
            }
            MarketRegime::Down | MarketRegime::Up => (config.trend_gross, config.trend_abs_net),
            MarketRegime::Neutral => (config.neutral_gross, 0.0),
        };
        let volatility_scale = if annualised_volatility <= EPSILON {
            1.0
        } else {
            (config.annual_volatility_target / annualised_volatility).min(1.0)
        };
        let desired_gross = regime_gross * volatility_scale;
        let desired_net = match self.side {
            TrendSide::Long => regime_abs_net * volatility_scale,
            TrendSide::Short => -regime_abs_net * volatility_scale,
            TrendSide::Neutral => 0.0,
        };

        self.current_gross = if desired_gross < self.current_gross {
            desired_gross
        } else {
            approach(
                self.current_gross,
                desired_gross,
                config.max_change_per_session,
            )
        };
        let net_at_current_gross = if desired_gross <= EPSILON {
            0.0
        } else {
            desired_net / desired_gross * self.current_gross
        };
        // A confirmed neutral state or side reversal is a risk-reducing event:
        // never retain exposure in the now-invalid direction merely to obey
        // the ramp. New exposure may ramp from zero on this session, but the
        // resulting budget is always sign-consistent with the decision.
        if net_at_current_gross.abs() <= EPSILON {
            self.current_net = 0.0;
        } else {
            if self.current_net * net_at_current_gross < 0.0 {
                self.current_net = 0.0;
            }
            self.current_net = approach(
                self.current_net,
                net_at_current_gross,
                config.max_change_per_session,
            )
            .clamp(-self.current_gross, self.current_gross);
        }
        let budget = Budget::from_gross_net(self.current_gross, self.current_net)?;
        Ok(DirectionDecision {
            regime,
            score,
            annualised_volatility,
            volatility_scale,
            desired_gross,
            desired_net,
            budget,
        })
    }
}

fn approach(current: f64, target: f64, maximum_change: f64) -> f64 {
    if target > current {
        (current + maximum_change).min(target)
    } else {
        (current - maximum_change).max(target)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AllocationDiagnostics {
    pub budget: Budget,
    pub proposed_long: f64,
    pub proposed_short: f64,
    pub proposed_gross: f64,
    pub realised_long: f64,
    pub realised_short: f64,
    pub realised_gross: f64,
    pub realised_net: f64,
    pub unused_long: f64,
    pub unused_short: f64,
    pub unused_gross: f64,
    #[serde(default)]
    pub capped_positions: Vec<String>,
    #[serde(default)]
    pub dropped_below_minimum: Vec<String>,
    #[serde(default)]
    pub capped_groups: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Allocation {
    pub weights: BTreeMap<String, f64>,
    pub diagnostics: AllocationDiagnostics,
}

/// Combine equal-capital rebalance phases on the calendar rather than on the
/// period index. Each input is one phase's daily `(session, NAV)` series; the
/// output is the combined book's daily NAV over the sessions every phase covers.
///
/// Averaging phase returns by period index treats phase `j`'s period `k` as
/// simultaneous with phase `j+1`'s period `k`, but those periods span different
/// sessions. The average then behaves like a moving average of overlapping
/// windows: it suppresses variance while preserving the mean, and any
/// annualisation of the smoothed series reports a Sharpe no phase earned. Keying
/// on the session date removes that smoothing entirely — when every phase holds
/// the same book the combined series is that book's own path.
///
/// Capital is split equally at the first common session, so every phase is
/// normalised to NAV 1.0 there and the combined book is thereafter held without
/// transfers between phases. Sessions before that (a phase's own ramp-up) and
/// after the earliest-ending phase are outside the window in which all phases
/// are invested, so they are excluded; inside the window the phases' session
/// grids must agree exactly, and a gap is an error rather than a silent
/// realignment of one phase's NAV onto another phase's dates.
pub fn equal_weight_phase_daily_navs(
    phases: &[Vec<(Date, f64)>],
) -> Result<Vec<(Date, f64)>, String> {
    if phases.is_empty() || phases.iter().any(Vec::is_empty) {
        return Err("rebalance phases must be non-empty".into());
    }
    for phase in phases {
        if phase.iter().any(|(_, nav)| !nav.is_finite() || *nav <= 0.0) {
            return Err("rebalance phase NAVs must be finite and positive".into());
        }
        if phase.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
            return Err("rebalance phase marks must be strictly ordered by session".into());
        }
    }
    let start = phases
        .iter()
        .map(|phase| phase[0].0)
        .max()
        .expect("non-empty phases");
    let end = phases
        .iter()
        .map(|phase| phase[phase.len() - 1].0)
        .min()
        .expect("non-empty phases");
    if start > end {
        return Err("rebalance phases have no overlapping session window".into());
    }
    let windows = phases
        .iter()
        .map(|phase| {
            phase
                .iter()
                .filter(|(date, _)| *date >= start && *date <= end)
                .copied()
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let reference = &windows[0];
    for (index, window) in windows.iter().enumerate().skip(1) {
        if window.len() != reference.len() {
            return Err(format!(
                "phase {index} covers {} sessions between {start} and {end}, phase 0 covers {}",
                window.len(),
                reference.len()
            ));
        }
        if let Some((_, (date, _))) = window
            .iter()
            .zip(reference)
            .enumerate()
            .find(|(_, (mine, theirs))| mine.0 != theirs.0)
        {
            return Err(format!("phase {index} has no {} session", date.0));
        }
    }
    let base = windows.iter().map(|window| window[0].1).collect::<Vec<_>>();
    Ok((0..reference.len())
        .map(|index| {
            let nav = windows
                .iter()
                .zip(&base)
                .map(|(window, opening)| window[index].1 / opening)
                .sum::<f64>()
                / windows.len() as f64;
            (reference[index].0, nav)
        })
        .collect())
}

/// Combine equal-capital rebalance phases without selecting a favourable
/// calendar alignment. Each input is a complete return series for one phase;
/// only the common complete prefix is used, so a partial terminal holding can
/// never receive extra weight.
#[deprecated(
    note = "averages phases by period index, which smooths overlapping holding windows and inflates annualised Sharpe; use equal_weight_phase_daily_navs. Retained only to summarise reports predating daily NAV marks."
)]
pub fn equal_weight_phase_returns(phases: &[Vec<f64>]) -> Result<Vec<f64>, String> {
    if phases.is_empty() || phases.iter().any(Vec::is_empty) {
        return Err("rebalance phases must be non-empty".into());
    }
    if phases
        .iter()
        .flatten()
        .any(|value| !value.is_finite() || *value <= -1.0)
    {
        return Err("rebalance phase returns must be finite and greater than -1".into());
    }
    let common = phases.iter().map(Vec::len).min().expect("non-empty phases");
    Ok((0..common)
        .map(|index| phases.iter().map(|phase| phase[index]).sum::<f64>() / phases.len() as f64)
        .collect())
}

/// Return candidate identities from strongest to weakest. Direction never
/// enters the score, so a side cannot receive a quota or bonus.
pub fn ranked_ids(candidates: &[Candidate], method: RankingMethod) -> Result<Vec<String>, String> {
    validate_candidates(candidates)?;
    let mut ranked = candidates.iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        let score = |candidate: &Candidate| match method {
            RankingMethod::Edge => candidate.edge,
            RankingMethod::EdgeVolatility => candidate.edge / candidate.volatility,
        };
        score(right)
            .total_cmp(&score(left))
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(ranked
        .into_iter()
        .map(|candidate| candidate.id.clone())
        .collect())
}

/// Retain economically eligible incumbents inside a wider rank buffer, then
/// fill remaining slots from the strongest candidates. Direction changes are
/// never buffered: an incumbent is retained only while its newly predicted
/// best side matches the held side. No side receives a quota.
pub fn buffered_ranked_ids(
    candidates: &[Candidate],
    method: RankingMethod,
    incumbents: &BTreeMap<String, Direction>,
    max_positions: usize,
    retention_rank: usize,
) -> Result<Vec<String>, String> {
    if max_positions == 0 || retention_rank < max_positions {
        return Err("retention rank must be at least the positive position limit".into());
    }
    let ranked = ranked_ids(candidates, method)?;
    let by_id = candidates
        .iter()
        .map(|candidate| (candidate.id.as_str(), candidate))
        .collect::<BTreeMap<_, _>>();
    let mut selected = ranked
        .iter()
        .take(retention_rank)
        .filter(|id| {
            incumbents.get(*id).is_some_and(|held_side| {
                by_id
                    .get(id.as_str())
                    .is_some_and(|candidate| candidate.direction == *held_side)
            })
        })
        .take(max_positions)
        .cloned()
        .collect::<BTreeSet<_>>();
    for id in &ranked {
        if selected.len() == max_positions {
            break;
        }
        selected.insert(id.clone());
    }
    Ok(ranked
        .into_iter()
        .filter(|id| selected.contains(id))
        .collect())
}

/// Produce absolute weight proposals using fixed economic/risk anchors. No
/// cross-sectional sum appears here, so adding or removing another candidate
/// cannot make an existing proposal larger.
pub fn propose(candidates: &[Candidate], config: &SizingConfig) -> Result<Vec<Proposal>, String> {
    validate_candidates(candidates)?;
    validate_sizing(config)?;
    candidates
        .iter()
        .map(|candidate| {
            let multiplier = match config.method {
                SizingMethod::Equal => 1.0,
                SizingMethod::Conviction => candidate.edge / config.reference_edge,
                SizingMethod::InverseVolatility => {
                    config.reference_volatility / candidate.volatility
                }
                SizingMethod::EdgeVolatility => {
                    (candidate.edge / config.reference_edge)
                        * (config.reference_volatility / candidate.volatility)
                }
            };
            let proposed_abs_weight = config.unit_abs_weight * multiplier;
            if !proposed_abs_weight.is_finite() || proposed_abs_weight < 0.0 {
                return Err(format!("{} has invalid proposed weight", candidate.id));
            }
            Ok(Proposal {
                id: candidate.id.clone(),
                direction: candidate.direction,
                proposed_abs_weight,
                min_abs_weight: config.min_abs_weight,
                max_abs_weight: config.max_abs_weight,
            })
        })
        .collect()
}

/// Apply caps and maximum budgets by scaling down only. Cap excess and weights
/// dropped below their economic minimum remain unallocated.
pub fn allocate(proposals: &[Proposal], budget: Budget) -> Result<Allocation, String> {
    budget.validate()?;
    validate_proposals(proposals)?;

    let proposed_long = normalise_zero(
        proposals
            .iter()
            .filter(|proposal| proposal.direction == Direction::Long)
            .map(|proposal| proposal.proposed_abs_weight)
            .sum::<f64>(),
    );
    let proposed_short = normalise_zero(
        proposals
            .iter()
            .filter(|proposal| proposal.direction == Direction::Short)
            .map(|proposal| proposal.proposed_abs_weight)
            .sum::<f64>(),
    );
    let mut capped_positions = Vec::new();
    let capped = proposals
        .iter()
        .map(|proposal| {
            if proposal.proposed_abs_weight > proposal.max_abs_weight + EPSILON {
                capped_positions.push(proposal.id.clone());
            }
            (
                proposal,
                proposal.proposed_abs_weight.min(proposal.max_abs_weight),
            )
        })
        .collect::<Vec<_>>();
    let capped_long = side_total(&capped, Direction::Long);
    let capped_short = side_total(&capped, Direction::Short);
    let long_scale = scale_down(capped_long, budget.max_long);
    let short_scale = scale_down(capped_short, budget.max_short);
    let sleeve_scaled_gross = capped_long * long_scale + capped_short * short_scale;
    let gross_scale = scale_down(sleeve_scaled_gross, budget.max_gross);

    let mut weights = BTreeMap::new();
    let mut dropped_below_minimum = Vec::new();
    for (proposal, capped_weight) in capped {
        let side_scale = match proposal.direction {
            Direction::Long => long_scale,
            Direction::Short => short_scale,
        };
        let absolute = capped_weight * side_scale * gross_scale;
        if absolute + EPSILON < proposal.min_abs_weight {
            dropped_below_minimum.push(proposal.id.clone());
            continue;
        }
        if absolute <= EPSILON {
            continue;
        }
        let sign = match proposal.direction {
            Direction::Long => 1.0,
            Direction::Short => -1.0,
        };
        weights.insert(proposal.id.clone(), sign * absolute);
    }

    let realised_long = weights
        .values()
        .copied()
        .filter(|weight| *weight > 0.0)
        .sum();
    let realised_short = -weights
        .values()
        .copied()
        .filter(|weight| *weight < 0.0)
        .sum::<f64>();
    let realised_gross = realised_long + realised_short;
    let diagnostics = AllocationDiagnostics {
        budget,
        proposed_long,
        proposed_short,
        proposed_gross: proposed_long + proposed_short,
        realised_long,
        realised_short,
        realised_gross,
        realised_net: realised_long - realised_short,
        unused_long: (budget.max_long - realised_long).max(0.0),
        unused_short: (budget.max_short - realised_short).max(0.0),
        unused_gross: (budget.max_gross - realised_gross).max(0.0),
        capped_positions,
        dropped_below_minimum,
        capped_groups: Vec::new(),
    };
    Ok(Allocation {
        weights,
        diagnostics,
    })
}

/// Apply one common gross ceiling to arbitrary groups after ordinary caps and
/// budgets. Binding groups are scaled down proportionally; freed exposure is
/// left in cash and never redistributed to another group or side.
pub fn allocate_with_group_cap(
    proposals: &[Proposal],
    budget: Budget,
    groups: &BTreeMap<String, String>,
    max_group_gross: f64,
) -> Result<Allocation, String> {
    if !max_group_gross.is_finite() || max_group_gross <= 0.0 {
        return Err("maximum group gross must be finite and positive".into());
    }
    if proposals.iter().any(|proposal| {
        groups
            .get(&proposal.id)
            .is_none_or(|group| group.trim().is_empty())
    }) {
        return Err("every proposal must have a non-empty group".into());
    }
    let mut allocation = allocate(proposals, budget)?;
    let mut totals = BTreeMap::<&str, f64>::new();
    for (id, weight) in &allocation.weights {
        *totals.entry(groups[id].as_str()).or_default() += weight.abs();
    }
    let scales = totals
        .iter()
        .map(|(group, total)| (*group, scale_down(*total, max_group_gross)))
        .collect::<BTreeMap<_, _>>();
    allocation.diagnostics.capped_groups = totals
        .iter()
        .filter(|(_, total)| **total > max_group_gross + EPSILON)
        .map(|(group, _)| (*group).to_owned())
        .collect();
    for (id, weight) in &mut allocation.weights {
        *weight *= scales[groups[id].as_str()];
    }
    // Group scaling can push a position that survived the ordinary
    // per-name minimum below it again. Drop it to cash rather than carry a
    // sub-minimum sliver; the freed weight is not redistributed (scale-down
    // only), matching the ordinary allocate() contract.
    let min_abs_weight_by_id: BTreeMap<&str, f64> = proposals
        .iter()
        .map(|proposal| (proposal.id.as_str(), proposal.min_abs_weight))
        .collect();
    let mut dropped_by_group_cap = Vec::new();
    allocation.weights.retain(|id, weight| {
        let minimum = min_abs_weight_by_id
            .get(id.as_str())
            .copied()
            .unwrap_or(0.0);
        if weight.abs() + EPSILON < minimum {
            dropped_by_group_cap.push(id.clone());
            false
        } else {
            true
        }
    });
    allocation
        .diagnostics
        .dropped_below_minimum
        .extend(dropped_by_group_cap);
    let realised_long = allocation
        .weights
        .values()
        .copied()
        .filter(|weight| *weight > 0.0)
        .sum::<f64>();
    let realised_short = -allocation
        .weights
        .values()
        .copied()
        .filter(|weight| *weight < 0.0)
        .sum::<f64>();
    let realised_gross = realised_long + realised_short;
    allocation.diagnostics.realised_long = realised_long;
    allocation.diagnostics.realised_short = realised_short;
    allocation.diagnostics.realised_gross = realised_gross;
    allocation.diagnostics.realised_net = realised_long - realised_short;
    allocation.diagnostics.unused_long = (budget.max_long - realised_long).max(0.0);
    allocation.diagnostics.unused_short = (budget.max_short - realised_short).max(0.0);
    allocation.diagnostics.unused_gross = (budget.max_gross - realised_gross).max(0.0);
    Ok(allocation)
}

fn side_total(capped: &[(&Proposal, f64)], direction: Direction) -> f64 {
    capped
        .iter()
        .filter(|(proposal, _)| proposal.direction == direction)
        .map(|(_, weight)| *weight)
        .sum()
}

fn scale_down(value: f64, maximum: f64) -> f64 {
    if value <= EPSILON || value <= maximum {
        1.0
    } else {
        maximum / value
    }
}

fn normalise_zero(value: f64) -> f64 {
    if value.abs() <= EPSILON {
        0.0
    } else {
        value
    }
}

fn validate_sizing(config: &SizingConfig) -> Result<(), String> {
    for (name, value) in [
        ("unit absolute weight", config.unit_abs_weight),
        ("minimum absolute weight", config.min_abs_weight),
        ("maximum absolute weight", config.max_abs_weight),
        ("reference edge", config.reference_edge),
        ("reference volatility", config.reference_volatility),
    ] {
        if !value.is_finite() || value < 0.0 {
            return Err(format!("{name} must be finite and non-negative"));
        }
    }
    if config.unit_abs_weight <= 0.0
        || config.max_abs_weight <= 0.0
        || config.reference_edge <= 0.0
        || config.reference_volatility <= 0.0
    {
        return Err("unit, maximum, and sizing references must be positive".into());
    }
    if config.min_abs_weight > config.max_abs_weight {
        return Err("minimum absolute weight must not exceed maximum".into());
    }
    Ok(())
}

fn validate_candidates(candidates: &[Candidate]) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    for candidate in candidates {
        if candidate.id.trim().is_empty() || !ids.insert(&candidate.id) {
            return Err("candidate ids must be non-empty and unique".into());
        }
        if !candidate.edge.is_finite() || candidate.edge <= 0.0 {
            return Err(format!("{} has invalid edge", candidate.id));
        }
        if !candidate.volatility.is_finite() || candidate.volatility <= 0.0 {
            return Err(format!("{} has invalid volatility", candidate.id));
        }
    }
    Ok(())
}

fn validate_proposals(proposals: &[Proposal]) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    for proposal in proposals {
        if proposal.id.trim().is_empty() || !ids.insert(&proposal.id) {
            return Err("proposal ids must be non-empty and unique".into());
        }
        for (name, value) in [
            ("proposed", proposal.proposed_abs_weight),
            ("minimum", proposal.min_abs_weight),
            ("maximum", proposal.max_abs_weight),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(format!("{} has invalid {name} weight", proposal.id));
            }
        }
        if proposal.max_abs_weight <= 0.0 || proposal.min_abs_weight > proposal.max_abs_weight {
            return Err(format!("{} has inconsistent weight limits", proposal.id));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, direction: Direction, edge: f64, volatility: f64) -> Candidate {
        Candidate {
            id: id.into(),
            direction,
            edge,
            volatility,
        }
    }

    fn proposal(id: &str, direction: Direction, proposed: f64, cap: f64) -> Proposal {
        Proposal {
            id: id.into(),
            direction,
            proposed_abs_weight: proposed,
            min_abs_weight: 0.0,
            max_abs_weight: cap,
        }
    }

    fn immediate_direction_config() -> DirectionConfig {
        DirectionConfig {
            max_change_per_session: 1.0,
            ..DirectionConfig::baseline(1.0).unwrap()
        }
    }

    #[test]
    fn direction_hysteresis_ignores_weak_flicker_and_then_exits() {
        let config = immediate_direction_config();
        let mut state = DirectionState::default();
        assert_eq!(
            state.update(0.4, 0.10, &config).unwrap().regime,
            MarketRegime::Up
        );
        assert_eq!(
            state.update(0.2, 0.10, &config).unwrap().regime,
            MarketRegime::Up
        );
        let exited = state.update(0.0, 0.10, &config).unwrap();
        assert_eq!(exited.regime, MarketRegime::Neutral);
        assert_eq!(exited.budget.target_net, Some(0.0));
        assert_eq!(
            state.update(0.2, 0.10, &config).unwrap().regime,
            MarketRegime::Neutral
        );
    }

    #[test]
    fn strong_down_budget_is_symmetric_but_does_not_force_positions() {
        let config = immediate_direction_config();
        let mut state = DirectionState::default();
        let decision = state.update(-1.0, 0.10, &config).unwrap();
        assert_eq!(decision.regime, MarketRegime::StrongDown);
        assert!((decision.budget.max_long - 0.25).abs() < EPSILON);
        assert!((decision.budget.max_short - 0.75).abs() < EPSILON);
        assert_eq!(decision.budget.target_net, Some(-0.5));

        let allocation = allocate(
            &[proposal("only_long", Direction::Long, 0.10, 0.10)],
            decision.budget,
        )
        .unwrap();
        assert_eq!(allocation.weights["only_long"], 0.10);
        assert!((allocation.diagnostics.unused_short - 0.75).abs() < EPSILON);
    }

    #[test]
    fn volatility_ceiling_only_scales_the_regime_budget_down() {
        let config = immediate_direction_config();
        let mut state = DirectionState::default();
        let decision = state.update(1.0, 0.36, &config).unwrap();
        assert!((decision.volatility_scale - 0.5).abs() < EPSILON);
        assert!((decision.desired_gross - 0.5).abs() < EPSILON);
        assert!((decision.budget.max_gross - 0.5).abs() < EPSILON);
        assert!((decision.budget.target_net.unwrap() - 0.25).abs() < EPSILON);
    }

    #[test]
    fn exposure_ramps_up_without_changing_the_requested_gross_net_ratio() {
        let config = DirectionConfig::baseline(1.0).unwrap();
        let mut state = DirectionState::default();
        let first = state.update(1.0, 0.10, &config).unwrap();
        assert!((first.budget.max_gross - 0.05).abs() < EPSILON);
        assert!((first.budget.target_net.unwrap() - 0.025).abs() < EPSILON);
        let second = state.update(1.0, 0.10, &config).unwrap();
        assert!((second.budget.max_gross - 0.10).abs() < EPSILON);
        assert!((second.budget.target_net.unwrap() - 0.05).abs() < EPSILON);
    }

    #[test]
    fn confirmed_reversal_never_carries_the_old_net_direction() {
        let config = DirectionConfig::baseline(1.0).unwrap();
        let mut state = DirectionState::default();
        for _ in 0..10 {
            state.update(-1.0, 0.10, &config).unwrap();
        }
        assert!(state.current_net < 0.0);
        let reversed = state.update(1.0, 0.10, &config).unwrap();
        assert_eq!(reversed.regime, MarketRegime::StrongUp);
        assert!(reversed.budget.target_net.unwrap() >= 0.0);
        assert!(reversed.budget.max_long >= reversed.budget.max_short);
    }

    #[test]
    fn direction_policy_rejects_invalid_thresholds_and_scores() {
        let mut config = DirectionConfig::baseline(1.0).unwrap();
        config.exit_threshold = config.enter_threshold;
        assert!(config.validate().is_err());
        assert!(DirectionState::default()
            .update(1.1, 0.1, &DirectionConfig::baseline(1.0).unwrap())
            .is_err());
    }

    #[test]
    fn gross_net_decomposition_is_exact_and_rejects_impossible_net() {
        let budget = Budget::from_gross_net(1.0, -0.5).unwrap();
        assert_eq!(budget.max_long, 0.25);
        assert_eq!(budget.max_short, 0.75);
        assert_eq!(budget.target_net, Some(-0.5));
        assert!(Budget::from_gross_net(0.5, 0.6).is_err());
    }

    #[test]
    fn one_sided_candidates_stay_one_sided_and_do_not_fill_budget() {
        let allocation = allocate(
            &[
                proposal("a", Direction::Long, 0.05, 0.05),
                proposal("b", Direction::Long, 0.05, 0.05),
            ],
            Budget::gross_only(1.0).unwrap(),
        )
        .unwrap();
        assert!(allocation.weights.values().all(|weight| *weight > 0.0));
        assert!((allocation.diagnostics.realised_gross - 0.1).abs() < EPSILON);
        assert!((allocation.diagnostics.unused_gross - 0.9).abs() < EPSILON);
    }

    #[test]
    fn rank_buffer_retains_eligible_incumbents_without_side_quotas() {
        let candidates = (0..6)
            .map(|index| {
                candidate(
                    &format!("c{index}"),
                    if index == 4 {
                        Direction::Short
                    } else {
                        Direction::Long
                    },
                    1.0 - index as f64 / 10.0,
                    0.02,
                )
            })
            .collect::<Vec<_>>();
        let incumbents = BTreeMap::from([
            ("c3".into(), Direction::Long),
            ("c4".into(), Direction::Long),
        ]);
        let selected =
            buffered_ranked_ids(&candidates, RankingMethod::Edge, &incumbents, 3, 5).unwrap();
        assert_eq!(selected, ["c0", "c1", "c3"]);
        assert!(
            !selected.contains(&"c4".into()),
            "side flips are never buffered"
        );
    }

    #[test]
    fn cap_excess_is_not_redistributed() {
        let allocation = allocate(
            &[
                proposal("a", Direction::Long, 0.8, 0.4),
                proposal("b", Direction::Short, 0.1, 0.4),
                proposal("c", Direction::Long, 0.1, 0.4),
            ],
            Budget::gross_only(0.9).unwrap(),
        )
        .unwrap();
        assert!((allocation.weights["a"] - 0.4).abs() < EPSILON);
        assert!((allocation.weights["b"] + 0.1).abs() < EPSILON);
        assert!((allocation.weights["c"] - 0.1).abs() < EPSILON);
        assert!((allocation.diagnostics.realised_gross - 0.6).abs() < EPSILON);
        assert_eq!(allocation.diagnostics.capped_positions, ["a"]);
    }

    #[test]
    fn group_cap_drops_positions_scaled_below_minimum_instead_of_keeping_them_tiny() {
        // Both survive the ordinary per-name allocate() pass on their own
        // minimums (0.05 and 0.03). The sector cap then scales the whole
        // "one" group down by 3/7 (0.15 / 0.35), which pushes "b" under its
        // own minimum (0.05*3/7 ~= 0.0214 < 0.03) while leaving "a" above
        // (0.30*3/7 ~= 0.1286 > 0.05). "b" must be dropped to cash, not kept
        // as a sub-minimum sliver, and the dropped weight must not inflate
        // "a" (scale-down-only: no redistribution).
        let mut a = proposal("a", Direction::Long, 0.30, 0.30);
        a.min_abs_weight = 0.05;
        let mut b = proposal("b", Direction::Long, 0.05, 0.05);
        b.min_abs_weight = 0.03;
        let proposals = [a, b];
        let groups = BTreeMap::from([("a".into(), "one".into()), ("b".into(), "one".into())]);
        let allocation =
            allocate_with_group_cap(&proposals, Budget::gross_only(1.0).unwrap(), &groups, 0.15)
                .unwrap();
        let expected_a = 0.30 * 3.0 / 7.0;
        assert!(!allocation.weights.contains_key("b"));
        assert!((allocation.weights["a"] - expected_a).abs() < EPSILON);
        assert_eq!(allocation.diagnostics.dropped_below_minimum, ["b"]);
        assert!((allocation.diagnostics.realised_long - expected_a).abs() < EPSILON);
        assert!((allocation.diagnostics.realised_gross - expected_a).abs() < EPSILON);
    }

    #[test]
    fn group_cap_scales_only_the_binding_group_and_leaves_cash() {
        let proposals = [
            proposal("a", Direction::Long, 0.20, 0.20),
            proposal("b", Direction::Long, 0.20, 0.20),
            proposal("c", Direction::Short, 0.20, 0.20),
        ];
        let groups = BTreeMap::from([
            ("a".into(), "one".into()),
            ("b".into(), "one".into()),
            ("c".into(), "two".into()),
        ]);
        let allocation =
            allocate_with_group_cap(&proposals, Budget::gross_only(1.0).unwrap(), &groups, 0.25)
                .unwrap();
        assert!((allocation.weights["a"] - 0.125).abs() < EPSILON);
        assert!((allocation.weights["b"] - 0.125).abs() < EPSILON);
        assert!((allocation.weights["c"] + 0.20).abs() < EPSILON);
        assert_eq!(allocation.diagnostics.capped_groups, ["one"]);
        assert!((allocation.diagnostics.realised_gross - 0.45).abs() < EPSILON);
        assert!((allocation.diagnostics.unused_gross - 0.55).abs() < EPSILON);
    }

    #[test]
    fn adding_a_capped_candidate_cannot_increase_existing_weights() {
        let budget = Budget::gross_only(1.0).unwrap();
        let base = allocate(
            &[
                proposal("a", Direction::Long, 0.1, 0.1),
                proposal("b", Direction::Short, 0.1, 0.1),
            ],
            budget,
        )
        .unwrap();
        let expanded = allocate(
            &[
                proposal("a", Direction::Long, 0.1, 0.1),
                proposal("b", Direction::Short, 0.1, 0.1),
                proposal("outlier", Direction::Long, 10.0, 0.2),
            ],
            budget,
        )
        .unwrap();
        assert!(expanded.weights["a"] <= base.weights["a"] + EPSILON);
        assert!(expanded.weights["b"].abs() <= base.weights["b"].abs() + EPSILON);
    }

    #[test]
    fn removing_an_under_budget_candidate_does_not_force_others_up() {
        let budget = Budget::gross_only(1.0).unwrap();
        let full = allocate(
            &[
                proposal("a", Direction::Long, 0.1, 0.2),
                proposal("b", Direction::Short, 0.1, 0.2),
            ],
            budget,
        )
        .unwrap();
        let reduced = allocate(&[proposal("a", Direction::Long, 0.1, 0.2)], budget).unwrap();
        assert_eq!(full.weights["a"], reduced.weights["a"]);
        assert!(reduced.diagnostics.unused_gross > full.diagnostics.unused_gross);
    }

    #[test]
    fn side_budget_scales_down_without_flipping_or_manufacturing_direction() {
        let allocation = allocate(
            &[
                proposal("long", Direction::Long, 0.5, 0.5),
                proposal("short", Direction::Short, 0.5, 0.5),
            ],
            Budget::from_gross_net(0.8, 0.4).unwrap(),
        )
        .unwrap();
        assert!((allocation.weights["long"] - 0.5).abs() < EPSILON);
        assert!((allocation.weights["short"] + 0.2).abs() < EPSILON);
        assert!((allocation.diagnostics.realised_net - 0.3).abs() < EPSILON);
        assert!((allocation.diagnostics.unused_long - 0.1).abs() < EPSILON);
    }

    #[test]
    fn post_budget_weight_below_minimum_is_dropped_without_renormalizing() {
        let mut small = proposal("small", Direction::Long, 0.02, 0.1);
        small.min_abs_weight = 0.015;
        let allocation = allocate(
            &[small, proposal("large", Direction::Long, 0.08, 0.1)],
            Budget::gross_only(0.05).unwrap(),
        )
        .unwrap();
        assert!(!allocation.weights.contains_key("small"));
        assert!((allocation.weights["large"] - 0.04).abs() < EPSILON);
        assert!((allocation.diagnostics.unused_gross - 0.01).abs() < EPSILON);
        assert_eq!(allocation.diagnostics.dropped_below_minimum, ["small"]);
    }

    #[test]
    fn fixed_anchors_keep_proposals_independent_of_the_candidate_set() {
        let config = SizingConfig {
            method: SizingMethod::EdgeVolatility,
            unit_abs_weight: 0.05,
            min_abs_weight: 0.0,
            max_abs_weight: 0.15,
            reference_edge: 0.01,
            reference_volatility: 0.02,
        };
        let a = candidate("a", Direction::Long, 0.02, 0.04);
        let alone = propose(std::slice::from_ref(&a), &config).unwrap();
        let together = propose(
            &[a, candidate("outlier", Direction::Short, 1.0, 0.001)],
            &config,
        )
        .unwrap();
        assert_eq!(
            alone[0].proposed_abs_weight,
            together[0].proposed_abs_weight
        );
    }

    #[test]
    fn risk_adjusted_ranking_has_no_direction_quota() {
        let candidates = [
            candidate("short", Direction::Short, 0.03, 0.03),
            candidate("long_low_risk", Direction::Long, 0.02, 0.01),
            candidate("long_high_risk", Direction::Long, 0.04, 0.08),
        ];
        assert_eq!(
            ranked_ids(&candidates, RankingMethod::EdgeVolatility).unwrap(),
            ["long_low_risk", "short", "long_high_risk"]
        );
    }

    #[test]
    #[allow(deprecated)]
    fn equal_weight_phases_use_only_the_common_complete_prefix() {
        let combined =
            equal_weight_phase_returns(&[vec![0.10, -0.05, 0.20], vec![0.00, 0.05]]).unwrap();
        assert_eq!(combined, [0.05, 0.0]);
    }

    /// A deterministic, mean-positive daily return path with enough dispersion
    /// that overlapping-window averaging visibly flattens it.
    fn daily_return_path(sessions: usize) -> Vec<f64> {
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        (0..sessions)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let uniform = (state >> 11) as f64 / (1_u64 << 53) as f64;
                0.0006 + 0.02 * (uniform - 0.5)
            })
            .collect()
    }

    fn session(index: usize) -> Date {
        // A synthetic contiguous session grid; only ordering and equality matter.
        Date::from_ordinal_date(2024, 1).expect("valid ordinal date")
            + time::Duration::days(index as i64)
    }

    /// Build one phase's daily `(date, nav)` series over `sessions[start..end]`,
    /// holding the common asset and paying `rebalance_cost` on every session
    /// where this phase rebalances.
    fn phase_marks(
        path: &[f64],
        first_session: usize,
        last_session: usize,
        offset: usize,
        cadence: usize,
        rebalance_cost: f64,
    ) -> Vec<(Date, f64)> {
        let mut nav = 1.0;
        (first_session..=last_session)
            .map(|index| {
                nav *= 1.0 + path[index];
                if index % cadence == offset % cadence {
                    nav *= 1.0 - rebalance_cost;
                }
                (session(index), nav)
            })
            .collect()
    }

    fn daily_returns(navs: &[(Date, f64)]) -> Vec<f64> {
        navs.windows(2)
            .map(|pair| pair[1].1 / pair[0].1 - 1.0)
            .collect()
    }

    fn annualised_sharpe(returns: &[f64], periods_per_year: f64) -> f64 {
        let average = returns.iter().sum::<f64>() / returns.len() as f64;
        let variance = returns
            .iter()
            .map(|value| (value - average).powi(2))
            .sum::<f64>()
            / returns.len() as f64;
        average * periods_per_year / (variance.sqrt() * periods_per_year.sqrt())
    }

    /// Period returns as the legacy index-averaging path sees them: each phase's
    /// own non-overlapping holding-period returns, taken off the same NAV path.
    fn phase_period_returns(navs: &[(Date, f64)], cadence: usize) -> Vec<f64> {
        navs.iter()
            .step_by(cadence)
            .collect::<Vec<_>>()
            .windows(2)
            .map(|pair| pair[1].1 / pair[0].1 - 1.0)
            .collect()
    }

    #[test]
    fn calendar_aligned_phases_do_not_smooth_a_common_daily_path() {
        let path = daily_return_path(260);
        let phases = (0..20)
            .map(|offset| phase_marks(&path, offset, 250, offset, 20, 0.0))
            .collect::<Vec<_>>();
        let combined = equal_weight_phase_daily_navs(&phases).unwrap();

        // The combined window starts at the last phase's first mark.
        assert_eq!(combined[0], (session(19), 1.0));
        assert_eq!(combined.last().unwrap().0, session(250));
        let combined_returns = daily_returns(&combined);
        let expected = &path[20..=250];
        assert_eq!(combined_returns.len(), expected.len());
        for (actual, wanted) in combined_returns.iter().zip(expected) {
            assert!(
                (actual - wanted).abs() < 1e-12,
                "combined daily return {actual} is not the common path's {wanted}"
            );
        }
    }

    #[test]
    fn identical_phases_keep_the_single_phase_sharpe() {
        let path = daily_return_path(200);
        let single = phase_marks(&path, 0, 199, 0, 20, 0.0);
        let phases = vec![single.clone(); 20];
        let combined = equal_weight_phase_daily_navs(&phases).unwrap();
        assert_eq!(combined.len(), single.len());
        let combined_sharpe = annualised_sharpe(&daily_returns(&combined), 252.0);
        let single_sharpe = annualised_sharpe(&daily_returns(&single), 252.0);
        assert!(
            (combined_sharpe - single_sharpe).abs() < 1e-9,
            "combined Sharpe {combined_sharpe} differs from the single-phase {single_sharpe}"
        );
    }

    #[test]
    fn combining_one_asset_cannot_beat_the_best_phase_sharpe() {
        // Long enough that a phase's Sharpe cannot win by sampling luck alone:
        // with only a couple of dozen holding periods the best of twenty phases
        // is dominated by estimation noise, which would hide the smoothing.
        let path = daily_return_path(2020);
        let cadence = 20;
        let phases = (0..cadence)
            .map(|offset| phase_marks(&path, offset, 2000, offset, cadence, 0.0015))
            .collect::<Vec<_>>();

        let best_phase_sharpe = phases
            .iter()
            .map(|phase| annualised_sharpe(&daily_returns(phase), 252.0))
            .fold(f64::MIN, f64::max);
        let combined_sharpe = annualised_sharpe(
            &daily_returns(&equal_weight_phase_daily_navs(&phases).unwrap()),
            252.0,
        );
        assert!(
            combined_sharpe <= best_phase_sharpe + 1e-6,
            "calendar-aligned Sharpe {combined_sharpe} exceeds the best phase's {best_phase_sharpe}"
        );

        // The defect this replaces: averaging by period index across phases whose
        // period k spans different sessions smooths the series and manufactures
        // a Sharpe no single phase earned.
        #[allow(deprecated)]
        let smoothed = equal_weight_phase_returns(
            &phases
                .iter()
                .map(|phase| phase_period_returns(phase, cadence))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let periods_per_year = 252.0 / cadence as f64;
        let smoothed_sharpe = annualised_sharpe(&smoothed, periods_per_year);
        let best_phase_period_sharpe = phases
            .iter()
            .map(|phase| annualised_sharpe(&phase_period_returns(phase, cadence), periods_per_year))
            .fold(f64::MIN, f64::max);
        assert!(
            smoothed_sharpe > best_phase_period_sharpe,
            "period-index averaging was expected to inflate Sharpe ({smoothed_sharpe} vs {best_phase_period_sharpe})"
        );
    }

    #[test]
    fn a_session_missing_from_one_phase_is_an_error_not_a_silent_truncation() {
        let path = daily_return_path(60);
        let mut phases = (0..3)
            .map(|offset| phase_marks(&path, offset, 50, offset, 20, 0.0))
            .collect::<Vec<_>>();
        phases[1].remove(20);
        let error = equal_weight_phase_daily_navs(&phases).unwrap_err();
        assert!(
            error.contains("session"),
            "expected a session-mismatch error, got {error}"
        );
    }

    #[test]
    fn phases_without_a_shared_window_are_rejected() {
        let path = daily_return_path(60);
        let early = phase_marks(&path, 0, 20, 0, 20, 0.0);
        let late = phase_marks(&path, 30, 50, 0, 20, 0.0);
        assert!(equal_weight_phase_daily_navs(&[early, late])
            .unwrap_err()
            .contains("overlap"));
    }
}
