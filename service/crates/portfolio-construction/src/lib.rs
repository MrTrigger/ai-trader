//! Strategy-neutral, direction-quota-free portfolio construction.
//!
//! Alpha models decide which signed candidates clear their economic hurdle.
//! Preliminary sizing turns those candidates into absolute weight proposals.
//! The allocator only reduces proposals to satisfy maximum budgets and limits:
//! it never creates a side, fills unused budget, or redistributes a cap excess.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

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
}
