//! Rust crypto portfolio decision core.
//!
//! This crate owns the deterministic path from Rust-computed feature rows and
//! venue truth to an immutable Plan. It has no venue adapter and cannot submit
//! orders. The executor remains a separate process and only parses the Plan.

pub mod backtest;
pub mod binance;
pub mod binance_archive;
pub mod config;
pub mod funding;
pub mod gate;
pub mod ic;
pub mod inspect;
pub mod liquidity;
pub mod model;
pub mod report;
pub mod research;
pub mod scores;
pub mod store;
pub mod training;
pub mod universe;
pub mod validate;

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use chrono::{DateTime, Utc};
use config::{Config, CostModel, RiskLimits};
use features_crypto::{DailyRow, HourlyRow};
use plan::{
    AssetCost, CostEstimate, CurrentPosition, Direction, Mode, Nav, Order, OrderReason, OrderType,
    Plan, Provenance, RiskCheck, RiskReport, Side, Status, Target, Warning, WarningKind,
};
use rust_decimal::{Decimal, RoundingStrategy};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use uuid::Uuid;

const PLAN_NAMESPACE: Uuid = Uuid::from_u128(0x6f1d5a1e6b1c5f4a9c2e3d7b8a0c4e11);
const SCHEMA_VERSION: &str = "1.2.0";
const SCORING_VERSION: &str = "sc-phase1-1";
const RISK_MODEL_VERSION: &str = "none-phase0";
const BPS: Decimal = Decimal::from_parts(10_000, 0, 0, false, 0);

fn d(v: f64) -> Result<Decimal, String> {
    if !v.is_finite() {
        return Err(format!("non-finite feature {v}"));
    }
    Decimal::from_str(&v.to_string()).map_err(|e| e.to_string())
}

fn q(value: Decimal, dp: u32) -> Decimal {
    let mut value = value.round_dp_with_strategy(dp, RoundingStrategy::MidpointNearestEven);
    value.rescale(dp);
    value
}

fn chrono_to_time(value: DateTime<Utc>) -> Result<OffsetDateTime, String> {
    OffsetDateTime::from_unix_timestamp_nanos(
        value
            .timestamp_nanos_opt()
            .ok_or("timestamp outside nanosecond range")? as i128,
    )
    .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Position {
    pub asset: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub qty: Decimal,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Portfolio {
    #[serde(with = "rust_decimal::serde::str")]
    pub cash: Decimal,
    #[serde(default)]
    pub positions: Vec<Position>,
}

impl Portfolio {
    fn nav(&self, prices: &BTreeMap<String, Decimal>) -> Result<Decimal, String> {
        self.positions.iter().try_fold(self.cash, |total, p| {
            prices
                .get(&p.asset)
                .map(|px| total + p.qty * *px)
                .ok_or_else(|| {
                    format!(
                        "holding {} but it has no price; refusing to mark it at zero",
                        p.asset
                    )
                })
        })
    }

    fn weights(
        &self,
        prices: &BTreeMap<String, Decimal>,
        nav: Decimal,
    ) -> BTreeMap<String, Decimal> {
        self.positions
            .iter()
            .filter(|p| !p.qty.is_zero())
            .map(|p| (p.asset.clone(), p.qty * prices[&p.asset] / nav))
            .collect()
    }
}

#[derive(Debug, Clone)]
struct Signal {
    asset: String,
    direction: Direction,
    conviction: Decimal,
    volatility: Option<Decimal>,
}

#[derive(Debug)]
struct Signals {
    values: Vec<Signal>,
    notes: Vec<String>,
    warnings: Vec<Warning>,
    scoring_version: String,
    model_id: Option<String>,
}

fn warning(kind: WarningKind, message: impl Into<String>) -> Warning {
    Warning {
        kind,
        message: message.into(),
    }
}

/// What the book may hold at all, before any signal ranks it.
///
/// Two liquidity floors, and they screen different things. `min_dollar_volume`
/// asks whether a name trades at all, over a whole day. `min_hourly_quote_volume`
/// asks whether it trades in the hours *we* send orders in, which is the
/// quantity that decides what a trade costs — a name can clear a $5M day on
/// volume that arrives while this bot is idle. KAITO does exactly that: it
/// clears the daily floor and trades $55,720 in the hour we cross it, which is
/// where 5.4x-under cost estimates and a measured 1.53x cost overrun came
/// from. Excluding that one name put realised cost at 1.02x the model.
///
/// Screening is the cheap fix and the cap is the expensive one: a name that
/// never enters the book costs nothing to size, while a name capped at 0.46%
/// of NAV has already consumed a slot in the cross-section to hold almost
/// nothing.
fn eligible<'a>(
    rows: impl Iterator<Item = &'a DailyRow>,
    cfg: &Config,
    profile: Option<&liquidity::Profile>,
) -> Result<(Vec<&'a DailyRow>, Vec<String>), String> {
    let mut keep = Vec::new();
    let mut notes = Vec::new();
    for row in rows {
        if row.bars_available < cfg.min_history_bars {
            notes.push(format!(
                "{}: {} bars{}, needs {}",
                row.asset,
                row.bars_available,
                if row.had_discontinuity {
                    " since a price discontinuity"
                } else {
                    ""
                },
                cfg.min_history_bars
            ));
            continue;
        }
        let Some(adv) = row.adv_quote else {
            notes.push(format!("{}: no liquidity estimate", row.asset));
            continue;
        };
        if d(adv)? < cfg.min_dollar_volume {
            notes.push(format!(
                "{}: median turnover {:.0} below {}",
                row.asset, adv, cfg.min_dollar_volume
            ));
            continue;
        }
        // The hour we trade in, not the day. Only a measured name can fail
        // this: an unmeasured one is not condemned for want of a reading.
        if cfg.min_hourly_quote_volume > Decimal::ZERO {
            if let Some(hourly) = profile.and_then(|p| p.hourly_volume(&row.asset)) {
                if hourly < cfg.min_hourly_quote_volume {
                    notes.push(format!(
                        "{}: {} of volume in the hours we trade, below {} - too thin to cross \
                         at our size",
                        row.asset,
                        q(hourly, 0),
                        cfg.min_hourly_quote_volume
                    ));
                    continue;
                }
            }
        }
        if row
            .vol_30
            .is_some_and(|v| d(v).is_ok_and(|v| v < cfg.min_volatility))
        {
            notes.push(format!(
                "{}: realised vol {:.4} below {} - a peg, not a position",
                row.asset,
                row.vol_30.unwrap(),
                cfg.min_volatility
            ));
            continue;
        }
        keep.push(row);
    }
    Ok((keep, notes))
}

fn generate_signal(
    cross: &[&DailyRow],
    hourly: &BTreeMap<String, &HourlyRow>,
    score_date: chrono::NaiveDate,
    cfg: &Config,
    profile: Option<&liquidity::Profile>,
) -> Result<Signals, String> {
    let (mut rows, mut notes) = eligible(cross.iter().copied(), cfg, profile)?;
    let mut warnings = Vec::new();
    let mut scoring_version = "none".to_string();
    let mut model_id = None;

    let values = match cfg.signal.as_str() {
        "placeholder_equal_long" => {
            warnings.push(warning(WarningKind::UnenforcedRule,
                "signal 'placeholder_equal_long' is a placeholder and claims no edge. It has not been through the backtest harness. No capital until it has."));
            rows.into_iter()
                .map(|r| Signal {
                    asset: r.asset.clone(),
                    direction: Direction::Long,
                    conviction: Decimal::ONE,
                    volatility: None,
                })
                .collect()
        }
        "liquidity_top" => {
            rows.sort_by(|a, b| {
                b.adv_quote
                    .unwrap()
                    .total_cmp(&a.adv_quote.unwrap())
                    .then_with(|| a.asset.cmp(&b.asset))
            });
            for r in rows.iter().skip(cfg.max_holdings) {
                notes.push(format!(
                    "{}: outside the top {} by liquidity",
                    r.asset, cfg.max_holdings
                ));
            }
            warnings.push(warning(WarningKind::UnenforcedRule,
                "signal 'liquidity_top' is a baseline, not a strategy: it claims no edge and exists to be beaten."));
            rows.into_iter()
                .take(cfg.max_holdings)
                .map(|r| {
                    Ok(Signal {
                        asset: r.asset.clone(),
                        direction: Direction::Long,
                        conviction: Decimal::ONE,
                        volatility: r.vol_30.map(d).transpose()?,
                    })
                })
                .collect::<Result<_, String>>()?
        }
        "xs_momentum" => {
            if rows.len() < cfg.min_cross_section {
                warnings.push(warning(WarningKind::DegenerateFeature, format!(
                    "group 'all' has {} asset(s), fewer than the {} a percentile rank needs to mean anything: scored neutral",
                    rows.len(), cfg.min_cross_section)));
                notes.push("cross-section too small to rank; target is flat".into());
                Vec::new()
            } else {
                scoring_version = SCORING_VERSION.into();
                // The shared scorer uses an average one-based rank among
                // measured values, with null measurements explicitly neutral
                // at 50. Ties must never be broken by input or asset order.
                let mut measured: Vec<_> = rows
                    .iter()
                    .enumerate()
                    .filter_map(|(i, r)| r.ret_30_skip_7.map(|v| (i, v)))
                    .collect();
                measured.sort_by(|a, b| a.1.total_cmp(&b.1));
                let measured_n = measured.len() as f64;
                let mut scores = vec![50.0; rows.len()];
                let mut start = 0;
                while start < measured.len() {
                    let mut end = start + 1;
                    while end < measured.len() && measured[end].1 == measured[start].1 {
                        end += 1;
                    }
                    let rank = ((start + 1) as f64 + end as f64) / 2.0;
                    let score = 100.0 * (rank - 0.5) / measured_n;
                    for (row_index, _) in &measured[start..end] {
                        scores[*row_index] = score;
                    }
                    start = end;
                }
                let missing: Vec<_> = rows
                    .iter()
                    .filter(|r| r.ret_30_skip_7.is_none())
                    .map(|r| r.asset.as_str())
                    .collect();
                if !missing.is_empty() {
                    warnings.push(warning(
                        WarningKind::DegenerateFeature,
                        format!(
                            "ret_30_skip_7 is missing for {}: scored neutral in factor 'momentum' rather than contributing a measurement",
                            missing.join(", ")
                        ),
                    ));
                }
                let mut scored: Vec<_> = rows.into_iter().zip(scores).collect();
                scored.sort_by(|(a, av), (b, bv)| {
                    bv.total_cmp(av).then_with(|| a.asset.cmp(&b.asset))
                });
                scored
                    .into_iter()
                    .take(cfg.max_holdings)
                    .map(|(r, score)| {
                        Ok(Signal {
                            asset: r.asset.clone(),
                            direction: Direction::Long,
                            conviction: q(d(score)?, 4),
                            volatility: r.vol_30.map(d).transpose()?,
                        })
                    })
                    .collect::<Result<_, String>>()?
            }
        }
        "gc_breakout" => {
            rows.retain(|r| r.gc_upper.is_some());
            rows.sort_by_key(|r| (r.gc_breakout_age.unwrap_or(u32::MAX), r.asset.clone()));
            rows.into_iter()
                .filter(|r| r.gc_breakout_age.is_some())
                .take(cfg.max_holdings)
                .map(|r| {
                    Ok(Signal {
                        asset: r.asset.clone(),
                        direction: Direction::Long,
                        conviction: if r.gc_breakout_age.unwrap() <= 25 {
                            Decimal::from(4)
                        } else {
                            Decimal::ONE
                        },
                        volatility: r.vol_30.map(d).transpose()?,
                    })
                })
                .collect::<Result<_, String>>()?
        }
        "gc_long_short" => {
            let warmed: Vec<_> = rows
                .into_iter()
                .filter(|r| r.gc_upper.is_some() && r.perp_listed)
                .collect();
            let mut longs: Vec<_> = warmed
                .iter()
                .copied()
                .filter(|r| r.gc_breakout_age.is_some())
                .collect();
            let mut shorts: Vec<_> = warmed
                .iter()
                .copied()
                .filter(|r| r.gc_breakout_age.is_none())
                .collect();
            if longs.len() < 3 || shorts.len() < 3 {
                notes.push(format!(
                    "only {} long and {} short candidates against a 3-a-side minimum; target is flat",
                    longs.len(), shorts.len()
                ));
                Vec::new()
            } else {
                if cfg.limits.max_position_count < 6 {
                    return Err(format!(
                        "gc_long_short needs max_position_count >= 6, got {}",
                        cfg.limits.max_position_count
                    ));
                }
                let tilt = cfg
                    .benchmark
                    .as_ref()
                    .and_then(|benchmark| cross.iter().find(|r| r.asset == *benchmark))
                    .and_then(|r| {
                        let filter = r.gc_regime_filter?;
                        let upper = r.gc_regime_upper?;
                        let slope = r.gc_regime_slope?;
                        let sign = if r.close > upper {
                            1.0
                        } else if r.close < filter {
                            -1.0
                        } else {
                            0.0
                        };
                        Some((sign * slope.abs() * 8.0).clamp(-0.5, 0.5))
                    })
                    .unwrap_or(0.0);
                let long_weight = d(0.5 + tilt)?;
                let short_weight = d(0.5 - tilt)?;
                if longs.len() + shorts.len() > cfg.limits.max_position_count {
                    let wanted = (Decimal::from(cfg.limits.max_position_count as u64)
                        * long_weight)
                        .round()
                        .to_string()
                        .parse::<usize>()
                        .map_err(|e| e.to_string())?;
                    let max_long = cfg
                        .limits
                        .max_position_count
                        .saturating_sub(3)
                        .min(longs.len());
                    let n_long = wanted.clamp(3, max_long);
                    let n_short = (cfg.limits.max_position_count - n_long).min(shorts.len());
                    longs.sort_by_key(|r| (r.gc_breakout_age.unwrap(), r.asset.clone()));
                    shorts.sort_by(|a, b| {
                        let distance = |r: &DailyRow| {
                            r.gc_lower
                                .map(|lower| (r.close - lower) / lower.abs())
                                .unwrap_or(f64::INFINITY)
                        };
                        distance(a)
                            .total_cmp(&distance(b))
                            .then_with(|| a.asset.cmp(&b.asset))
                    });
                    longs.truncate(n_long);
                    shorts.truncate(n_short);
                    notes.push(format!(
                        "truncated to {} long and {} short to fit max_position_count={}",
                        longs.len(),
                        shorts.len(),
                        cfg.limits.max_position_count
                    ));
                }
                let mut out = Vec::new();
                let per_long = long_weight / Decimal::from(longs.len() as u64);
                let per_short = short_weight / Decimal::from(shorts.len() as u64);
                longs.sort_by_key(|r| r.asset.clone());
                shorts.sort_by_key(|r| r.asset.clone());
                for (side, direction, conviction) in [
                    (longs, Direction::Long, per_long),
                    (shorts, Direction::Short, per_short),
                ] {
                    for r in side {
                        out.push(Signal {
                            asset: r.asset.clone(),
                            direction,
                            conviction,
                            volatility: r.vol_30.map(d).transpose()?,
                        });
                    }
                }
                out
            }
        }
        "ml_ranker" => {
            if rows.len() < 12 {
                notes.push(format!(
                    "{} eligible assets against a 12 minimum; target is flat",
                    rows.len()
                ));
                Vec::new()
            } else {
                let path = cfg
                    .model_path
                    .as_ref()
                    .ok_or("ml_ranker requires model_path; no fallback model is permitted")?;
                let model = model::Model::load(std::path::Path::new(path))?;
                let raw: Vec<_> = model
                    .features
                    .iter()
                    .map(|v| v.trim_start_matches("x_").to_owned())
                    .collect();
                let matrix: Vec<Vec<Option<f64>>> = rows
                    .iter()
                    .map(|row| {
                        raw.iter()
                            .map(|name| {
                                row.value(name)
                                    .or_else(|| hourly.get(&row.asset).and_then(|h| h.value(name)))
                            })
                            .collect()
                    })
                    .collect();
                let normalised = features_crypto::rank_normalise(&matrix)?;
                let mut out = Vec::new();
                for (row, values) in rows.iter().zip(normalised) {
                    let score = model.predict(&values, score_date)?;
                    let volatility = hourly.get(&row.asset).and_then(|h| h.rv_24h).or(row.vol_30);
                    let Some(volatility) = volatility.filter(|v| *v > 0.0) else {
                        continue;
                    };
                    // Conviction is an EXPECTED RETURN in every construction
                    // downstream: the cost threshold compares it to a
                    // round-trip in return units, and the risk-adjusted sizing
                    // divides it by vol exactly once. A per-risk model emits
                    // return-per-unit-risk, so its score is multiplied back
                    // into return units here rather than teaching every
                    // constructor about reward types.
                    let expected_return = match model.reward.as_str() {
                        "per_risk" => score.abs() * volatility,
                        _ => score.abs(),
                    };
                    out.push(Signal {
                        asset: row.asset.clone(),
                        direction: if score > 0.0 {
                            Direction::Long
                        } else {
                            Direction::Short
                        },
                        conviction: d(expected_return)?,
                        volatility: Some(d(volatility)?),
                    });
                }
                notes.push(format!(
                    "scored {} eligible assets with {} trained through {}",
                    out.len(),
                    model.model_version,
                    model.trained_through
                ));
                model_id = Some(model.model_id);
                out
            }
        }
        other => {
            return Err(format!(
            "signal {other:?} is not yet available in the Rust runtime; refusing Python fallback"
        ))
        }
    };
    Ok(Signals {
        values,
        notes,
        warnings,
        scoring_version,
        model_id,
    })
}

#[derive(Debug)]
struct Construction {
    weights: BTreeMap<String, Decimal>,
    constructor: String,
    notes: Vec<String>,
}

fn construct(
    signals: &[Signal],
    cfg: &Config,
    nav: Decimal,
    profile: Option<&liquidity::Profile>,
) -> Result<Construction, String> {
    let mut weights = BTreeMap::new();
    let mut notes = Vec::new();
    if signals.is_empty() {
        return Ok(Construction {
            weights,
            constructor: cfg.constructor.clone(),
            notes: vec!["no signals - target is flat".into()],
        });
    }
    match cfg.constructor.as_str() {
        "equal_weight" => {
            let per = (cfg.target_gross_exposure / Decimal::from(signals.len() as u64))
                .min(cfg.limits.max_position);
            for s in signals {
                weights.insert(
                    s.asset.clone(),
                    if s.direction == Direction::Long {
                        per
                    } else {
                        -per
                    },
                );
            }
        }
        "conviction_tilt" => {
            let total: Decimal = signals.iter().map(|s| s.conviction).sum();
            if total <= Decimal::ZERO {
                notes.push("convictions sum to zero - target is flat".into());
            } else {
                for s in signals {
                    let share = (cfg.target_gross_exposure * s.conviction / total)
                        .min(cfg.limits.max_position);
                    weights.insert(
                        s.asset.clone(),
                        if s.direction == Direction::Long {
                            share
                        } else {
                            -share
                        },
                    );
                }
            }
        }
        "inverse_vol" => {
            let usable: Vec<_> = signals
                .iter()
                .filter_map(|s| {
                    s.volatility
                        .filter(|v| *v > Decimal::ZERO)
                        .map(|v| (s, Decimal::ONE / v))
                })
                .collect();
            let total: Decimal = usable.iter().map(|(_, v)| *v).sum();
            for (s, inv) in usable {
                let share = (cfg.target_gross_exposure * inv / total).min(cfg.limits.max_position);
                weights.insert(
                    s.asset.clone(),
                    if s.direction == Direction::Long {
                        share
                    } else {
                        -share
                    },
                );
            }
        }
        "risk_adjusted" => {
            let floor = Decimal::from(2) * (cfg.costs.commission_bps + cfg.costs.spread_bps) / BPS;
            let usable: Vec<_> = signals
                .iter()
                .filter_map(|s| {
                    s.volatility
                        .filter(|v| *v > Decimal::ZERO && s.conviction >= floor)
                        .map(|v| (s, v))
                })
                .collect();
            let longs: Vec<_> = usable
                .iter()
                .copied()
                .filter(|(s, _)| s.direction == Direction::Long)
                .collect();
            let shorts: Vec<_> = usable
                .iter()
                .copied()
                .filter(|(s, _)| s.direction == Direction::Short)
                .collect();
            if longs.len() < 2 || shorts.len() < 2 {
                notes.push(format!(
                    "{} long and {} short cleared the threshold; a two-sided book cannot form",
                    longs.len(),
                    shorts.len()
                ));
            } else {
                // Faithful to the recovered harness: the floating book is the
                // top 24 by |expected return| ACROSS both sides, split
                // afterwards - not a per-side cap, and not ranked per-risk.
                // Risk enters at the sizing step (edge/vol within each side),
                // not at selection. A 20L/4S day is legal; a 1-sided one is not.
                let mut all: Vec<_> = usable.clone();
                all.sort_by(|(a, _), (b, _)| {
                    b.conviction
                        .partial_cmp(&a.conviction)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                all.truncate(24);
                let longs: Vec<_> = all
                    .iter()
                    .copied()
                    .filter(|(s, _)| s.direction == Direction::Long)
                    .collect();
                let shorts: Vec<_> = all
                    .iter()
                    .copied()
                    .filter(|(s, _)| s.direction == Direction::Short)
                    .collect();
                if longs.len() < 2 || shorts.len() < 2 {
                    notes.push(format!(
                        "top-24 book has {} long and {} short; a two-sided book cannot form",
                        longs.len(),
                        shorts.len()
                    ));
                } else {
                    let half = cfg.target_gross_exposure / Decimal::from(2);
                    for (side, sign) in [(longs, Decimal::ONE), (shorts, -Decimal::ONE)] {
                        let total: Decimal = side.iter().map(|(s, vol)| s.conviction / *vol).sum();
                        for (s, vol) in side {
                            weights.insert(
                                s.asset.clone(),
                                sign * half * (s.conviction / vol) / total,
                            );
                        }
                    }
                    let largest = weights.values().map(|v| v.abs()).max().unwrap_or_default();
                    if largest > cfg.limits.max_position {
                        let scale = cfg.limits.max_position / largest;
                        for weight in weights.values_mut() {
                            *weight *= scale;
                        }
                        notes.push(format!(
                            "per-position cap binds: whole book scaled by {}",
                            q(scale, 3)
                        ));
                    }
                    notes.push(format!(
                        "{} long and {} short, sized by edge/volatility",
                        weights.values().filter(|v| **v > Decimal::ZERO).count(),
                        weights.values().filter(|v| **v < Decimal::ZERO).count()
                    ));
                }
            }
        }
        other => return Err(format!("unknown constructor {other:?}")),
    }
    notes.extend(cap_by_participation(&mut weights, cfg, nav, profile));
    Ok(Construction {
        weights,
        constructor: cfg.constructor.clone(),
        notes,
    })
}

/// Hold no more of a name than its market will carry, and put what will not
/// fit somewhere it will.
///
/// Spec 8: a position's cap is the lower of `max_position` and
/// `participation_limit * hourly_volume / NAV`. The first is a mandate, the
/// second is the market, and until now only the mandate was enforced — so at
/// six times today's NAV the planner would have kept asking for a $20k
/// position in a name whose hour trades $200k, and simply paid.
///
/// The remainder spills down the same side of the book, proportional to the
/// headroom each name has left, and iterates because filling one name can push
/// it into its own cap. What cannot be placed is left unspent and disclosed:
/// idle capital is a smaller error than an unfillable order, and a silent
/// shortfall is a larger one than either.
///
/// Same side only. Spilling a capped long into the shorts would answer "this
/// name is too thin" by changing the book's net exposure, which is not an
/// answer to that question.
fn cap_by_participation(
    weights: &mut BTreeMap<String, Decimal>,
    cfg: &Config,
    nav: Decimal,
    profile: Option<&liquidity::Profile>,
) -> Vec<String> {
    let limit = cfg.limits.participation_limit;
    if limit <= Decimal::ZERO || nav <= Decimal::ZERO {
        return Vec::new();
    }
    let Some(profile) = profile else {
        return Vec::new();
    };
    // A name with no measured volume keeps the mandate cap. Assuming it is
    // illiquid would drop names for want of a measurement.
    let cap_of = |asset: &str| -> Decimal {
        match profile.hourly_volume(asset) {
            Some(v) if v > Decimal::ZERO => (limit * v / nav).min(cfg.limits.max_position),
            _ => cfg.limits.max_position,
        }
    };

    let mut notes = Vec::new();
    let mut capped: Vec<String> = Vec::new();
    for sign in [Decimal::ONE, -Decimal::ONE] {
        let side: Vec<String> = weights
            .iter()
            .filter(|(_, w)| !w.is_zero() && w.is_sign_positive() == sign.is_sign_positive())
            .map(|(a, _)| a.clone())
            .collect();
        if side.is_empty() {
            continue;
        }
        let mut spill = Decimal::ZERO;
        for asset in &side {
            let cap = cap_of(asset);
            let w = weights[asset];
            if w.abs() > cap {
                spill += w.abs() - cap;
                weights.insert(asset.clone(), sign * cap);
                capped.push(asset.clone());
            }
        }
        // Place the remainder where there is room. Proportional to headroom so
        // the shape of the book survives; iterated because each pass can fill
        // a name to its own cap.
        for _ in 0..8 {
            if spill <= Decimal::ZERO {
                break;
            }
            let room: Vec<(String, Decimal)> = side
                .iter()
                .filter_map(|a| {
                    let r = cap_of(a) - weights[a].abs();
                    (r > Decimal::ZERO).then(|| (a.clone(), r))
                })
                .collect();
            let total: Decimal = room.iter().map(|(_, r)| *r).sum();
            if total <= Decimal::ZERO {
                break;
            }
            let placing = spill.min(total);
            for (asset, r) in &room {
                let add = placing * *r / total;
                let w = weights[asset];
                weights.insert(asset.clone(), w + sign * add);
            }
            spill -= placing;
        }
        if spill > Decimal::ZERO {
            notes.push(format!(
                "participation cap leaves {} of {} weight unplaced: the whole side is at its \
                 liquidity limit",
                q(spill, 4),
                if sign.is_sign_positive() {
                    "long"
                } else {
                    "short"
                }
            ));
        }
    }
    if !capped.is_empty() {
        capped.sort();
        notes.push(format!(
            "participation cap binds on {}: {}",
            capped.len(),
            capped.join(", ")
        ));
    }
    notes
}

fn risk(
    weights: &BTreeMap<String, Decimal>,
    limits: &RiskLimits,
    clusters: &BTreeMap<String, String>,
    betas: &BTreeMap<String, Option<Decimal>>,
) -> (RiskReport, Vec<String>) {
    let mut checks = Vec::new();
    let mut disclosures = Vec::new();
    let gross: Decimal = weights.values().map(|v| v.abs()).sum();
    checks.push(RiskCheck {
        name: "max_gross_exposure".into(),
        limit: limits.max_gross_exposure,
        value: gross,
        passed: gross <= limits.max_gross_exposure,
        detail: Some("sum of |weight| across targets".into()),
    });
    let largest = weights
        .values()
        .map(|v| v.abs())
        .max()
        .unwrap_or(Decimal::ZERO);
    checks.push(RiskCheck {
        name: "max_position".into(),
        limit: limits.max_position,
        value: largest,
        passed: largest <= limits.max_position,
        detail: None,
    });
    let count = Decimal::from(weights.values().filter(|v| !v.is_zero()).count() as u64);
    checks.push(RiskCheck {
        name: "max_position_count".into(),
        limit: Decimal::from(limits.max_position_count as u64),
        value: count,
        passed: count <= Decimal::from(limits.max_position_count as u64),
        detail: None,
    });
    if let Some(limit) = limits.max_net_exposure {
        let net: Decimal = weights.values().sum();
        checks.push(RiskCheck {
            name: "max_net_exposure".into(),
            limit,
            value: net.abs(),
            passed: net.abs() <= limit,
            detail: None,
        });
    }
    if let Some(limit) = limits.max_cluster_exposure {
        let mut grouped: BTreeMap<String, Decimal> = BTreeMap::new();
        let mut unknown = Vec::new();
        for (asset, weight) in weights.iter().filter(|(_, w)| !w.is_zero()) {
            let key = clusters.get(asset).cloned().unwrap_or_else(|| {
                unknown.push(asset.clone());
                format!("_unclassified:{asset}")
            });
            *grouped.entry(key).or_default() += weight.abs();
        }
        let (name, value) = grouped
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)))
            .unwrap_or(("no positions".into(), Decimal::ZERO));
        if !unknown.is_empty() {
            disclosures.push(format!(
                "cluster limit does not constrain {}: no cluster is configured for them",
                unknown.join(", ")
            ));
        }
        checks.push(RiskCheck {
            name: "max_cluster_exposure".into(),
            limit,
            value,
            passed: value <= limit,
            detail: Some(format!(
                "largest cluster {}",
                name.trim_start_matches("_unclassified:")
            )),
        });
    }
    if let Some(limit) = limits.max_benchmark_beta {
        let mut assumed = Vec::new();
        let beta: Decimal = weights
            .iter()
            .map(|(asset, weight)| {
                let b = betas.get(asset).copied().flatten().unwrap_or_else(|| {
                    assumed.push(asset.clone());
                    Decimal::ONE
                });
                *weight * b
            })
            .sum();
        if !assumed.is_empty() {
            disclosures.push(format!(
                "beta assumed 1 for {}: too little history to estimate one",
                assumed.join(", ")
            ));
        }
        checks.push(RiskCheck {
            name: "max_benchmark_beta".into(),
            limit,
            value: beta.abs(),
            passed: beta.abs() <= limit,
            detail: Some("|w'beta| against the configured benchmark".into()),
        });
    }
    let failed: Vec<_> = checks
        .iter()
        .filter(|c| !c.passed)
        .map(|c| format!("{} {} exceeds {}", c.name, c.value, c.limit))
        .collect();
    (
        RiskReport {
            passed: failed.is_empty(),
            checks,
            rejected_reason: (!failed.is_empty()).then(|| failed.join("; ")),
        },
        disclosures,
    )
}

#[derive(Debug)]
struct Estimate {
    asset: String,
    notional: Decimal,
    spread: Decimal,
    impact: Decimal,
    total: Decimal,
}

/// What one order costs, and against which liquidity.
///
/// Two things the flat model got wrong, both fixed here, both inert until a
/// [`liquidity::Profile`] supplies the measurement:
///
/// **The spread is per asset.** One constant for every name is defensible for
/// the liquid majority and badly wrong for the tail — measured, KAITO's spread
/// runs 10 to 30 times the flat 0.50bp the model charges it.
///
/// **Impact meets one hour's liquidity, not a day's.** An order is sliced, and
/// each slice crosses against the volume available in the hour it is sent. So
/// the denominator is the hour's volume and the numerator is one slice, which
/// is where the spec's `1/√N` comes from: dividing the order by N divides
/// participation by N, and impact goes as its square root.
///
/// Falling back to daily ADV when no profile exists is not a rescaling of the
/// same number — a day holds 24 hours and the square root of that is nearly
/// five. The impact coefficient was assumed against the daily denominator, so
/// the two paths are not interchangeable and a profile must not be introduced
/// without re-running the walk-forward behind it.
struct Liquidity {
    /// Volume in one slice-hour, when measured.
    hourly: Option<Decimal>,
    /// Average daily volume — the old denominator, and the fallback.
    adv: Option<Decimal>,
    /// Measured full spread in bps, when measured.
    spread_bps: Option<Decimal>,
    /// How many slices the order is split into.
    slices: Decimal,
}

fn estimate(
    asset: &str,
    notional: Decimal,
    liq: &Liquidity,
    vol: Option<Decimal>,
    model: &CostModel,
) -> Estimate {
    // One slice against one hour, or the whole order against a day. Never a
    // slice against a day, which would flatter every thin name.
    let (denominator, numerator) = match liq.hourly.filter(|v| *v > Decimal::ZERO) {
        Some(hourly) => (Some(hourly), notional / liq.slices.max(Decimal::ONE)),
        None => (liq.adv.filter(|v| *v > Decimal::ZERO), notional),
    };
    let impact = if notional <= Decimal::ZERO {
        Decimal::ZERO
    } else if let (Some(denominator), Some(vol)) = (denominator, vol.filter(|v| *v > Decimal::ZERO))
    {
        let participation = (numerator / denominator)
            .to_string()
            .parse::<f64>()
            .unwrap_or(f64::INFINITY)
            .sqrt();
        model.impact_coefficient * d(participation).unwrap_or(BPS) * vol * BPS
    } else {
        BPS
    };
    let spread = liq.spread_bps.unwrap_or(model.spread_bps);
    Estimate {
        asset: asset.into(),
        notional,
        spread,
        impact,
        total: spread + model.commission_bps + impact,
    }
}

fn reason(current: Decimal, target: Decimal) -> OrderReason {
    if current.is_zero() {
        OrderReason::Entry
    } else if target.is_zero() {
        OrderReason::Exit
    } else if target.abs() > current.abs() {
        OrderReason::Increase
    } else {
        OrderReason::Reduce
    }
}

/// Everything `diff` needs to know about the market, as opposed to the book.
struct Market<'a> {
    prices: &'a BTreeMap<String, Decimal>,
    adv: &'a BTreeMap<String, Option<Decimal>>,
    vol: &'a BTreeMap<String, Option<Decimal>>,
    profile: Option<&'a liquidity::Profile>,
}

fn diff(
    weights: &BTreeMap<String, Decimal>,
    current: &BTreeMap<String, Decimal>,
    market: &Market<'_>,
    nav: Decimal,
    cfg: &Config,
) -> (Vec<Order>, Vec<Estimate>, Vec<String>, Decimal, Decimal) {
    let Market {
        prices,
        adv,
        vol,
        profile,
    } = *market;
    let assets: BTreeSet<_> = weights.keys().chain(current.keys()).cloned().collect();
    let mut candidates = Vec::new();
    let mut skipped = Vec::new();
    for asset in assets {
        let target = weights.get(&asset).copied().unwrap_or_default();
        let now = current.get(&asset).copied().unwrap_or_default();
        let drift = target - now;
        if drift.is_zero() {
            continue;
        }
        let Some(price) = prices.get(&asset) else {
            skipped.push(format!("{asset}: no price, cannot size a trade"));
            continue;
        };
        let notional = drift * nav;
        let estimate = estimate(
            &asset,
            notional.abs(),
            &Liquidity {
                hourly: profile.and_then(|p| p.hourly_volume(&asset)),
                adv: adv.get(&asset).copied().flatten(),
                spread_bps: profile.and_then(|p| p.spread(&asset)),
                slices: Decimal::from(cfg.execution_slices.max(1) as u64),
            },
            vol.get(&asset).copied().flatten(),
            &cfg.costs,
        );
        let why = reason(now, target);
        if why != OrderReason::Exit {
            if notional.abs() < cfg.limits.min_position_notional {
                skipped.push(format!("{asset}: notional below minimum"));
                continue;
            }
            if drift.abs() * BPS < estimate.total * Decimal::from(2) * cfg.rebalance_cost_multiple {
                skipped.push(format!("{asset}: drift under cost deadband"));
                continue;
            }
        }
        candidates.push((asset, drift, notional, *price, why, estimate));
    }
    // Match the retired planner's turnover policy exactly: exits are exempt,
    // then the remaining budget is spent on the largest absolute drift,
    // regardless of whether that drift is a reduction or an increase.  The
    // execution-safe reason ordering is applied only after budget selection.
    candidates.sort_by(
        |a, b| match (a.4 == OrderReason::Exit, b.4 == OrderReason::Exit) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            (true, true) => a.0.cmp(&b.0),
            (false, false) => b.1.abs().cmp(&a.1.abs()).then_with(|| a.0.cmp(&b.0)),
        },
    );
    let mut used = Decimal::ZERO;
    let mut dropped = Decimal::ZERO;
    let mut selected = Vec::new();
    for (asset, drift, notional, price, why, est) in candidates {
        if why != OrderReason::Exit && used + drift.abs() > cfg.turnover_budget {
            dropped += drift.abs();
            skipped.push(format!("{asset}: trade deferred by turnover budget"));
            continue;
        }
        used += drift.abs();
        selected.push((asset, drift, notional, price, why, est));
    }
    let reason_rank = |reason: OrderReason| match reason {
        OrderReason::Exit => 0,
        OrderReason::Reduce => 1,
        OrderReason::Increase => 2,
        OrderReason::Entry => 3,
        OrderReason::Rebalance => 4,
    };
    selected.sort_by(|a, b| {
        reason_rank(a.4)
            .cmp(&reason_rank(b.4))
            .then_with(|| a.0.cmp(&b.0))
    });
    let mut orders = Vec::new();
    let mut estimates = Vec::new();
    for (asset, _drift, notional, price, why, est) in selected {
        orders.push(Order {
            asset,
            side: if notional > Decimal::ZERO {
                Side::Buy
            } else {
                Side::Sell
            },
            qty: q((notional / price).abs(), 8),
            order_type: OrderType::Market,
            limit_price: None,
            reason: why,
            est_cost_bps: Some(q(est.total, 2)),
        });
        estimates.push(est);
    }
    (orders, estimates, skipped, used, dropped)
}

pub struct DecisionInput<'a> {
    pub as_of: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub mode: Mode,
    pub config: &'a Config,
    pub daily_features: &'a [DailyRow],
    pub hourly_features: Option<&'a [HourlyRow]>,
    pub eligible_universe: &'a BTreeSet<String>,
    pub portfolio: &'a Portfolio,
    pub inputs_hash: &'a str,
}

#[derive(Debug)]
pub struct DecisionResult {
    pub plan: Plan,
    pub notes: Vec<String>,
    pub skipped: Vec<String>,
}

pub fn decide(input: DecisionInput<'_>) -> Result<DecisionResult, String> {
    let cfg = input.config;
    if input.eligible_universe.is_empty() {
        return Err("universe snapshot has no eligible assets".into());
    }
    let horizon = input.as_of - chrono::Duration::seconds(cfg.interval_s);
    let newest = input
        .daily_features
        .iter()
        .filter(|r| r.ts_utc <= horizon)
        .map(|r| r.ts_utc)
        .max()
        .ok_or_else(|| format!("no daily features at or before {horizon}"))?;
    let max_staleness = chrono::Duration::seconds(cfg.interval_s * 2);
    if horizon - newest > max_staleness {
        return Err(format!(
            "stale bars: newest is {newest}, horizon is {horizon} ({}s > {}s)",
            (horizon - newest).num_seconds(),
            max_staleness.num_seconds()
        ));
    }
    let mut latest: BTreeMap<String, &DailyRow> = BTreeMap::new();
    for row in input.daily_features.iter().filter(|r| r.ts_utc <= horizon) {
        if latest
            .get(&row.asset)
            .is_none_or(|old| old.ts_utc < row.ts_utc)
        {
            latest.insert(row.asset.clone(), row);
        }
    }
    let missing: Vec<_> = input
        .eligible_universe
        .iter()
        .filter(|asset| !latest.contains_key(*asset))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "universe is incomplete: no bars for {missing:?}; refusing exits against a partial universe"
        ));
    }
    if cfg.limits.max_benchmark_beta.is_some() {
        let benchmark = cfg
            .benchmark
            .as_ref()
            .ok_or("max_benchmark_beta is enforced but no benchmark is configured")?;
        if !latest.contains_key(benchmark) {
            return Err(format!(
                "max_benchmark_beta is enforced but benchmark {benchmark} has no bars at the horizon"
            ));
        }
    }
    if latest.is_empty() {
        return Err(format!("no bars at or before {horizon}"));
    }
    let newest = latest.values().map(|r| r.ts_utc).max().unwrap();
    if horizon - newest > chrono::Duration::seconds(cfg.interval_s * 2) {
        return Err(format!(
            "stale bars: newest is {newest}, horizon is {horizon}"
        ));
    }
    let missing: Vec<_> = input
        .eligible_universe
        .iter()
        .filter(|a| !latest.contains_key(*a))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(format!("universe is incomplete: no bars for {missing:?}"));
    }

    let prices: BTreeMap<_, _> = latest
        .iter()
        .map(|(a, r)| Ok((a.clone(), d(r.mark_close)?)))
        .collect::<Result<_, String>>()?;
    let nav = input.portfolio.nav(&prices)?;
    if nav <= Decimal::ZERO {
        return Err(format!("non-positive NAV ({nav})"));
    }
    let current = input.portfolio.weights(&prices, nav);
    let cross: Vec<_> = input
        .eligible_universe
        .iter()
        .filter_map(|a| latest.get(a).copied())
        .collect();
    let mut hourly: BTreeMap<String, &HourlyRow> = BTreeMap::new();
    let hourly_horizon = input.as_of - chrono::Duration::hours(1);
    for row in input
        .hourly_features
        .unwrap_or_default()
        .iter()
        .filter(|r| r.ts_utc <= hourly_horizon)
    {
        if hourly
            .get(&row.asset)
            .is_none_or(|old| old.ts_utc < row.ts_utc)
        {
            hourly.insert(row.asset.clone(), row);
        }
    }
    // The leakage guard compares against the newest FEATURE date, not the
    // decision date. Training rows are dated by their decision stamp D and
    // carry a label whose return window runs into D+1; a model trained through
    // C therefore contains information about D = C+1. Handing the guard the
    // decision date let exactly that day through — one leaked date per fold,
    // which the Python side (comparing `frame["ts_utc"].max()`, the last closed
    // bar) always refused. Match the stricter side.
    let newest_feature_date = input
        .as_of
        .date_naive()
        .pred_opt()
        .ok_or("date underflow")?;
    // Measured tradeability, if anyone has measured it. Absent is the shipped
    // behaviour: one flat spread, impact against daily volume, no cap, no
    // hourly floor. Loaded before the signal because eligibility is the first
    // thing it decides — a name too thin to cross should never be ranked, let
    // alone sized and then capped back down to nothing.
    let profile = match cfg.liquidity_profile.as_deref() {
        Some(path) => Some(liquidity::Profile::load(path)?),
        None => None,
    };
    let generated = generate_signal(&cross, &hourly, newest_feature_date, cfg, profile.as_ref())?;
    let construction = construct(&generated.values, cfg, nav, profile.as_ref())?;
    let betas: BTreeMap<_, _> = latest
        .iter()
        .map(|(a, r)| Ok((a.clone(), r.beta_bench.map(d).transpose()?.map(|v| q(v, 6)))))
        .collect::<Result<_, String>>()?;
    let (report, disclosures) = risk(&construction.weights, &cfg.limits, &cfg.clusters, &betas);
    let adv: BTreeMap<_, _> = latest
        .iter()
        .map(|(a, r)| Ok((a.clone(), r.adv_quote.map(d).transpose()?)))
        .collect::<Result<_, String>>()?;
    let vol: BTreeMap<_, _> = latest
        .iter()
        .map(|(a, r)| {
            Ok((
                a.clone(),
                r.vol_30
                    .map(|v| d(v).map(|v| v / d(365.0_f64.sqrt()).unwrap()))
                    .transpose()?,
            ))
        })
        .collect::<Result<_, String>>()?;
    let (orders, estimates, mut skipped, used, dropped) = if report.passed {
        diff(
            &construction.weights,
            &current,
            &Market {
                prices: &prices,
                adv: &adv,
                vol: &vol,
                profile: profile.as_ref(),
            },
            nav,
            cfg,
        )
    } else {
        (
            Vec::new(),
            Vec::new(),
            vec!["plan rejected: no orders computed".into()],
            Decimal::ZERO,
            Decimal::ZERO,
        )
    };

    let mut warnings = generated.warnings;
    // A cap that quietly reshapes the book belongs on the artefact, not in a
    // log line. `TurnoverCapped` is the nearest existing kind and reads the
    // same way: the plan wanted more than it was allowed to do.
    for note in &construction.notes {
        if note.starts_with("participation cap") {
            warnings.push(Warning {
                kind: WarningKind::TurnoverCapped,
                message: note.clone(),
            });
        }
    }

    warnings.push(warning(
        WarningKind::UnenforcedRule,
        "no risk model: covariance does not inform construction",
    ));
    if cfg.limits.max_net_exposure.is_none() {
        warnings.push(warning(
            WarningKind::UnenforcedRule,
            "limit max_net_exposure is not enforced",
        ));
    }
    if cfg.limits.max_cluster_exposure.is_none() {
        warnings.push(warning(
            WarningKind::UnenforcedRule,
            "limit max_cluster_exposure is not enforced",
        ));
    }
    if cfg.limits.max_benchmark_beta.is_none() {
        warnings.push(warning(
            WarningKind::UnenforcedRule,
            "limit max_benchmark_beta is not enforced",
        ));
    }
    warnings.extend(
        disclosures
            .into_iter()
            .map(|v| warning(WarningKind::UnenforcedRule, v)),
    );
    if !cfg.costs.calibrated {
        warnings.push(warning(WarningKind::Other, "cost model is uncalibrated: the impact coefficient is assumed, not fitted to realised fills"));
    }
    if !dropped.is_zero() {
        warnings.push(warning(
            WarningKind::TurnoverCapped,
            format!(
                "turnover budget {} spent ({used} used); deferred {dropped} weight",
                cfg.turnover_budget
            ),
        ));
    }

    let total_quote: Decimal = estimates.iter().map(|e| e.notional * e.total / BPS).sum();
    let targets = construction
        .weights
        .iter()
        .map(|(asset, weight)| Target {
            asset: asset.clone(),
            weight: q(*weight, 6),
            direction: if *weight >= Decimal::ZERO {
                Direction::Long
            } else {
                Direction::Short
            },
            conviction: generated
                .values
                .iter()
                .find(|s| s.asset == *asset)
                .map(|s| q(s.conviction, 4)),
        })
        .collect();
    let current_positions = input
        .portfolio
        .positions
        .iter()
        .map(|p| CurrentPosition {
            asset: p.asset.clone(),
            qty: q(p.qty, 8),
            weight: q(current.get(&p.asset).copied().unwrap_or_default(), 6),
        })
        .collect();
    let cost_estimate = CostEstimate {
        total_bps: q(
            if nav > Decimal::ZERO {
                total_quote / nav * BPS
            } else {
                Decimal::ZERO
            },
            2,
        ),
        total_quote: q(total_quote, 2),
        per_asset: estimates
            .iter()
            .map(|e| AssetCost {
                asset: e.asset.clone(),
                bps: q(e.total, 2),
                spread_bps: Some(q(e.spread, 2)),
                impact_bps: Some(q(e.impact, 2)),
            })
            .collect(),
    };
    let gross: Decimal = construction.weights.values().map(|v| v.abs()).sum();
    let net: Decimal = construction.weights.values().sum();
    let run_id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("run:{}", input.as_of.to_rfc3339()).as_bytes(),
    );
    let mut plan = Plan {
        schema_version: SCHEMA_VERSION.into(),
        plan_id: Uuid::nil(),
        run_id,
        bot_id: cfg.bot_id.clone(),
        created_at: chrono_to_time(input.created_at)?,
        as_of: chrono_to_time(input.as_of)?,
        mode: input.mode,
        status: if report.passed {
            Status::Accepted
        } else {
            Status::Rejected
        },
        quote_currency: cfg.quote_currency.clone(),
        nav: Nav {
            total: q(nav, 2),
            cash: q(input.portfolio.cash, 2),
            gross_exposure: q(gross, 6),
            net_exposure: q(net, 6),
            benchmark_beta: None,
        },
        provenance: Provenance {
            planner_version: env!("CARGO_PKG_VERSION").into(),
            feature_set_version: features_crypto::FEATURE_SET_VERSION.into(),
            scoring_version: generated.scoring_version,
            risk_model_version: RISK_MODEL_VERSION.into(),
            ruleset_version: cfg.ruleset_version.clone(),
            constructor: construction.constructor,
            constructor_requested: cfg.constructor.clone(),
            model_id: generated.model_id.clone(),
            inputs_hash: input.inputs_hash.into(),
            universe_size: input.eligible_universe.len() as u32,
        },
        targets,
        current: current_positions,
        orders,
        risk_report: report,
        cost_estimate,
        warnings,
    };
    plan.plan_id = content_id(&plan)?;
    // Parse our own wire output through the executor's invariants before it can
    // leave this crate. A Rust producer is not trusted merely for being Rust.
    Plan::parse(&canonical_json(&plan)?).map_err(|e| e.to_string())?;
    let mut notes = generated.notes;
    notes.extend(construction.notes);
    if skipped.is_empty() {
        skipped = Vec::new();
    }
    Ok(DecisionResult {
        plan,
        notes,
        skipped,
    })
}

fn canonical_json<T: serde::Serialize>(value: &T) -> Result<String, String> {
    // Serialize through Value so object keys use serde_json::Map's stable
    // lexical ordering rather than Rust struct declaration order. This is the
    // same canonical wire rule the former Python producer used.
    let value = serde_json::to_value(value).map_err(|e| e.to_string())?;
    let mut text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
    text.push('\n');
    Ok(text)
}

fn content_id(plan: &Plan) -> Result<Uuid, String> {
    let mut value = serde_json::to_value(plan).map_err(|e| e.to_string())?;
    let object = value
        .as_object_mut()
        .ok_or("plan did not serialize to object")?;
    object.remove("created_at");
    object.remove("plan_id");
    let body = canonical_json(&value)?;
    let digest = Sha256::digest(body.as_bytes());
    Ok(Uuid::new_v5(
        &PLAN_NAMESPACE,
        format!("{digest:x}").as_bytes(),
    ))
}

pub fn write_plan(path: &std::path::Path, plan: &Plan) -> Result<(), String> {
    let text = canonical_json(plan)?;
    Plan::parse(&text).map_err(|e| e.to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, text.as_bytes()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;

    fn row(asset: &str, ts: DateTime<Utc>, close: f64) -> DailyRow {
        DailyRow {
            ts_utc: ts,
            asset: asset.into(),
            open: close,
            high: close,
            low: close,
            close,
            mark_open: close,
            mark_close: close,
            had_discontinuity: false,
            bars_available: 200,
            perp_listed: true,
            ret_1: Some(0.01),
            ret_7: Some(0.1),
            ret_30: Some(0.2),
            ret_90: Some(0.3),
            ret_30_skip_7: Some(0.15),
            vol_30: Some(0.5),
            adv_quote: Some(100_000_000.0),
            ret_180: None,
            vol_7: None,
            vol_90: None,
            skew_30: None,
            semivol_30: None,
            dist_high_90: None,
            dist_low_90: None,
            amihud_30: None,
            turnover_20: None,
            range_frac_14: None,
            band_width: None,
            dist_upper: None,
            dist_lower: None,
            breakout_age_f: None,
            vol_ratio: None,
            f_gap: None,
            f_range: None,
            f_clv: None,
            f_volsurp: None,
            f_trsurp: None,
            f_amihud: None,
            f_ret2: None,
            f_ret3: None,
            f_accel: None,
            f_dvol: None,
            funding_7d: None,
            funding_30d: None,
            funding_chg: None,
            funding_z: None,
            beta_bench: Some(1.0),
            gc_filter: Some(close),
            gc_upper: Some(close * 1.1),
            gc_lower: Some(close * 0.9),
            gc_breakout_age: None,
            gc_regime_filter: Some(close),
            gc_regime_upper: Some(close * 1.1),
            gc_regime_slope: Some(0.01),
        }
    }

    fn config() -> Config {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config/default.toml");
        let mut cfg = Config::load(&path).unwrap();
        // The fixtures below encode the placeholder baseline; the deployed
        // config now names the ranker, which needs a model artefact these unit
        // tests deliberately do not have. Tests exercising another signal or
        // constructor set it themselves.
        cfg.signal = "placeholder_equal_long".into();
        cfg.constructor = "conviction_tilt".into();
        cfg.model_path = None;
        cfg
    }

    /// A profile with one thin name, sized so the cap bites exactly once.
    fn thin_profile(asset: &str, hourly: i64) -> liquidity::Profile {
        let mut p = liquidity::Profile {
            measured_at: "2026-08-12T00:00:00Z".into(),
            hours: vec![1, 2],
            ..Default::default()
        };
        p.assets.insert(
            asset.into(),
            liquidity::AssetLiquidity {
                hourly_quote_volume: Some(Decimal::from(hourly)),
                ..Default::default()
            },
        );
        p
    }

    /// The KAITO case, as measured on 2026-08-12: it clears the $5M daily
    /// floor and trades $55,720 in the hour we cross it. The daily screen has
    /// nothing to say about that; the hourly one removes it.
    #[test]
    fn a_name_that_clears_the_day_can_still_be_too_thin_in_our_hour() {
        let mut cfg = config();
        cfg.min_hourly_quote_volume = Decimal::from(250_000);
        let ts = "2026-08-01T00:00:00Z".parse().unwrap();
        let rows = [row("BTC", ts, 100.0), row("KAITO", ts, 1.0)];
        // Both clear min_dollar_volume comfortably.
        assert!(d(rows[1].adv_quote.unwrap()).unwrap() > cfg.min_dollar_volume);

        let mut profile = thin_profile("KAITO", 55_720);
        profile.assets.insert(
            "BTC".into(),
            liquidity::AssetLiquidity {
                hourly_quote_volume: Some(Decimal::from(40_000_000)),
                ..Default::default()
            },
        );
        let (keep, notes) = eligible(rows.iter(), &cfg, Some(&profile)).unwrap();
        assert_eq!(
            keep.iter().map(|r| r.asset.as_str()).collect::<Vec<_>>(),
            ["BTC"]
        );
        assert!(notes
            .iter()
            .any(|n| n.contains("KAITO") && n.contains("55720") && n.contains("hours we trade")));
    }

    #[test]
    fn the_hourly_floor_is_off_until_set_and_never_condemns_an_unmeasured_name() {
        let mut cfg = config();
        let ts = "2026-08-01T00:00:00Z".parse().unwrap();
        let rows = [row("KAITO", ts, 1.0)];
        let profile = thin_profile("KAITO", 55_720);

        // Shipped: the floor is zero, so a measured-thin name stays eligible.
        assert_eq!(cfg.min_hourly_quote_volume, Decimal::ZERO);
        assert_eq!(
            eligible(rows.iter(), &cfg, Some(&profile)).unwrap().0.len(),
            1
        );

        // Set, but with no profile at all: nothing to screen against, and the
        // planner does not invent a reading in order to drop a name.
        cfg.min_hourly_quote_volume = Decimal::from(250_000);
        assert_eq!(eligible(rows.iter(), &cfg, None).unwrap().0.len(), 1);

        // Set, with a profile that covers other names but not this one.
        let elsewhere = thin_profile("BTC", 40_000_000);
        assert_eq!(
            eligible(rows.iter(), &cfg, Some(&elsewhere))
                .unwrap()
                .0
                .len(),
            1
        );
    }

    #[test]
    fn a_measured_spread_replaces_the_flat_one_only_where_it_was_measured() {
        let cfg = config();
        let flat = cfg.costs.spread_bps;
        let mut profile = liquidity::Profile::default();
        profile.assets.insert(
            "KAITO".into(),
            liquidity::AssetLiquidity {
                spread_bps: Some(Decimal::from(15)),
                ..Default::default()
            },
        );
        let liq = |asset: &str| Liquidity {
            hourly: None,
            adv: Some(Decimal::from(100_000_000)),
            spread_bps: profile.spread(asset),
            slices: Decimal::ONE,
        };
        let thin = estimate(
            "KAITO",
            Decimal::from(9_000),
            &liq("KAITO"),
            None,
            &cfg.costs,
        );
        let fat = estimate("BTC", Decimal::from(9_000), &liq("BTC"), None, &cfg.costs);
        assert_eq!(
            thin.spread,
            Decimal::from(15),
            "measured wins where it exists"
        );
        assert_eq!(
            fat.spread, flat,
            "and a name never sampled keeps the flat one"
        );
    }

    /// Impact meets ONE slice against ONE hour. The old model met the whole
    /// order against a whole day, which flatters a thin name by the square
    /// root of twenty-four.
    #[test]
    fn impact_prices_a_slice_against_an_hour_not_an_order_against_a_day() {
        let cfg = config();
        let vol = Some(Decimal::new(5, 2));
        let notional = Decimal::from(9_000);
        let daily = estimate(
            "KAITO",
            notional,
            &Liquidity {
                hourly: None,
                adv: Some(Decimal::from(4_800_000)),
                spread_bps: None,
                slices: Decimal::from(2),
            },
            vol,
            &cfg.costs,
        );
        // The same name, same day's volume, but priced against the hour it is
        // actually sent in — one twenty-fourth of it — in two slices.
        let hourly = estimate(
            "KAITO",
            notional,
            &Liquidity {
                hourly: Some(Decimal::from(200_000)),
                adv: Some(Decimal::from(4_800_000)),
                spread_bps: None,
                slices: Decimal::from(2),
            },
            vol,
            &cfg.costs,
        );
        assert!(
            hourly.impact > daily.impact,
            "an hour holds less than a day: {} should exceed {}",
            hourly.impact,
            daily.impact
        );
        // sqrt((9000/2)/200000) / sqrt(9000/4800000) = sqrt(0.0225/0.001875)
        // = sqrt(12) = 3.46.
        let ratio = (hourly.impact / daily.impact).round_dp(2);
        assert_eq!(ratio, Decimal::new(346, 2), "got {ratio}");
    }

    #[test]
    fn slicing_an_order_reduces_its_impact_by_the_root_of_the_slice_count() {
        let cfg = config();
        let vol = Some(Decimal::new(5, 2));
        let one = |slices: i64| {
            estimate(
                "KAITO",
                Decimal::from(9_000),
                &Liquidity {
                    hourly: Some(Decimal::from(200_000)),
                    adv: None,
                    spread_bps: None,
                    slices: Decimal::from(slices),
                },
                vol,
                &cfg.costs,
            )
            .impact
        };
        // Spec 8: N slices divide the order by N against the same per-hour
        // liquidity, so impact falls as 1/root-N.
        let ratio = (one(1) / one(4)).round_dp(2);
        assert_eq!(ratio, Decimal::from(2), "got {ratio}");
    }

    #[test]
    fn a_thin_name_is_capped_and_the_remainder_goes_to_its_own_side() {
        let mut cfg = config();
        cfg.limits.participation_limit = Decimal::new(5, 2); // 5% of the hour
        let nav = Decimal::from(600_000);
        // 5% of $200k is $10k, which at a $600k book is a 1.67% weight.
        let profile = thin_profile("KAITO", 200_000);
        let mut weights = BTreeMap::from([
            ("KAITO".to_string(), Decimal::new(10, 2)),
            ("BTC".to_string(), Decimal::new(10, 2)),
            ("ETH".to_string(), Decimal::new(10, 2)),
        ]);
        let before: Decimal = weights.values().sum();
        let notes = cap_by_participation(&mut weights, &cfg, nav, Some(&profile));

        let cap = Decimal::new(5, 2) * Decimal::from(200_000) / nav;
        assert_eq!(weights["KAITO"].round_dp(6), cap.round_dp(6));
        assert!(
            notes.iter().any(|n| n.contains("KAITO")),
            "the cap must be disclosed, got {notes:?}"
        );
        // Nothing is lost: the gross the constructor asked for is still there,
        // moved to names with room rather than dropped.
        let after: Decimal = weights.values().sum();
        assert_eq!(
            after.round_dp(6),
            before.round_dp(6),
            "spill must preserve the side's gross"
        );
        assert!(weights["BTC"] > Decimal::new(10, 2), "BTC absorbed some");
        assert!(weights["ETH"] > Decimal::new(10, 2), "ETH absorbed some");
    }

    #[test]
    fn a_capped_long_never_spills_into_the_shorts() {
        let mut cfg = config();
        cfg.limits.participation_limit = Decimal::new(5, 2);
        let nav = Decimal::from(600_000);
        let profile = thin_profile("KAITO", 200_000);
        let mut weights = BTreeMap::from([
            ("KAITO".to_string(), Decimal::new(10, 2)),
            ("BTC".to_string(), Decimal::new(10, 2)),
            ("WLD".to_string(), Decimal::new(-10, 2)),
        ]);
        cap_by_participation(&mut weights, &cfg, nav, Some(&profile));
        assert_eq!(
            weights["WLD"],
            Decimal::new(-10, 2),
            "answering 'this long is too thin' by changing net exposure is not an answer"
        );
    }

    #[test]
    fn with_no_profile_or_no_limit_the_book_is_left_exactly_as_it_was() {
        let mut cfg = config();
        let nav = Decimal::from(600_000);
        let profile = thin_profile("KAITO", 200_000);
        let original = BTreeMap::from([
            ("KAITO".to_string(), Decimal::new(10, 2)),
            ("BTC".to_string(), Decimal::new(10, 2)),
        ]);

        // No limit configured: the shipped behaviour.
        cfg.limits.participation_limit = Decimal::ZERO;
        let mut w = original.clone();
        assert!(cap_by_participation(&mut w, &cfg, nav, Some(&profile)).is_empty());
        assert_eq!(w, original);

        // Limit configured but nothing measured: a name we have not looked at
        // must not be treated as illiquid.
        cfg.limits.participation_limit = Decimal::new(5, 2);
        let mut w = original.clone();
        assert!(cap_by_participation(&mut w, &cfg, nav, None).is_empty());
        assert_eq!(w, original);
    }

    #[test]
    fn a_side_that_is_entirely_capped_says_so_rather_than_overfilling() {
        let mut cfg = config();
        cfg.limits.participation_limit = Decimal::new(5, 2);
        let nav = Decimal::from(600_000);
        let mut profile = thin_profile("KAITO", 200_000);
        profile.assets.insert(
            "PUMP".into(),
            liquidity::AssetLiquidity {
                hourly_quote_volume: Some(Decimal::from(200_000)),
                ..Default::default()
            },
        );
        let mut weights = BTreeMap::from([
            ("KAITO".to_string(), Decimal::new(20, 2)),
            ("PUMP".to_string(), Decimal::new(20, 2)),
        ]);
        let notes = cap_by_participation(&mut weights, &cfg, nav, Some(&profile));
        let cap = Decimal::new(5, 2) * Decimal::from(200_000) / nav;
        assert_eq!(weights["KAITO"].round_dp(6), cap.round_dp(6));
        assert_eq!(weights["PUMP"].round_dp(6), cap.round_dp(6));
        assert!(
            notes.iter().any(|n| n.contains("unplaced")),
            "idle capital must be stated, not silently absorbed: {notes:?}"
        );
    }

    #[test]
    fn turnover_budget_selects_largest_drift_before_reason_ordering() {
        let mut cfg = config();
        cfg.turnover_budget = Decimal::new(4, 1);
        cfg.limits.min_position_notional = Decimal::ZERO;
        let weights = BTreeMap::from([
            ("A".into(), Decimal::new(2, 1)),
            ("B".into(), Decimal::new(45, 2)),
            ("C".into(), Decimal::new(1, 1)),
        ]);
        let current = BTreeMap::from([
            ("A".into(), Decimal::new(3, 1)),
            ("B".into(), Decimal::new(1, 1)),
            ("C".into(), Decimal::new(3, 1)),
        ]);
        let prices = BTreeMap::from([
            ("A".into(), Decimal::from(100)),
            ("B".into(), Decimal::from(100)),
            ("C".into(), Decimal::from(100)),
        ]);
        let adv = BTreeMap::from([
            ("A".into(), Some(Decimal::from(100_000_000))),
            ("B".into(), Some(Decimal::from(100_000_000))),
            ("C".into(), Some(Decimal::from(100_000_000))),
        ]);
        let vol = BTreeMap::from([
            ("A".into(), Some(Decimal::new(5, 1))),
            ("B".into(), Some(Decimal::new(5, 1))),
            ("C".into(), Some(Decimal::new(5, 1))),
        ]);

        let (orders, _, skipped, used, dropped) = diff(
            &weights,
            &current,
            &Market {
                prices: &prices,
                adv: &adv,
                vol: &vol,
                profile: None,
            },
            Decimal::from(100_000),
            &cfg,
        );

        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].asset, "B");
        assert_eq!(orders[0].reason, OrderReason::Increase);
        assert_eq!(used, Decimal::new(35, 2));
        assert_eq!(dropped, Decimal::new(3, 1));
        assert!(skipped.iter().any(|message| message.starts_with("A:")));
        assert!(skipped.iter().any(|message| message.starts_with("C:")));
    }

    #[test]
    fn momentum_uses_average_tie_ranks_and_neutral_nulls() {
        let mut cfg = config();
        cfg.signal = "xs_momentum".into();
        cfg.max_holdings = 5;
        cfg.min_cross_section = 5;
        let ts = "2026-08-01T00:00:00Z".parse().unwrap();
        let mut rows = [
            row("AAA", ts, 10.0),
            row("BBB", ts, 10.0),
            row("CCC", ts, 10.0),
            row("DDD", ts, 10.0),
            row("EEE", ts, 10.0),
        ];
        rows[0].ret_30_skip_7 = Some(3.0);
        rows[1].ret_30_skip_7 = Some(3.0);
        rows[2].ret_30_skip_7 = Some(1.0);
        rows[3].ret_30_skip_7 = Some(2.0);
        rows[4].ret_30_skip_7 = None;
        let refs: Vec<_> = rows.iter().collect();
        let out = generate_signal(&refs, &BTreeMap::new(), ts.date_naive(), &cfg, None).unwrap();
        let scores: BTreeMap<_, _> = out
            .values
            .iter()
            .map(|s| (s.asset.as_str(), s.conviction))
            .collect();
        assert_eq!(scores["AAA"], Decimal::from(75));
        assert_eq!(scores["BBB"], Decimal::from(75));
        assert_eq!(scores["EEE"], Decimal::from(50));
        assert!(out
            .warnings
            .iter()
            .any(|w| w.message.contains("EEE") && w.message.contains("scored neutral")));
    }

    #[test]
    fn long_short_tilt_never_exceeds_the_name_budget() {
        let mut cfg = config();
        cfg.signal = "gc_long_short".into();
        cfg.limits.max_position_count = 6;
        cfg.benchmark = Some("BTC".into());
        let ts = "2026-08-01T00:00:00Z".parse().unwrap();
        let mut rows = Vec::new();
        for i in 0..10 {
            let mut r = row(&format!("L{i:02}"), ts, 100.0);
            r.gc_breakout_age = Some(i);
            rows.push(r);
        }
        for i in 0..10 {
            rows.push(row(&format!("S{i:02}"), ts, 100.0 - i as f64));
        }
        let mut benchmark = row("BTC", ts, 120.0);
        benchmark.gc_regime_upper = Some(110.0);
        benchmark.gc_regime_filter = Some(100.0);
        benchmark.gc_regime_slope = Some(1.0);
        rows.push(benchmark);
        let refs: Vec<_> = rows.iter().collect();
        let out = generate_signal(&refs, &BTreeMap::new(), ts.date_naive(), &cfg, None).unwrap();
        assert_eq!(out.values.len(), 6);
        assert_eq!(
            out.values
                .iter()
                .filter(|s| s.direction == Direction::Long)
                .count(),
            3
        );
        assert_eq!(
            out.values
                .iter()
                .filter(|s| s.direction == Direction::Short)
                .count(),
            3
        );
    }

    #[test]
    fn risk_adjusted_is_two_sided_and_scales_the_whole_book_at_the_cap() {
        let mut cfg = config();
        cfg.constructor = "risk_adjusted".into();
        cfg.target_gross_exposure = Decimal::ONE;
        cfg.limits.max_position = Decimal::new(2, 1);
        let signals = vec![
            Signal {
                asset: "L1".into(),
                direction: Direction::Long,
                conviction: Decimal::new(2, 2),
                volatility: Some(Decimal::new(1, 1)),
            },
            Signal {
                asset: "L2".into(),
                direction: Direction::Long,
                conviction: Decimal::new(1, 2),
                volatility: Some(Decimal::new(2, 1)),
            },
            Signal {
                asset: "S1".into(),
                direction: Direction::Short,
                conviction: Decimal::new(2, 2),
                volatility: Some(Decimal::new(1, 1)),
            },
            Signal {
                asset: "S2".into(),
                direction: Direction::Short,
                conviction: Decimal::new(1, 2),
                volatility: Some(Decimal::new(2, 1)),
            },
        ];
        let out = construct(&signals, &cfg, Decimal::from(100_000), None).unwrap();
        assert_eq!(out.weights.len(), 4);
        assert_eq!(
            out.weights.values().map(|w| w.abs()).max(),
            Some(Decimal::new(2, 1))
        );
        assert_eq!(
            out.weights.values().filter(|w| **w > Decimal::ZERO).count(),
            2
        );
        assert_eq!(
            out.weights.values().filter(|w| **w < Decimal::ZERO).count(),
            2
        );
    }

    #[test]
    fn rust_plan_round_trips_through_the_executor_contract() {
        let mut cfg = config();
        cfg.limits.max_cluster_exposure = None;
        let as_of = "2026-08-01T00:00:00Z".parse().unwrap();
        let features = vec![
            row("BTC", as_of - TimeDelta::days(1), 100.0),
            row("ETH", as_of - TimeDelta::days(1), 50.0),
        ];
        let universe = ["BTC".into(), "ETH".into()].into_iter().collect();
        let portfolio = Portfolio {
            cash: Decimal::from(100_000),
            positions: Vec::new(),
        };
        let out = decide(DecisionInput {
            as_of,
            created_at: as_of,
            mode: Mode::Live,
            config: &cfg,
            daily_features: &features,
            hourly_features: None,
            eligible_universe: &universe,
            portfolio: &portfolio,
            inputs_hash: "fixture",
        })
        .unwrap();
        assert_eq!(out.plan.status, Status::Accepted);
        assert_eq!(out.plan.orders.len(), 2);
        Plan::parse(&canonical_json(&out.plan).unwrap()).unwrap();
    }

    #[test]
    fn rejected_plan_has_no_orders() {
        let mut cfg = config();
        cfg.limits.max_gross_exposure = Decimal::new(1, 2);
        cfg.limits.max_cluster_exposure = None;
        let as_of = "2026-08-01T00:00:00Z".parse().unwrap();
        let features = vec![row("BTC", as_of - TimeDelta::days(1), 100.0)];
        let universe = ["BTC".into()].into_iter().collect();
        let portfolio = Portfolio {
            cash: Decimal::from(100_000),
            positions: Vec::new(),
        };
        let out = decide(DecisionInput {
            as_of,
            created_at: as_of,
            mode: Mode::Live,
            config: &cfg,
            daily_features: &features,
            hourly_features: None,
            eligible_universe: &universe,
            portfolio: &portfolio,
            inputs_hash: "fixture",
        })
        .unwrap();
        assert_eq!(out.plan.status, Status::Rejected);
        assert!(out.plan.orders.is_empty());
    }
}
