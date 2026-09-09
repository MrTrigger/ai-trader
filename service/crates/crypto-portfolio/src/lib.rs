//! Rust crypto portfolio decision core.
//!
//! This crate owns the deterministic path from Rust-computed feature rows and
//! venue truth to an immutable Plan. It has no venue adapter and cannot submit
//! orders. The executor remains a separate process and only parses the Plan.

pub mod backtest;
pub mod binance;
pub mod binance_archive;
pub mod config;
pub mod directional;
pub mod execution;
pub mod experiment;
pub mod funding;
pub mod gate;
pub mod ic;
pub mod inspect;
pub mod lighter_execution;
pub mod lighter_history;
pub mod lighter_hourly;
pub mod liquidity;
pub mod market_context;
pub mod model;
pub mod model_schedule;
pub mod monte_carlo;
pub mod replay_suite;
pub mod report;
pub mod research;
pub mod scores;
pub mod selection_timing;
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

fn adjust_native_funding(
    generated: &mut Signals,
    forecast: &funding::NativeForecast,
) -> Result<(), String> {
    for (asset, estimate) in &forecast.assets {
        if let funding::NativeEstimate::Unavailable { observed_hours } = estimate {
            generated.warnings.push(warning(WarningKind::Other, format!(
                "native funding unavailable for {asset} at {}: {observed_hours}/168 preceding hourly rates; price conviction unchanged",
                forecast.as_of,
            )));
        }
    }
    let mut adjusted = Vec::new();
    for mut signal in generated.values.drain(..) {
        match forecast.assets.get(&signal.asset).ok_or_else(|| {
            format!(
                "native funding forecast has no availability record for {}",
                signal.asset,
            )
        })? {
            funding::NativeEstimate::Available { long_cost_24h } => {
                let Some(net) = funding::native_net_conviction_24h(
                    signal.conviction,
                    signal.direction,
                    *long_cost_24h,
                ) else {
                    continue;
                };
                signal.conviction = net;
            }
            funding::NativeEstimate::Unavailable { .. } => {}
        }
        adjusted.push(signal);
    }
    generated.values = adjusted;
    generated.scoring_version = format!("{}+native-carry-7d-1", generated.scoring_version);
    generated.warnings.push(warning(WarningKind::Other,
        "native carry adjusts model price conviction after unchanged feature normalization: signed trailing168 hourly percentage rates /100 /7; favorable carry is rewarded, adverse carry deducted, model directions never flipped. Realized funding remains a separate cash flow."));
    Ok(())
}

fn directional_agreement(signals: Vec<Signal>, confirmation: &[Signal]) -> Vec<Signal> {
    signals
        .into_iter()
        .filter(|signal| {
            confirmation
                .iter()
                .any(|other| other.asset == signal.asset && other.direction == signal.direction)
        })
        .collect()
}

fn generate_signal(
    cross: &[&DailyRow],
    hourly: &BTreeMap<String, &HourlyRow>,
    score_date: chrono::NaiveDate,
    cfg: &Config,
    profile: Option<&liquidity::Profile>,
    btc_context: Option<&DailyRow>,
) -> Result<Signals, String> {
    let (mut rows, mut notes) = eligible(cross.iter().copied(), cfg, profile)?;
    if cfg.benchmark_only {
        rows.retain(|row| Some(&row.asset) == cfg.benchmark.as_ref());
        notes.push("Directional signal restricted to the eligible benchmark asset.".into());
    }
    let mut warnings = Vec::new();
    let mut scoring_version = "none".to_string();
    let mut model_id = None;

    let mut values = match cfg.signal.as_str() {
        "benchmark_consensus" => {
            let mut component = cfg.clone();
            component.signal = "tsmom_ensemble".into();
            let momentum =
                generate_signal(cross, hourly, score_date, &component, profile, btc_context)?;
            component.signal = "market_model_tilt".into();
            let timing =
                generate_signal(cross, hourly, score_date, &component, profile, btc_context)?;
            scoring_version = "benchmark-momentum-timing-consensus-1".into();
            model_id = timing.model_id;
            notes.extend(momentum.notes);
            notes.extend(timing.notes);
            notes.push("Hold the benchmark only when slow momentum and the purged market timer agree on direction; otherwise target cash.".into());
            warnings.extend(momentum.warnings);
            warnings.extend(timing.warnings);
            directional_agreement(momentum.values, &timing.values)
        }
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
        "tsmom_ensemble" | "tsmom_fast_ensemble" => {
            // crypto-directional round 1 (docs/crypto-directional-design.md):
            // Per coin, mean sign(ret_h) over {30, 90, 180} days;
            // the faster research variant uses {7, 30, 90} days.
            // Deterministic; no model, no cross-sectional demeaning - the
            // sign IS the market view, which is this bot's whole point.
            // Universe: the top max_holdings by rank-day liquidity among
            // eligible names; a name missing any horizon or vol is skipped
            // by name (young listings), never scored neutral.
            let fast = cfg.signal == "tsmom_fast_ensemble";
            scoring_version = if fast {
                "tsmom-fast-ensemble-1"
            } else {
                "tsmom-ensemble-1"
            }
            .into();
            rows.sort_by(|a, b| {
                b.adv_quote
                    .unwrap_or(0.0)
                    .total_cmp(&a.adv_quote.unwrap_or(0.0))
                    .then_with(|| a.asset.cmp(&b.asset))
            });
            let mut out = Vec::new();
            for r in rows.iter().take(cfg.max_holdings) {
                let horizons = if fast {
                    (r.ret_7, r.ret_30, r.ret_90)
                } else {
                    (r.ret_30, r.ret_90, r.ret_180)
                };
                let (Some(short), Some(medium), Some(long), Some(vol)) =
                    (horizons.0, horizons.1, horizons.2, r.vol_30)
                else {
                    notes.push(format!(
                        "{}: missing a trend horizon or vol; not scored",
                        r.asset
                    ));
                    continue;
                };
                if vol <= 0.0 {
                    notes.push(format!("{}: non-positive vol; not scored", r.asset));
                    continue;
                }
                let s = (short.signum() + medium.signum() + long.signum()) / 3.0;
                if s == 0.0 {
                    notes.push(format!("{}: horizons disagree to zero; flat", r.asset));
                    continue;
                }
                out.push(Signal {
                    asset: r.asset.clone(),
                    direction: if s > 0.0 {
                        Direction::Long
                    } else {
                        Direction::Short
                    },
                    conviction: d(s.abs())?,
                    volatility: Some(d(vol)?),
                });
            }
            out
        }
        "donchian_ensemble" | "donchian_ensemble_ls" => {
            // Round-2 V1/V2 (docs/crypto-directional-design.md): the
            // Donchian ensemble state computed causally in features-crypto.
            // V1: long-flat, conviction = share of the nine sub-strategies
            // long. V2: conviction = |long share - short share|, direction
            // its sign. Universe: top max_holdings by rank-day liquidity.
            scoring_version = "donchian-ensemble-1".into();
            let ls = cfg.signal == "donchian_ensemble_ls";
            rows.sort_by(|a, b| {
                b.adv_quote
                    .unwrap_or(0.0)
                    .total_cmp(&a.adv_quote.unwrap_or(0.0))
                    .then_with(|| a.asset.cmp(&b.asset))
            });
            let mut out = Vec::new();
            for r in rows.iter().take(cfg.max_holdings) {
                let (Some(long), Some(short)) = (r.don_long, r.don_short) else {
                    notes.push(format!("{}: donchian state not warm; not scored", r.asset));
                    continue;
                };
                let vol = r.vol_90.or(r.vol_30);
                let Some(vol) = vol.filter(|v| *v > 0.0) else {
                    notes.push(format!("{}: no volatility estimate; not scored", r.asset));
                    continue;
                };
                let s = if ls { long - short } else { long };
                if s == 0.0 {
                    continue; // flat is a real state, not a signal
                }
                out.push(Signal {
                    asset: r.asset.clone(),
                    direction: if s > 0.0 {
                        Direction::Long
                    } else {
                        Direction::Short
                    },
                    conviction: d(s.abs())?,
                    volatility: Some(d(vol)?),
                });
            }
            out
        }
        "donchian_strength_select" => {
            // Round-2 V6: cross-sectional selection by trend STRENGTH. From
            // the top-30 by liquidity, rank by |don_long - don_short| and
            // keep the 10 strongest with a nonzero net signal, each in its
            // own direction - concentrated where the directional potential
            // is, instead of holding everything the liquidity screen admits.
            scoring_version = "donchian-strength-1".into();
            rows.sort_by(|a, b| {
                b.adv_quote
                    .unwrap_or(0.0)
                    .total_cmp(&a.adv_quote.unwrap_or(0.0))
                    .then_with(|| a.asset.cmp(&b.asset))
            });
            let mut scored: Vec<(&&DailyRow, f64, f64)> = Vec::new();
            for r in rows.iter().take(cfg.selection_pool) {
                let (Some(long), Some(short)) = (r.don_long, r.don_short) else {
                    continue;
                };
                let Some(vol) = r.vol_90.or(r.vol_30).filter(|v| *v > 0.0) else {
                    continue;
                };
                let s = long - short;
                if s != 0.0 {
                    scored.push((r, s, vol));
                }
            }
            scored.sort_by(|a, b| {
                b.1.abs()
                    .total_cmp(&a.1.abs())
                    .then_with(|| a.0.asset.cmp(&b.0.asset))
            });
            scored.truncate(cfg.max_holdings);
            let mut out = Vec::new();
            for (r, s, vol) in scored {
                out.push(Signal {
                    asset: r.asset.clone(),
                    direction: if s > 0.0 {
                        Direction::Long
                    } else {
                        Direction::Short
                    },
                    conviction: d(s.abs())?,
                    volatility: Some(d(vol)?),
                });
            }
            out
        }
        "tsmom_z" => {
            // Round-2 V3: slow TSMOM with a CONTINUOUS clipped response -
            // z_h = ret_h / (sigma_ann * sqrt(h/365)) per horizon h in
            // {30, 90, 180}, response = clip(mean z, +-2) / 2 in [-1, 1].
            scoring_version = "tsmom-z-1".into();
            rows.sort_by(|a, b| {
                b.adv_quote
                    .unwrap_or(0.0)
                    .total_cmp(&a.adv_quote.unwrap_or(0.0))
                    .then_with(|| a.asset.cmp(&b.asset))
            });
            let mut out = Vec::new();
            for r in rows.iter().take(cfg.max_holdings) {
                let (Some(r30), Some(r90), Some(r180), Some(vol)) =
                    (r.ret_30, r.ret_90, r.ret_180, r.vol_90.or(r.vol_30))
                else {
                    notes.push(format!("{}: missing a horizon or vol; not scored", r.asset));
                    continue;
                };
                if vol <= 0.0 {
                    continue;
                }
                let z = |ret: f64, h: f64| ret / (vol * (h / 365.0).sqrt());
                let mean_z = (z(r30, 30.0) + z(r90, 90.0) + z(r180, 180.0)) / 3.0;
                let resp = (mean_z.clamp(-2.0, 2.0)) / 2.0;
                if resp == 0.0 {
                    continue;
                }
                out.push(Signal {
                    asset: r.asset.clone(),
                    direction: if resp > 0.0 {
                        Direction::Long
                    } else {
                        Direction::Short
                    },
                    conviction: d(resp.abs())?,
                    volatility: Some(d(vol)?),
                });
            }
            out
        }
        "market_model_tilt" => {
            // Round-2 V4a: one market-level prediction per day from the
            // timing model, applied as a uniform signed conviction across
            // the top max_holdings by liquidity - the model decides the
            // lean, the vol-parity constructor sizes it. Features come from
            // the pre-built market matrix (the same rows training used), so
            // train and inference cannot disagree on a feature.
            scoring_version = "market-timing-1".into();
            let matrix_path = cfg
                .market_matrix_path
                .as_deref()
                .ok_or("market_model_tilt needs market_matrix_path")?;
            let model_path = cfg
                .timing_model_path
                .as_deref()
                .ok_or("market_model_tilt needs timing_model_path")?;
            let matrix = directional::read_matrix(std::path::Path::new(matrix_path))?;
            let model = directional::TimingModel::load(std::path::Path::new(model_path))?;
            model_id = Some(format!("timing:{}", model.trained_through));
            let Some(row) = matrix.iter().rev().find(|r| r.date <= score_date) else {
                notes.push(format!(
                    "no market row at or before {score_date}; target is flat"
                ));
                return Ok(Signals {
                    values: Vec::new(),
                    notes,
                    warnings,
                    scoring_version,
                    model_id,
                });
            };
            if (score_date - row.date).num_days() > 3 {
                notes.push(format!(
                    "market row {} is stale for {score_date}; target is flat",
                    row.date
                ));
                return Ok(Signals {
                    values: Vec::new(),
                    notes,
                    warnings,
                    scoring_version,
                    model_id,
                });
            }
            let resp = model.response(row)?;
            notes.push(format!(
                "timing model: market row {} -> response {:.3}",
                row.date, resp
            ));
            if resp == 0.0 {
                Vec::new()
            } else {
                rows.sort_by(|a, b| {
                    b.adv_quote
                        .unwrap_or(0.0)
                        .total_cmp(&a.adv_quote.unwrap_or(0.0))
                        .then_with(|| a.asset.cmp(&b.asset))
                });
                let mut out = Vec::new();
                for r in rows.iter().take(cfg.max_holdings) {
                    let Some(vol) = r.vol_90.or(r.vol_30).filter(|v| *v > 0.0) else {
                        continue;
                    };
                    out.push(Signal {
                        asset: r.asset.clone(),
                        direction: if resp > 0.0 {
                            Direction::Long
                        } else {
                            Direction::Short
                        },
                        conviction: d(resp.abs())?,
                        volatility: Some(d(vol)?),
                    });
                }
                out
            }
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
            let trade_model = if cfg.ranker_trade_assets.is_some() {
                let path = cfg
                    .model_path
                    .as_ref()
                    .ok_or("ml_ranker requires model_path; no fallback model is permitted")?;
                let model = model::Model::load(std::path::Path::new(path))?;
                if model.reward != "per_risk_abs" || model.label_horizon_hours != Some(24) {
                    return Err(
                        "ranker_trade_assets requires an explicit 24h per_risk_abs model".into(),
                    );
                }
                Some(model)
            } else {
                None
            };
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
                let model = match trade_model {
                    Some(model) => model,
                    None => model::Model::load(std::path::Path::new(path))?,
                };
                if cfg.constructor == "ranker_cost_aware_vol_parity"
                    && model.label_horizon_hours != Some(24)
                {
                    return Err(
                        "ranker_cost_aware_vol_parity requires an explicit 24h model label horizon"
                            .into(),
                    );
                }
                if cfg.native_funding_adjustment && model.label_horizon_hours != Some(24) {
                    return Err(
                        "native_funding_adjustment requires an explicit 24h model label horizon"
                            .into(),
                    );
                }
                if cfg.funding_cost_adjustment
                    && model.label_horizon_hours.is_some_and(|hours| hours != 24)
                {
                    return Err("funding_cost_adjustment requires a 24h model label horizon".into());
                }
                let raw: Vec<_> = model
                    .ranked_features()
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
                let mut normalised = features_crypto::rank_normalise(&matrix)?;
                if model.market_context.is_some() {
                    let context = market_context::values(btc_context, score_date)?;
                    for values in &mut normalised {
                        values.extend(context);
                    }
                }
                // Legacy models use the completed feature date as their
                // one-day label embargo. Explicit label windows are checked
                // against the decision date by Model::predict itself.
                let model_date = if model.label_horizon_hours.is_some() {
                    score_date.succ_opt().ok_or("decision date overflow")?
                } else {
                    score_date
                };
                let mut out = Vec::new();
                for (row, values) in rows.iter().zip(normalised) {
                    let score = model.predict(&values, model_date)?;
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
                        "per_risk" | "per_risk_abs" => score.abs() * volatility,
                        _ => score.abs(),
                    };
                    let direction = if score > 0.0 {
                        Direction::Long
                    } else {
                        Direction::Short
                    };
                    let mut conviction = d(expected_return)?;
                    conviction = htf_conviction(
                        conviction,
                        direction,
                        row.ret_30,
                        row.ret_90,
                        cfg.ranker_trend_weight,
                    )?;
                    if cfg.funding_cost_adjustment {
                        if row.ts_utc.date_naive() > score_date {
                            return Err(format!(
                                "{} funding row is newer than the completed feature date",
                                row.asset
                            ));
                        }
                        let Some(net) =
                            funding::net_conviction_24h(conviction, direction, row.funding_7d)?
                        else {
                            continue;
                        };
                        conviction = net;
                    }
                    out.push(Signal {
                        asset: row.asset.clone(),
                        direction,
                        conviction,
                        volatility: Some(d(volatility)?),
                    });
                }
                notes.push(format!(
                    "scored {} eligible assets with {} trained through {}",
                    out.len(),
                    model.model_version,
                    model.trained_through
                ));
                if cfg.funding_cost_adjustment {
                    notes.push("ranker conviction subtracts adverse 24h funding estimated as trailing funding_7d / 7; favorable carry is not rewarded; historical missing rates remain zero-imputed".into());
                }
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
    if let Some(assets) = &cfg.ranker_trade_assets {
        let scored = values.len();
        values.retain(|signal| assets.contains(&signal.asset));
        notes.push(format!(
            "ranker trade assets {assets:?}: retained {} of {scored} signals; omitted {}; scoring cohort unchanged",
            values.len(), scored - values.len()
        ));
    }
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

struct DirectionalComponents {
    relative: Config,
    directional: Config,
    relative_model_id: String,
    directional_model_id: String,
}

fn directional_component_configs(
    cfg: &Config,
    decision_date: chrono::NaiveDate,
) -> Result<Option<DirectionalComponents>, String> {
    let Some(path) = &cfg.ranker_directional_model_path else {
        return Ok(None);
    };
    let mut relative = cfg.clone();
    relative.ranker_directional_model_path = None;
    relative.target_gross_exposure = Decimal::new(8, 1);
    relative.limits.max_gross_exposure = Decimal::ONE;
    relative.limits.max_position = Decimal::new(25, 2);
    relative.limits.max_net_exposure = Some(Decimal::new(1, 1));
    relative.limits.max_cluster_exposure = Some(Decimal::new(4, 1));
    relative.limits.max_benchmark_beta = Some(Decimal::new(125, 2));
    let mut directional = relative.clone();
    directional.model_path = Some(path.clone());
    directional.model_schedule = None;
    directional.constructor = "tsmom_vol_parity".into();
    directional.limits.max_net_exposure = Some(Decimal::ONE);
    directional.limits.max_cluster_exposure = Some(Decimal::ONE);
    let mut ids = Vec::new();
    for (name, component, reward) in [
        ("relative", &relative, "per_risk"),
        ("directional", &directional, "per_risk_abs"),
    ] {
        let model = model::Model::load(std::path::Path::new(
            component
                .model_path
                .as_deref()
                .ok_or("directional overlay requires a primary model")?,
        ))?;
        if model.reward != reward || model.label_horizon_hours != Some(24) {
            return Err(format!("directional overlay {name} model requires {reward} reward and explicit 24h horizon"));
        }
        model.ensure_available(decision_date)?;
        ids.push(model.model_id);
    }
    Ok(Some(DirectionalComponents {
        relative,
        directional,
        relative_model_id: ids.remove(0),
        directional_model_id: ids.remove(0),
    }))
}

fn additive_directional_targets(relative: Construction, directional: Construction) -> Construction {
    let mut weights = BTreeMap::new();
    let mut notes = Vec::new();
    for (name, component) in [("relative", relative), ("directional", directional)] {
        notes.push(format!(
            "directional overlay {name} full shared-NAV targets {:?}",
            component.weights
        ));
        notes.extend(
            component
                .notes
                .into_iter()
                .map(|note| format!("{name}: {note}")),
        );
        for (asset, weight) in component.weights {
            *weights.entry(asset).or_default() += weight;
        }
    }
    weights.retain(|_, weight: &mut Decimal| !weight.is_zero());
    notes.push("directional overlay adds two full mandates without rescaling; one account nets orders, costs and funding".into());
    Construction {
        weights,
        notes,
        constructor: "ranker_directional_additive".into(),
    }
}

fn blend_configs(cfg: &Config) -> Option<(Decimal, Config, Config)> {
    let fraction = cfg
        .ranker_btc_momentum_fraction
        .filter(|fraction| *fraction > Decimal::ZERO)?;
    Some(blend_component_configs(cfg, fraction))
}

fn blend_component_configs(cfg: &Config, fraction: Decimal) -> (Decimal, Config, Config) {
    let mut ranker = cfg.clone();
    ranker.ranker_btc_momentum_fraction = None;
    ranker.ranker_btc_regime = false;
    ranker.target_gross_exposure = Decimal::new(8, 1);
    ranker.limits.max_gross_exposure = Decimal::ONE;
    ranker.limits.max_position = Decimal::new(25, 2);
    ranker.limits.max_net_exposure = Some(Decimal::new(1, 1));
    ranker.limits.max_cluster_exposure = Some(Decimal::new(4, 1));
    ranker.limits.max_benchmark_beta = Some(Decimal::new(125, 2));
    let mut momentum = ranker.clone();
    momentum.signal = "tsmom_ensemble".into();
    momentum.constructor = "tsmom_vol_parity".into();
    momentum.benchmark_only = true;
    momentum.max_holdings = 1;
    momentum.limits.capacity_balance = false;
    momentum.limits.max_position = Decimal::new(8, 1);
    momentum.limits.max_net_exposure = Some(Decimal::new(8, 1));
    momentum.limits.max_cluster_exposure = Some(Decimal::new(8, 1));
    (fraction, ranker, momentum)
}

fn veto_positive_trend_shorts(
    construction: &mut Construction,
    latest: &BTreeMap<String, &DailyRow>,
    horizon: DateTime<Utc>,
) -> Result<Vec<String>, String> {
    let mut vetoes = Vec::new();
    for (asset, weight) in &construction.weights {
        if *weight >= Decimal::ZERO {
            continue;
        }
        let row = latest
            .get(asset)
            .filter(|row| row.ts_utc == horizon)
            .ok_or_else(|| {
                format!("ranker_short_trend_veto requires {asset} at completed horizon {horizon}")
            })?;
        let ret = row
            .ret_30
            .filter(|value| value.is_finite())
            .ok_or_else(|| {
                format!("ranker_short_trend_veto requires finite {asset} ret_30 at {horizon}")
            })?;
        if ret > 0.0 {
            vetoes.push((asset.clone(), format!(
                "short trend veto: {asset} target {weight} removed; completed ret_30 {ret} at {horizon}"
            )));
        }
    }
    for (asset, _) in &vetoes {
        construction.weights.remove(asset);
    }
    Ok(vetoes.into_iter().map(|(_, note)| note).collect())
}

fn btc_regime_signal(
    row: Option<&DailyRow>,
    horizon: DateTime<Utc>,
) -> Result<(Decimal, Signal), String> {
    let row = row
        .filter(|r| r.asset == "BTC" && r.ts_utc == horizon)
        .ok_or("ranker_btc_regime requires BTC at the prior completed daily horizon")?;
    let ret = row
        .ret_30
        .filter(|v| v.is_finite())
        .ok_or("ranker_btc_regime requires finite BTC ret_30")?;
    let vol = row
        .vol_30
        .filter(|v| v.is_finite() && *v > 0.0)
        .ok_or("ranker_btc_regime requires finite positive BTC vol_30")?;
    Ok((
        if ret > 0.0 {
            Decimal::ONE
        } else {
            Decimal::ZERO
        },
        Signal {
            asset: "BTC".into(),
            direction: Direction::Long,
            conviction: Decimal::ONE,
            volatility: Some(d(vol)?),
        },
    ))
}

fn blend_targets(ranker: Construction, momentum: Construction, fraction: Decimal) -> Construction {
    let mut weights = BTreeMap::new();
    let mut notes = Vec::new();
    for (name, component, share) in [
        ("ranker", ranker, Decimal::ONE - fraction),
        ("btc_momentum", momentum, fraction),
    ] {
        let targets: BTreeMap<_, _> = component
            .weights
            .into_iter()
            .map(|(asset, weight)| (asset, weight * share))
            .collect();
        notes.push(format!(
            "blend {name} capital fraction {share}; shared-NAV targets {targets:?}"
        ));
        notes.extend(
            component
                .notes
                .into_iter()
                .map(|note| format!("{name}: {note}")),
        );
        for (asset, weight) in targets {
            *weights.entry(asset).or_default() += weight;
        }
    }
    weights.retain(|_, weight: &mut Decimal| !weight.is_zero());
    notes.push("blend nets component targets before one shared risk check, turnover budget and resize deadband; fills and funding belong to one account".into());
    Construction {
        weights,
        notes,
        constructor: "ranker_btc_momentum_blend".into(),
    }
}

fn htf_conviction(
    conviction: Decimal,
    direction: Direction,
    ret_30: Option<f64>,
    ret_90: Option<f64>,
    weight: Decimal,
) -> Result<Decimal, String> {
    if weight == Decimal::ZERO {
        return Ok(conviction);
    }
    let (Some(a), Some(b)) = (ret_30, ret_90) else {
        return Ok(conviction);
    };
    if !a.is_finite() || !b.is_finite() {
        return Err("HTF returns must be finite".into());
    }
    let sign = |v: f64| {
        if v > 0.0 {
            1.0
        } else if v < 0.0 {
            -1.0
        } else {
            0.0
        }
    };
    let alignment = d((sign(a) + sign(b)) / 2.0)?;
    let side = if direction == Direction::Long {
        Decimal::ONE
    } else {
        -Decimal::ONE
    };
    Ok(conviction * (Decimal::ONE + weight * side * alignment))
}

/// Breadth trend for the trend overlay (`docs/research/trend-overlay.md`):
/// for h in {30, 90}, `b_h = 2 * share(ret_h > 0) - 1` over the eligible
/// universe's latest rows; `trend = (b_30 + b_90) / 2` in [-1, 1]. `None`
/// if either horizon has no row with a value (the rule cannot be evaluated,
/// and the constructor that needs it refuses rather than assuming flat).
fn breadth_trend(cross: &[&DailyRow], overlay: &config::Overlay) -> Option<Decimal> {
    fn breadth(values: impl Iterator<Item = Option<f64>>) -> Option<f64> {
        let v: Vec<f64> = values.flatten().collect();
        if v.is_empty() {
            return None;
        }
        let up = v.iter().filter(|x| **x > 0.0).count() as f64;
        Some(2.0 * up / v.len() as f64 - 1.0)
    }
    let ret_h = |r: &DailyRow, h: u32| -> Option<f64> {
        match h {
            7 => r.ret_7,
            30 => r.ret_30,
            90 => r.ret_90,
            180 => r.ret_180,
            _ => None,
        }
    };
    let ba = breadth(cross.iter().map(|r| ret_h(r, overlay.horizon_a)))?;
    let bb = breadth(cross.iter().map(|r| ret_h(r, overlay.horizon_b)))?;
    let mut trend = (ba + bb) / 2.0;
    if let Some(target) = overlay.vol_target {
        // Sweep variant: scale the tilt down when the universe is volatile.
        let mut vols: Vec<f64> = cross
            .iter()
            .filter_map(|r| r.vol_30)
            .filter(|v| *v > 0.0)
            .collect();
        if vols.is_empty() {
            return None;
        }
        vols.sort_by(|a, b| a.total_cmp(b));
        let med = vols[vols.len() / 2];
        let target: f64 = target.to_string().parse().ok()?;
        trend *= (target / med).min(1.0);
    }
    d(trend).ok()
}

#[cfg_attr(not(test), allow(dead_code))]
fn construct(
    signals: &[Signal],
    cfg: &Config,
    nav: Decimal,
    profile: Option<&liquidity::Profile>,
) -> Result<Construction, String> {
    construct_with_tilt(signals, cfg, nav, profile, None)
}

/// Staggered sleeves: with N tranches, each asset belongs to one fixed
/// sleeve (a stable hash of its symbol) and only the sleeve whose turn it is
/// today may change weight; every other asset keeps its current weight,
/// including a pending exit, until its day comes round. Each name therefore
/// holds a one-day forecast for N days, which is the cadence research picked
/// (docs/research/cadence.json) but which was never migrated to Rust. Risk
/// limits still apply to the whole resulting book.
fn apply_tranches(
    construction: &mut Construction,
    current: &BTreeMap<String, Decimal>,
    cfg: &Config,
    as_of: DateTime<Utc>,
) -> Vec<String> {
    let tranches = cfg.ranker_tranches;
    let today = (as_of.timestamp().div_euclid(86_400)).rem_euclid(tranches as i64) as usize;
    let sleeve = |asset: &str| asset.bytes().fold(0usize, |acc, b| (acc * 31 + b as usize) % tranches);
    let original_net: Decimal = construction.weights.values().copied().sum();
    let names: BTreeSet<String> = construction
        .weights
        .keys()
        .chain(current.keys())
        .cloned()
        .collect();
    let mut held_over = 0usize;
    for asset in names {
        if sleeve(&asset) == today {
            continue;
        }
        match current.get(&asset).copied().filter(|w| !w.is_zero()) {
            Some(weight) => {
                construction.weights.insert(asset, weight);
                held_over += 1;
            }
            None => {
                construction.weights.remove(&asset);
            }
        }
    }
    let mut notes = vec![format!(
        "tranche {} of {} trades today; {} position(s) outside the sleeve keep their weight",
        today + 1,
        tranches,
        held_over
    )];
    // The held-over sleeves and today's fresh sleeve were never sized as one
    // book. Two limits can break: the position count, because fresh entries
    // arrive before stale exits leave, and the net band, because each sleeve
    // is only balanced in aggregate. Trim the smallest fresh entries first,
    // then scale one side of today's sleeve so the whole book nets to what
    // the constructor asked for. Held positions are never touched here.
    let max_count = cfg.limits.max_position_count;
    if max_count > 0 && construction.weights.len() > max_count {
        let mut fresh: Vec<(String, Decimal)> = construction
            .weights
            .iter()
            .filter(|(asset, _)| {
                sleeve(asset) == today && !current.get(*asset).is_some_and(|w| !w.is_zero())
            })
            .map(|(asset, w)| (asset.clone(), w.abs()))
            .collect();
        fresh.sort_by(|a, b| a.1.cmp(&b.1));
        let mut dropped = 0usize;
        for (asset, _) in fresh {
            if construction.weights.len() <= max_count {
                break;
            }
            construction.weights.remove(&asset);
            dropped += 1;
        }
        if dropped > 0 {
            notes.push(format!(
                "tranche: dropped {dropped} smallest fresh entr{} to respect the {max_count}-position limit",
                if dropped == 1 { "y" } else { "ies" }
            ));
        }
    }
    let held_net: Decimal = construction
        .weights
        .iter()
        .filter(|(asset, _)| sleeve(asset) != today)
        .map(|(_, w)| *w)
        .sum();
    let sleeve_long: Decimal = construction
        .weights
        .iter()
        .filter(|(asset, w)| sleeve(asset) == today && **w > Decimal::ZERO)
        .map(|(_, w)| *w)
        .sum();
    let sleeve_short: Decimal = construction
        .weights
        .iter()
        .filter(|(asset, w)| sleeve(asset) == today && **w < Decimal::ZERO)
        .map(|(_, w)| w.abs())
        .sum();
    let desired = original_net - held_net;
    let actual = sleeve_long - sleeve_short;
    if actual != desired {
        let (shrink_longs, factor) = if actual > desired {
            let new_long = (sleeve_short + desired).max(Decimal::ZERO);
            (true, if sleeve_long.is_zero() { Decimal::ONE } else { new_long / sleeve_long })
        } else {
            let new_short = (sleeve_long - desired).max(Decimal::ZERO);
            (false, if sleeve_short.is_zero() { Decimal::ONE } else { new_short / sleeve_short })
        };
        if factor != Decimal::ONE {
            for (asset, w) in construction.weights.iter_mut() {
                if sleeve(asset) == today && ((*w > Decimal::ZERO) == shrink_longs) {
                    *w *= factor;
                }
            }
            construction.weights.retain(|_, w| !w.is_zero());
            notes.push(format!(
                "tranche: scaled today's {} side by {} so the book nets {} with {} held over",
                if shrink_longs { "long" } else { "short" },
                q(factor, 4),
                q(original_net, 4),
                q(held_net, 4)
            ));
        }
    }
    notes
}

/// `tilt` is the breadth trend in [-1, 1] (see `breadth_trend`), consumed
/// only by `risk_adjusted_tilted`; every other constructor ignores it.
fn construct_with_tilt(
    signals: &[Signal],
    cfg: &Config,
    nav: Decimal,
    profile: Option<&liquidity::Profile>,
    tilt: Option<Decimal>,
) -> Result<Construction, String> {
    construct_with_tilt_held(signals, cfg, nav, profile, tilt, &BTreeMap::new())
}

/// `held` is the current book as asset -> signed weight. It only matters when
/// `ranker_hold_rank_multiple` is set: an incumbent whose signal direction
/// still matches its held side stays selected while it ranks inside the top
/// `ranker_selection_count * multiple`, and new names only fill the slots
/// incumbents leave open. Entries need a top-K rank; exits need a fall below
/// K * multiple. Selection hysteresis, sizing unchanged.
fn construct_with_tilt_held(
    signals: &[Signal],
    cfg: &Config,
    nav: Decimal,
    profile: Option<&liquidity::Profile>,
    tilt: Option<Decimal>,
    held: &BTreeMap<String, Decimal>,
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
        "risk_adjusted" | "risk_adjusted_score_selected" => {
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
                // Both constructors select the configured count across both sides, then
                // size each side by conviction/vol. The score-selected variant
                // also uses conviction/vol when choosing those names.
                let mut all: Vec<_> = usable.clone();
                let reachable_score = |signal: &Signal, vol: Decimal| {
                    let score = signal.conviction / vol;
                    if !cfg.ranker_capacity_selection {
                        return score;
                    }
                    let capacity = profile
                        .and_then(|p| p.hourly_volume(&signal.asset))
                        .unwrap_or_default()
                        * cfg.limits.participation_limit;
                    let intended = nav * cfg.target_gross_exposure
                        / Decimal::from(cfg.ranker_selection_count as u64);
                    score * (capacity / intended).clamp(Decimal::ZERO, Decimal::ONE)
                };
                if cfg.ranker_capacity_selection {
                    all.retain(|(signal, vol)| reachable_score(signal, *vol) > Decimal::ZERO);
                }
                all.sort_by(|(a, a_vol), (b, b_vol)| {
                    if cfg.constructor == "risk_adjusted_score_selected" {
                        reachable_score(b, *b_vol).cmp(&reachable_score(a, *a_vol))
                    } else {
                        b.conviction
                            .partial_cmp(&a.conviction)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    }
                });
                if let Some(multiple) = cfg.ranker_hold_rank_multiple {
                    // Selection hysteresis. `all` is sorted best-first; an
                    // incumbent inside the wider rank window keeps its slot
                    // if its held side still agrees with the signal.
                    let count = cfg.ranker_selection_count;
                    let window = (Decimal::from(count as u64) * multiple)
                        .ceil()
                        .to_string()
                        .parse::<usize>()
                        .unwrap_or(count)
                        .max(count);
                    let side_matches = |signal: &Signal| {
                        held.get(&signal.asset).is_some_and(|w| {
                            (*w > Decimal::ZERO && signal.direction == Direction::Long)
                                || (*w < Decimal::ZERO && signal.direction == Direction::Short)
                        })
                    };
                    let incumbents: Vec<_> = all
                        .iter()
                        .take(window)
                        .filter(|(signal, _)| side_matches(signal))
                        .map(|(signal, _)| signal.asset.clone())
                        .collect();
                    let mut selected = Vec::with_capacity(count);
                    for entry in &all {
                        if incumbents.contains(&entry.0.asset) {
                            selected.push(*entry);
                        }
                    }
                    selected.truncate(count);
                    for entry in &all {
                        if selected.len() >= count {
                            break;
                        }
                        if !incumbents.contains(&entry.0.asset) {
                            selected.push(*entry);
                        }
                    }
                    notes.push(format!(
                        "selection hysteresis kept {} incumbent(s) inside rank {} for a top-{} book",
                        incumbents.len().min(count),
                        window,
                        count
                    ));
                    all = selected;
                } else if cfg.ranker_selection == config::RankerSelection::PerSide {
                    let mut long_count = 0;
                    let mut short_count = 0;
                    all.retain(|(signal, _)| {
                        let count = if signal.direction == Direction::Long {
                            &mut long_count
                        } else {
                            &mut short_count
                        };
                        *count += 1;
                        *count <= cfg.ranker_selection_count / 2
                    });
                } else {
                    all.truncate(cfg.ranker_selection_count);
                }
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
                        "top-{} book has {} long and {} short; a two-sided book cannot form",
                        cfg.ranker_selection_count,
                        longs.len(),
                        shorts.len()
                    ));
                } else {
                    let net = match cfg.ranker_allocation {
                        config::RankerAllocation::Balanced => Decimal::ZERO,
                        config::RankerAllocation::Breadth | config::RankerAllocation::Timing => {
                            cfg.ranker_net_band
                                * tilt.ok_or("ranker allocation is missing its market signal")?
                        }
                        config::RankerAllocation::Confidence => {
                            let long: Decimal = longs.iter().map(|(s, v)| s.conviction / *v).sum();
                            let short: Decimal =
                                shorts.iter().map(|(s, v)| s.conviction / *v).sum();
                            cfg.ranker_net_band * (long - short) / (long + short)
                        }
                    };
                    if net.abs() > cfg.ranker_net_band {
                        return Err("ranker allocation exceeds its declared net band".into());
                    }
                    if cfg.ranker_allocation != config::RankerAllocation::Balanced {
                        notes.push(format!(
                            "ranker {:?} allocation targets net {} at gross {}",
                            cfg.ranker_allocation,
                            q(net, 4),
                            cfg.target_gross_exposure
                        ));
                    }
                    for (side, sign) in [(longs, Decimal::ONE), (shorts, -Decimal::ONE)] {
                        let sleeve = (cfg.target_gross_exposure + sign * net) / Decimal::from(2);
                        let total: Decimal = side.iter().map(|(s, vol)| s.conviction / *vol).sum();
                        for (s, vol) in side {
                            weights.insert(
                                s.asset.clone(),
                                sign * sleeve * (s.conviction / vol) / total,
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
        "risk_adjusted_unbalanced" | "risk_adjusted_score_unbalanced" => {
            // Top 24 across both sides above the expected-return cost floor,
            // selected by return or return/volatility, then sized by
            // edge/volatility JOINTLY to the gross target, with no per-side
            // split. `risk_adjusted` hands each side exactly half the gross,
            // which on a 20-long/4-short day concentrates 0.40 in the four
            // names the model liked least; this constructor lets the book be
            // as long or as short as the ranked list says. Net exposure is
            // whatever falls out and is reported, not targeted; the risk
            // report's `max_net_exposure` is the only thing that bounds it.
            let floor = Decimal::from(2) * (cfg.costs.commission_bps + cfg.costs.spread_bps) / BPS;
            let mut usable: Vec<_> = signals
                .iter()
                .filter_map(|s| {
                    s.volatility
                        .filter(|v| *v > Decimal::ZERO && s.conviction >= floor)
                        .map(|v| (s, v))
                })
                .collect();
            usable.sort_by(|(a, a_vol), (b, b_vol)| {
                if cfg.constructor == "risk_adjusted_score_unbalanced" {
                    (b.conviction / *b_vol)
                        .cmp(&(a.conviction / *a_vol))
                        .then_with(|| a.asset.cmp(&b.asset))
                } else {
                    b.conviction
                        .partial_cmp(&a.conviction)
                        .unwrap_or(std::cmp::Ordering::Equal)
                }
            });
            usable.truncate(24);
            if usable.is_empty() {
                notes.push("no signal cleared the cost floor; target is flat".into());
            } else {
                let total: Decimal = usable.iter().map(|(s, vol)| s.conviction / *vol).sum();
                for (s, vol) in &usable {
                    let sign = if s.direction == Direction::Long {
                        Decimal::ONE
                    } else {
                        -Decimal::ONE
                    };
                    weights.insert(
                        s.asset.clone(),
                        sign * cfg.target_gross_exposure * (s.conviction / *vol) / total,
                    );
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
                let long: Decimal = weights.values().filter(|v| **v > Decimal::ZERO).sum();
                let short: Decimal = weights.values().filter(|v| **v < Decimal::ZERO).sum();
                notes.push(format!(
                    "{} long ({}) and {} short ({}), sized by edge/volatility across both sides; net {}",
                    weights.values().filter(|v| **v > Decimal::ZERO).count(),
                    q(long, 3),
                    weights.values().filter(|v| **v < Decimal::ZERO).count(),
                    q(short.abs(), 3),
                    q(long + short, 3)
                ));
            }
        }
        "risk_adjusted_tilted" => {
            // docs/research/trend-overlay.md: the frozen risk_adjusted
            // selection and within-side edge/vol sizing, with the two sleeves
            // set from a net target N = TREND_BAND * tilt instead of half the
            // gross each: long = (G+N)/2, short = (G-N)/2. The ranker still
            // picks the names; the overlay only says how much net.
            let Some(tilt) = tilt else {
                return Err("risk_adjusted_tilted needs the breadth trend (ret_30/ret_90 over the eligible universe) and none could be computed".into());
            };
            let n_target = cfg.overlay.band * tilt;
            let long_sleeve = (cfg.target_gross_exposure + n_target) / Decimal::from(2);
            let short_sleeve = (cfg.target_gross_exposure - n_target) / Decimal::from(2);
            let floor = Decimal::from(2) * (cfg.costs.commission_bps + cfg.costs.spread_bps) / BPS;
            let usable: Vec<_> = signals
                .iter()
                .filter_map(|s| {
                    s.volatility
                        .filter(|v| *v > Decimal::ZERO && s.conviction >= floor)
                        .map(|v| (s, v))
                })
                .collect();
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
                for (side, sign, sleeve) in [
                    (longs, Decimal::ONE, long_sleeve),
                    (shorts, -Decimal::ONE, short_sleeve),
                ] {
                    let total: Decimal = side.iter().map(|(s, vol)| s.conviction / *vol).sum();
                    for (s, vol) in side {
                        weights.insert(
                            s.asset.clone(),
                            sign * sleeve * (s.conviction / vol) / total,
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
                    "trend overlay: breadth trend {} -> net target {} (long sleeve {}, short sleeve {}); {} long and {} short, sized by edge/volatility",
                    q(tilt, 3),
                    q(n_target, 3),
                    q(long_sleeve, 3),
                    q(short_sleeve, 3),
                    weights.values().filter(|v| **v > Decimal::ZERO).count(),
                    weights.values().filter(|v| **v < Decimal::ZERO).count()
                ));
            }
        }
        "tsmom_vol_parity" | "ranker_cost_aware_vol_parity" => {
            // crypto-directional round 1: w_i proportional to s_i / sigma_i,
            // normalised so sum(|w_i| * sigma_i) = vol_budget (an ex-ante
            // book-vol proxy under full correlation), gross capped at the
            // target, per-name capped, whole-book scale when a cap binds.
            // Only the daily ranker variant screens its expected returns
            // against the fixed round-trip commission and spread floor.
            let vol_budget = cfg.vol_parity_budget;
            let cost_aware = cfg.constructor == "ranker_cost_aware_vol_parity";
            let floor = if cost_aware {
                Decimal::from(2) * (cfg.costs.commission_bps + cfg.costs.spread_bps) / BPS
            } else {
                Decimal::ZERO
            };
            let usable: Vec<_> = signals
                .iter()
                .filter(|s| !cost_aware || s.conviction >= floor)
                .filter_map(|s| s.volatility.filter(|v| *v > Decimal::ZERO).map(|v| (s, v)))
                .collect();
            let denom: Decimal = usable.iter().map(|(s, _)| s.conviction).sum();
            if cost_aware {
                notes.push(format!(
                    "daily ranker round-trip cost floor {floor}; {} usable of {} signals",
                    usable.len(),
                    signals.len()
                ));
            }
            if usable.is_empty() || (cost_aware && denom.is_zero()) {
                notes.push("no scored names; target is flat".into());
            } else {
                // w_i = sign * (conviction_i / sigma_i) * k with k chosen so
                // sum |w_i| sigma_i = min(vol_budget budget, gross-implied):
                // sum |w_i| sigma_i = k * sum conviction_i => k = vol_budget / denom.
                let k = vol_budget / denom;
                for (s, vol) in &usable {
                    let sign = if s.direction == Direction::Long {
                        Decimal::ONE
                    } else {
                        -Decimal::ONE
                    };
                    weights.insert(s.asset.clone(), sign * k * s.conviction / *vol);
                }
                let gross: Decimal = weights.values().map(|v| v.abs()).sum();
                if gross > cfg.target_gross_exposure {
                    let scale = cfg.target_gross_exposure / gross;
                    for w in weights.values_mut() {
                        *w *= scale;
                    }
                    notes.push(format!(
                        "gross cap binds: whole book scaled by {}",
                        q(scale, 3)
                    ));
                }
                let largest = weights.values().map(|v| v.abs()).max().unwrap_or_default();
                if largest > cfg.limits.max_position {
                    let scale = cfg.limits.max_position / largest;
                    for w in weights.values_mut() {
                        *w *= scale;
                    }
                    notes.push(format!(
                        "per-position cap binds: whole book scaled by {}",
                        q(scale, 3)
                    ));
                }
                let long: Decimal = weights.values().filter(|v| **v > Decimal::ZERO).sum();
                let short: Decimal = weights.values().filter(|v| **v < Decimal::ZERO).sum();
                notes.push(format!(
                    "tsmom vol parity: {} long ({}) / {} short ({}), net {}, ex-ante vol budget {}",
                    weights.values().filter(|v| **v > Decimal::ZERO).count(),
                    q(long, 3),
                    weights.values().filter(|v| **v < Decimal::ZERO).count(),
                    q(short.abs(), 3),
                    q(long + short, 3),
                    vol_budget
                ));
            }
        }
        "donchian_vol_sized" => {
            // Round-2 sizing (pre-registered): weight_i = sign * conviction_i
            // * min(0.25 / sigma_i, 2) / max_holdings. The divisor is the
            // UNIVERSE size, not the scored count - a book with few active
            // signals is meant to be mostly cash, not levered up.
            let n = Decimal::from(cfg.max_holdings.max(1) as u64);
            let cap = Decimal::from(2);
            let budget = Decimal::new(25, 2);
            for s in signals {
                let Some(vol) = s.volatility.filter(|v| *v > Decimal::ZERO) else {
                    continue;
                };
                let sign = if s.direction == Direction::Long {
                    Decimal::ONE
                } else {
                    -Decimal::ONE
                };
                let lever = (budget / vol).min(cap);
                weights.insert(s.asset.clone(), sign * s.conviction * lever / n);
            }
            let gross: Decimal = weights.values().map(|v| v.abs()).sum();
            if gross > cfg.target_gross_exposure {
                let scale = cfg.target_gross_exposure / gross;
                for w in weights.values_mut() {
                    *w *= scale;
                }
                notes.push(format!(
                    "gross cap binds: whole book scaled by {}",
                    q(scale, 3)
                ));
            }
            let largest = weights.values().map(|v| v.abs()).max().unwrap_or_default();
            if largest > cfg.limits.max_position {
                let scale = cfg.limits.max_position / largest;
                for w in weights.values_mut() {
                    *w *= scale;
                }
                notes.push(format!(
                    "per-position cap binds: whole book scaled by {}",
                    q(scale, 3)
                ));
            }
            let long: Decimal = weights.values().filter(|v| **v > Decimal::ZERO).sum();
            let short: Decimal = weights.values().filter(|v| **v < Decimal::ZERO).sum();
            notes.push(format!(
                "donchian vol-sized: {} long ({}) / {} short ({}), net {}, gross {}",
                weights.values().filter(|v| **v > Decimal::ZERO).count(),
                q(long, 3),
                weights.values().filter(|v| **v < Decimal::ZERO).count(),
                q(short.abs(), 3),
                q(long + short, 3),
                q(long - short, 3)
            ));
        }
        "top30_by_score" => {
            // The user's spec, literally (2026-08-19): take the top 30 by
            // |expected return| regardless of side, divide the gross in
            // proportion to |expected return| (no volatility scaling, no
            // per-side split), cap per name. Net exposure is whatever the
            // list says. Exists to test that spec as written; it is not the
            // frozen constructor.
            let floor = Decimal::from(2) * (cfg.costs.commission_bps + cfg.costs.spread_bps) / BPS;
            let mut usable: Vec<_> = signals.iter().filter(|s| s.conviction >= floor).collect();
            usable.sort_by(|a, b| {
                b.conviction
                    .partial_cmp(&a.conviction)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            usable.truncate(30);
            if usable.is_empty() {
                notes.push("no signal cleared the cost floor; target is flat".into());
            } else {
                let total: Decimal = usable.iter().map(|s| s.conviction).sum();
                for s in &usable {
                    let sign = if s.direction == Direction::Long {
                        Decimal::ONE
                    } else {
                        -Decimal::ONE
                    };
                    weights.insert(
                        s.asset.clone(),
                        sign * cfg.target_gross_exposure * s.conviction / total,
                    );
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
                let long: Decimal = weights.values().filter(|v| **v > Decimal::ZERO).sum();
                let short: Decimal = weights.values().filter(|v| **v < Decimal::ZERO).sum();
                notes.push(format!(
                    "top-30 by score: {} long ({}) and {} short ({}), sized by score; net {}",
                    weights.values().filter(|v| **v > Decimal::ZERO).count(),
                    q(long, 3),
                    weights.values().filter(|v| **v < Decimal::ZERO).count(),
                    q(short.abs(), 3),
                    q(long + short, 3)
                ));
            }
        }
        other => return Err(format!("unknown constructor {other:?}")),
    }
    notes.extend(cap_by_participation(&mut weights, cfg, nav, profile));
    if cfg.limits.capacity_balance {
        if profile.is_none() {
            return Err("capacity_balance requires a dated liquidity profile".into());
        }
        balance_capacity(&mut weights);
        notes.push("capacity balance scales both sleeves to the smaller feasible total".into());
    }
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
fn balance_capacity(weights: &mut BTreeMap<String, Decimal>) {
    let long: Decimal = weights.values().filter(|w| **w > Decimal::ZERO).sum();
    let short: Decimal = weights
        .values()
        .filter(|w| **w < Decimal::ZERO)
        .map(|w| w.abs())
        .sum();
    let sleeve = long.min(short);
    for weight in weights.values_mut() {
        let total = if *weight > Decimal::ZERO { long } else { short };
        if total > Decimal::ZERO {
            *weight *= sleeve / total;
        }
    }
}

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
            Some(v) => (limit * v.max(Decimal::ZERO) / nav).min(cfg.limits.max_position),
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

/// What a rebalance moved, split by whether the cap had a say in it.
///
/// `spent` is the allowance consumed by additions, which is what
/// `turnover_budget` governs. `turned_over` is everything the book actually
/// traded, reductions included. They diverge on the days that matter most, and
/// reporting only the first would understate the churn on exactly those days.
#[derive(Debug, Clone, Copy)]
struct Turnover {
    spent: Decimal,
    turned_over: Decimal,
    /// Weight of additions the net-exposure guard released past the turnover
    /// budget to bring the projected book back inside `max_net_exposure`
    /// (zero when the limit is unset or never bound).
    net_guard_released: Decimal,
    /// The book's projected net exposure after this plan's orders - what the
    /// guard measured, before and after releasing.
    net_before_guard: Decimal,
    net_after_guard: Decimal,
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
) -> (Vec<Order>, Vec<Estimate>, Vec<String>, Turnover, Decimal) {
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
            // Round-2 no-trade band (docs/crypto-directional-design.md):
            // resizes under the band are noise; entries/exits and sign
            // flips always trade. Off (zero) for the frozen bot.
            if cfg.rebalance_drift_band > Decimal::ZERO
                && matches!(why, OrderReason::Increase | OrderReason::Reduce)
                && !now.is_zero()
                && target.is_sign_positive() == now.is_sign_positive()
                && drift.abs() < cfg.rebalance_drift_band * now.abs()
            {
                skipped.push(format!("{asset}: resize under the drift band"));
                continue;
            }
            if drift.abs() * BPS < estimate.total * Decimal::from(2) * cfg.rebalance_cost_multiple {
                skipped.push(format!("{asset}: drift under cost deadband"));
                continue;
            }
        }
        candidates.push((asset, drift, notional, *price, why, estimate));
    }
    // Spending the budget to ADD risk is discretionary; spending it to SHED
    // risk is not. The retired planner exempted exits on exactly that
    // reasoning — "a position we no longer want is risk we are still carrying,
    // and pacing out of it is not a saving" — and then budgeted every Reduce,
    // which is the same sentence with a different quantity.
    //
    // The cost of the inconsistency was not theoretical. The budget came out
    // fully spent every single day, deferring ~0.36 of intended weight, so a
    // book could not follow the model when it reversed. KAITO went from the
    // highest-conviction long in the book to rank 19 of 22 overnight; the
    // planner asked for 4.54% -> 0.22% and the cap refused, leaving $869 of
    // loss in a position nobody wanted to hold.
    //
    // So risk reduction is free and only entries and increases compete. The
    // book converges on the target from BELOW on a reversal day, which is the
    // safe direction to be wrong in: gross drifts down until the budget can
    // rebuild it, rather than staying long what the model has abandoned.
    let free = |why: OrderReason| matches!(why, OrderReason::Exit | OrderReason::Reduce);
    candidates.sort_by(|a, b| match (free(a.4), free(b.4)) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        (true, true) => a.0.cmp(&b.0),
        (false, false) => b.1.abs().cmp(&a.1.abs()).then_with(|| a.0.cmp(&b.0)),
    });
    // What the cap governs, and what the book actually did. They are no longer
    // the same number, and reporting only the first would understate turnover
    // on exactly the days it matters.
    let mut spent = Decimal::ZERO;
    let mut turned_over = Decimal::ZERO;
    let mut dropped = Decimal::ZERO;
    let mut selected = Vec::new();
    let mut deferred_candidates = Vec::new();
    for (asset, drift, notional, price, why, est) in candidates {
        if !free(why) && spent + drift.abs() > cfg.turnover_budget {
            dropped += drift.abs();
            skipped.push(format!("{asset}: trade deferred by turnover budget"));
            deferred_candidates.push((asset, drift, notional, price, why, est));
            continue;
        }
        // Only additions consume the allowance. Letting reductions eat it
        // would trade one hostage for another: a heavy de-risking day would
        // leave nothing to buy with, so the book would shed what the model
        // abandoned and never establish what it now wants.
        if !free(why) {
            spent += drift.abs();
        }
        turned_over += drift.abs();
        selected.push((asset, drift, notional, price, why, est));
    }
    // The net-exposure guard. `risk()` checks the TARGET's net, which the
    // constructor keeps at zero by design; the book that exists after this
    // plan is a different number, because the turnover budget lets every
    // reduction through and defers additions. On a reversal day that shrinks
    // one side while the other side's rebuild waits, and the book goes net
    // without any plan ever asking for it (Phase 2 measured -13.2% against a
    // 0.0% target). Restoring neutrality is risk REDUCTION, so it is treated
    // exactly like a Reduce: it does not need the cap's permission. Deferred
    // additions on the side that shrinks |net| are released, largest drift
    // first, until the projected book is inside the limit or none remain.
    // What it could not fix is disclosed as a warning, not hidden in a
    // rejected plan that would leave the book where it is.
    let projected_net = |sel: &[(String, Decimal, Decimal, Decimal, OrderReason, Estimate)]| {
        let mut book: BTreeMap<&str, Decimal> =
            current.iter().map(|(a, w)| (a.as_str(), *w)).collect();
        for (asset, drift, ..) in sel {
            *book.entry(asset.as_str()).or_default() += *drift;
        }
        book.values().sum::<Decimal>()
    };
    let net_before_guard = projected_net(&selected);
    let mut net_after_guard = net_before_guard;
    let mut net_guard_released = Decimal::ZERO;
    if let Some(limit) = cfg.limits.max_net_exposure {
        if net_before_guard.abs() > limit {
            // Only additions that shrink |net|: longs when net is short of
            // the band, shorts when it is over it.
            let want_long = net_before_guard < Decimal::ZERO;
            deferred_candidates.retain(|(_, drift, ..)| (*drift > Decimal::ZERO) == want_long);
            deferred_candidates
                .sort_by(|a, b| b.1.abs().cmp(&a.1.abs()).then_with(|| a.0.cmp(&b.0)));
            let mut released_names = Vec::new();
            for item in deferred_candidates {
                if net_after_guard.abs() <= limit {
                    break;
                }
                net_after_guard += item.1;
                net_guard_released += item.1.abs();
                turned_over += item.1.abs();
                dropped -= item.1.abs();
                skipped.retain(|m| m != &format!("{}: trade deferred by turnover budget", item.0));
                released_names.push(item.0.clone());
                selected.push(item);
            }
            if !released_names.is_empty() {
                skipped.push(format!(
                    "net exposure guard: released {} deferred addition(s) past the turnover \
                     budget to bring projected net from {} toward |net| <= {}: {}",
                    released_names.len(),
                    q(net_before_guard, 4),
                    limit,
                    released_names.join(", ")
                ));
            }
        }
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
    (
        orders,
        estimates,
        skipped,
        Turnover {
            spent,
            turned_over,
            net_guard_released,
            net_before_guard,
            net_after_guard,
        },
        dropped,
    )
}

pub struct DecisionInput<'a> {
    pub native_funding: Option<&'a funding::NativeForecast>,
    pub sizing_liquidity: Option<&'a liquidity::Profile>,
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
    let scheduled_config = input
        .config
        .model_schedule
        .as_ref()
        .map(|schedule| {
            let entry = schedule.at(input.as_of)?;
            let mut cfg = input.config.clone();
            cfg.model_path = Some(entry.model_path.clone());
            Ok::<_, String>(cfg)
        })
        .transpose()?;
    let cfg = scheduled_config.as_ref().unwrap_or(input.config);
    let native_funding = if cfg.native_funding_adjustment {
        let forecast = input
            .native_funding
            .ok_or("native funding adjustment requires a dated native forecast")?;
        forecast.validate_date(input.as_of)?;
        Some(forecast)
    } else {
        None
    };
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
    let loaded_profile = match cfg.liquidity_profile.as_deref() {
        Some(path) => Some(liquidity::Profile::load(path)?),
        None => None,
    };
    let profile = loaded_profile.as_ref();
    let regime = cfg
        .ranker_btc_regime
        .then(|| btc_regime_signal(latest.get("BTC").copied(), horizon))
        .transpose()?;
    if regime
        .as_ref()
        .is_some_and(|(fraction, _)| *fraction == Decimal::ONE)
        && !input.eligible_universe.contains("BTC")
    {
        return Err("ranker_btc_regime positive state requires eligible BTC".into());
    }
    let directional_components = directional_component_configs(cfg, input.as_of.date_naive())?;
    let signal_cfg = directional_components
        .as_ref()
        .map_or(cfg, |components| &components.relative);
    let mut generated = generate_signal(
        &cross,
        &hourly,
        newest_feature_date,
        signal_cfg,
        profile,
        latest.get("BTC").copied(),
    )?;
    if let Some(forecast) = native_funding {
        adjust_native_funding(&mut generated, forecast)?;
    }
    let breadth = breadth_trend(&cross, &cfg.overlay);
    let tilt = match cfg.ranker_allocation {
        config::RankerAllocation::Timing => Some(d(selection_timing::predict(
            cfg.timing_model_path
                .as_deref()
                .ok_or("missing timing model")?,
            cfg.market_matrix_path
                .as_deref()
                .ok_or("missing market matrix")?,
            newest_feature_date,
        )?)?),
        _ => breadth,
    };
    let betas: BTreeMap<_, _> = latest
        .iter()
        .map(|(a, r)| Ok((a.clone(), r.beta_bench.map(d).transpose()?.map(|v| q(v, 6)))))
        .collect::<Result<_, String>>()?;
    let mut component_checks = Vec::new();
    let mut component_disclosures = Vec::new();
    let components = regime
        .as_ref()
        .map(|(fraction, _)| blend_component_configs(cfg, *fraction))
        .or_else(|| blend_configs(cfg));
    let mut construction = if let Some(overlay) = directional_components {
        let secondary = generate_signal(
            &cross,
            &hourly,
            newest_feature_date,
            &overlay.directional,
            profile,
            latest.get("BTC").copied(),
        )?;
        let relative = construct_with_tilt(
            &generated.values,
            &overlay.relative,
            nav,
            input.sizing_liquidity.or(profile),
            tilt,
        )?;
        let directional = construct_with_tilt(
            &secondary.values,
            &overlay.directional,
            nav,
            input.sizing_liquidity.or(profile),
            None,
        )?;
        for (name, component, component_cfg) in [
            ("relative", &relative, &overlay.relative),
            ("directional", &directional, &overlay.directional),
        ] {
            let (report, disclosures) = risk(
                &component.weights,
                &component_cfg.limits,
                &cfg.clusters,
                &betas,
            );
            component_checks.extend(report.checks.into_iter().map(|mut check| {
                check.name = format!("{name}.{}", check.name);
                check
            }));
            component_disclosures.extend(
                disclosures
                    .into_iter()
                    .map(|text| format!("{name}: {text}")),
            );
        }
        generated.notes.extend(
            secondary
                .notes
                .into_iter()
                .map(|note| format!("directional: {note}")),
        );
        generated.warnings.extend(secondary.warnings);
        generated.scoring_version = format!("{}+directional-additive-1", generated.scoring_version);
        let ids = format!(
            "directional overlay models: relative {}; directional {}",
            overlay.relative_model_id, overlay.directional_model_id
        );
        generated.notes.push(ids.clone());
        generated.warnings.push(warning(WarningKind::Other, ids));
        let combined = additive_directional_targets(relative, directional);
        generated.warnings.extend(
            combined
                .notes
                .iter()
                .filter(|note| note.starts_with("directional overlay "))
                .map(|note| warning(WarningKind::Other, note)),
        );
        combined
    } else if let Some((fraction, ranker_cfg, momentum_cfg)) = components {
        let ranker_share = Decimal::ONE - fraction;
        let ranker = if ranker_share.is_zero() {
            Construction {
                weights: BTreeMap::new(),
                constructor: ranker_cfg.constructor.clone(),
                notes: vec!["zero capital share; no ranker targets".into()],
            }
        } else {
            construct_with_tilt(
                &generated.values,
                &ranker_cfg,
                nav * ranker_share,
                input.sizing_liquidity.or(profile),
                tilt,
            )?
        };
        let momentum_signals = if let Some((_, signal)) = &regime {
            Signals {
                values: if fraction.is_zero() {
                    Vec::new()
                } else {
                    vec![signal.clone()]
                },
                notes: vec![format!(
                    "BTC prior completed ret_30 regime; BTC capital fraction {fraction}"
                )],
                warnings: Vec::new(),
                scoring_version: "btc-ret30-regime-1".into(),
                model_id: None,
            }
        } else {
            generate_signal(
                &cross,
                &hourly,
                newest_feature_date,
                &momentum_cfg,
                profile,
                latest.get("BTC").copied(),
            )?
        };
        let momentum = if fraction.is_zero() {
            Construction {
                weights: BTreeMap::new(),
                constructor: momentum_cfg.constructor.clone(),
                notes: vec!["zero capital share; no BTC targets".into()],
            }
        } else if regime.is_some() {
            Construction {
                weights: BTreeMap::from([("BTC".into(), momentum_cfg.target_gross_exposure)]),
                constructor: "btc_regime_long".into(),
                notes: vec!["BTC long conviction 1; fixed gross target 0.80".into()],
            }
        } else {
            construct_with_tilt(
                &momentum_signals.values,
                &momentum_cfg,
                nav * fraction,
                input.sizing_liquidity.or(profile),
                None,
            )?
        };
        for (name, component, component_cfg, share) in [
            ("ranker", &ranker, &ranker_cfg, ranker_share),
            ("btc_momentum", &momentum, &momentum_cfg, fraction),
        ] {
            if share.is_zero() {
                continue;
            }
            let (report, disclosures) = risk(
                &component.weights,
                &component_cfg.limits,
                &cfg.clusters,
                &betas,
            );
            component_checks.extend(report.checks.into_iter().map(|mut check| {
                check.name = format!("{name}.{}", check.name);
                check
            }));
            component_disclosures.extend(
                disclosures
                    .into_iter()
                    .map(|text| format!("{name}: {text}")),
            );
        }
        generated.notes.extend(momentum_signals.notes);
        generated.warnings.extend(momentum_signals.warnings);
        generated.scoring_version = if regime.is_some() {
            format!("{}+btc-ret30-regime-1", generated.scoring_version)
        } else {
            format!("{}+btc-tsmom-target-blend-1", generated.scoring_version)
        };
        let combined = blend_targets(ranker, momentum, fraction);
        for note in &combined.notes {
            if note.starts_with("blend ") {
                generated.warnings.push(warning(WarningKind::Other, note));
            }
        }
        combined
    } else {
        construct_with_tilt_held(
            &generated.values,
            cfg,
            nav,
            input.sizing_liquidity.or(profile),
            tilt,
            &current,
        )?
    };
    if cfg.ranker_short_trend_veto {
        let notes = veto_positive_trend_shorts(&mut construction, &latest, horizon)?;
        generated
            .warnings
            .extend(notes.iter().map(|note| warning(WarningKind::Other, note)));
        construction.notes.extend(notes);
        generated.scoring_version =
            format!("{}+short-trend-veto-ret30-1", generated.scoring_version);
    }
    if cfg.ranker_tranches > 1 {
        let notes = apply_tranches(&mut construction, &current, cfg, input.as_of);
        construction.notes.extend(notes);
        generated.scoring_version = format!(
            "{}+tranches-{}-1",
            generated.scoring_version, cfg.ranker_tranches
        );
    }
    // Shadow overlay (docs/research/trend-overlay.md, "reopened on live
    // evidence"): whatever constructor runs, every plan records what the
    // breadth-trend overlay WOULD have requested, so the paper run accrues a
    // live answer to "what would the lean have earned" without the book
    // taking any of it. A measurement line, not a decision.
    let shadow_overlay = if cfg.constructor != "risk_adjusted_tilted"
        && cfg.ranker_allocation == config::RankerAllocation::Balanced
    {
        breadth.map(|t| {
            format!(
                    "shadow overlay: breadth trend {} (h{}/h{}) would target net {} at band {}; current target net {}",
                q(t, 3),
                cfg.overlay.horizon_a,
                cfg.overlay.horizon_b,
                    q(cfg.overlay.band * t, 3),
                    cfg.overlay.band,
                    q(construction.weights.values().copied().sum(), 3)
            )
        })
    } else {
        None
    };
    let (mut report, mut disclosures) =
        risk(&construction.weights, &cfg.limits, &cfg.clusters, &betas);
    if !component_checks.is_empty() {
        report.checks.extend(component_checks);
        let failed: Vec<_> = report
            .checks
            .iter()
            .filter(|check| !check.passed)
            .map(|check| format!("{} {} exceeds {}", check.name, check.value, check.limit))
            .collect();
        report.passed = failed.is_empty();
        report.rejected_reason = (!failed.is_empty()).then(|| failed.join("; "));
    }
    disclosures.append(&mut component_disclosures);
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
                profile,
            },
            nav,
            cfg,
        )
    } else {
        (
            Vec::new(),
            Vec::new(),
            vec!["plan rejected: no orders computed".into()],
            Turnover {
                spent: Decimal::ZERO,
                turned_over: Decimal::ZERO,
                net_guard_released: Decimal::ZERO,
                net_before_guard: Decimal::ZERO,
                net_after_guard: Decimal::ZERO,
            },
            Decimal::ZERO,
        )
    };

    let mut warnings = generated.warnings;
    if let Some(line) = shadow_overlay {
        warnings.push(warning(WarningKind::Other, line));
    }
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
    if let Some(limit) = cfg.limits.max_net_exposure {
        if !used.net_guard_released.is_zero() {
            warnings.push(warning(
                WarningKind::TurnoverCapped,
                format!(
                    "net exposure guard released {} weight of additions past the turnover \
                     budget: projected net {} -> {} against |net| <= {}",
                    q(used.net_guard_released, 4),
                    q(used.net_before_guard, 4),
                    q(used.net_after_guard, 4),
                    limit
                ),
            ));
        }
        if report.passed && used.net_after_guard.abs() > limit {
            warnings.push(warning(
                WarningKind::Other,
                format!(
                    "projected net exposure {} exceeds max_net_exposure {} after this plan's \
                     orders and nothing deferred could reduce it; the book is outside its \
                     limit until a later plan can rebuild the light side",
                    q(used.net_after_guard, 4),
                    limit
                ),
            ));
        }
    }
    if !dropped.is_zero() {
        warnings.push(warning(
            WarningKind::TurnoverCapped,
            format!(
                "turnover budget {}: {} spent adding, {} more turned over reducing \
                 risk (exempt from the cap); deferred {dropped} weight",
                cfg.turnover_budget,
                used.spent,
                used.turned_over - used.spent
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
            conviction: if regime
                .as_ref()
                .is_some_and(|(fraction, _)| *fraction == Decimal::ONE)
            {
                Some(Decimal::ONE)
            } else if cfg.ranker_directional_model_path.is_some()
                || cfg
                    .ranker_btc_momentum_fraction
                    .is_some_and(|fraction| fraction > Decimal::ZERO)
            {
                None
            } else {
                generated
                    .values
                    .iter()
                    .find(|s| s.asset == *asset)
                    .map(|s| q(s.conviction, 4))
            },
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
            don_long: None,
            don_short: None,
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

    #[test]
    fn trade_assets_preserve_full_cohort_scores_and_causal_plan() {
        let path =
            std::env::temp_dir().join(format!("trade-assets-model-{}.json", std::process::id()));
        let model = serde_json::json!({
            "format_version":model::FORMAT_VERSION,"model_version":model::MODEL_VERSION,
            "feature_set_version":features_crypto::FEATURE_SET_VERSION,
            "trained_through":"2025-01-01","trained_at":"fixture","n_rows":12,"n_dates":1,
            "features":["x_ret_7"],"reward":"per_risk_abs","label_horizon_hours":24,"label_lag_hours":1,
            "target_contract":training::TARGET_CONTRACT,
            "tree_info":[{"tree_index":0,"num_leaves":2,"tree_structure":{
                "split_feature":0,"threshold":0.0,"decision_type":"<=",
                "left_child":{"leaf_value":-0.08},"right_child":{"leaf_value":0.08}
            }}]
        });
        std::fs::write(&path, serde_json::to_vec(&model).unwrap()).unwrap();
        let mut cfg = config();
        cfg.signal = "ml_ranker".into();
        cfg.constructor = "tsmom_vol_parity".into();
        cfg.model_path = Some(path.display().to_string());
        let as_of: DateTime<Utc> = "2026-01-08T00:00:00Z".parse().unwrap();
        let mut rows: Vec<_> = (0..12)
            .map(|i| {
                let asset = match i {
                    4 => "ETH".into(),
                    8 => "BTC".into(),
                    _ => format!("A{i}"),
                };
                let mut row = row(&asset, as_of - TimeDelta::days(1), 100.0);
                row.ret_7 = Some(i as f64);
                row
            })
            .collect();
        let generate = |cfg: &Config| {
            generate_signal(
                &rows.iter().collect::<Vec<_>>(),
                &BTreeMap::new(),
                (as_of - TimeDelta::days(1)).date_naive(),
                cfg,
                None,
                None,
            )
            .unwrap()
        };
        let full = generate(&cfg);
        assert_eq!(full.values.len(), 12);
        cfg.ranker_trade_assets = Some(vec!["BTC".into(), "ETH".into(), "UNLISTED".into()]);
        let filtered = generate(&cfg);
        assert_eq!(filtered.values.len(), 2);
        assert_eq!(filtered.model_id, full.model_id);
        assert_eq!(filtered.scoring_version, full.scoring_version);
        assert_eq!(
            serde_json::to_value(&filtered.warnings).unwrap(),
            serde_json::to_value(&full.warnings).unwrap()
        );
        for signal in &filtered.values {
            let prior = full
                .values
                .iter()
                .find(|v| v.asset == signal.asset)
                .unwrap();
            assert_eq!(
                (signal.direction, signal.conviction, signal.volatility),
                (prior.direction, prior.conviction, prior.volatility)
            );
        }
        assert!(filtered
            .notes
            .iter()
            .any(|n| n.contains("scored 12 eligible assets")));
        assert!(filtered
            .notes
            .iter()
            .any(|n| n.contains("retained 2 of 12 signals; omitted 10")));
        let extra = row("EXTRA", as_of - TimeDelta::days(1), 100.0);
        let eligible_without_btc: Vec<_> = rows
            .iter()
            .filter(|r| r.asset != "BTC")
            .chain(std::iter::once(&extra))
            .collect();
        let ineligible = generate_signal(
            &eligible_without_btc,
            &BTreeMap::new(),
            (as_of - TimeDelta::days(1)).date_naive(),
            &cfg,
            None,
            None,
        )
        .unwrap();
        assert_eq!(ineligible.values.len(), 1);
        assert_eq!(ineligible.values[0].asset, "ETH");
        let universe: BTreeSet<_> = rows.iter().map(|r| r.asset.clone()).collect();
        let book = Portfolio {
            cash: Decimal::from(100000),
            positions: Vec::new(),
        };
        let run = |cfg: &Config, rows: &[DailyRow]| {
            decide(DecisionInput {
                native_funding: None,
                sizing_liquidity: None,
                as_of,
                created_at: as_of,
                mode: Mode::Dry,
                config: cfg,
                daily_features: rows,
                hourly_features: None,
                eligible_universe: &universe,
                portfolio: &book,
                inputs_hash: "trade-assets-fixture",
            })
            .unwrap()
        };
        let before = run(&cfg, &rows);
        let mut future = rows[0].clone();
        future.asset = "BTC".into();
        future.ts_utc = as_of;
        future.ret_7 = Some(-1000.0);
        rows.push(future);
        assert_eq!(
            canonical_json(&before.plan).unwrap(),
            canonical_json(&run(&cfg, &rows).plan).unwrap()
        );
        cfg.ranker_trade_assets = Some(vec!["UNLISTED".into()]);
        let refs: Vec<_> = rows[..12].iter().collect();
        let empty = generate_signal(
            &refs,
            &BTreeMap::new(),
            (as_of - TimeDelta::days(1)).date_naive(),
            &cfg,
            None,
            None,
        )
        .unwrap();
        assert!(empty.values.is_empty());
        assert!(construct(&empty.values, &cfg, Decimal::from(100000), None)
            .unwrap()
            .weights
            .is_empty());
        assert!(empty
            .notes
            .iter()
            .any(|n| n.contains("retained 0 of 12 signals; omitted 12")));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn trade_assets_require_absolute_daily_model_even_with_empty_cohort() {
        let path =
            std::env::temp_dir().join(format!("trade-assets-contract-{}.json", std::process::id()));
        let mut doc = serde_json::json!({
            "format_version":model::FORMAT_VERSION,"model_version":model::MODEL_VERSION,
            "feature_set_version":features_crypto::FEATURE_SET_VERSION,
            "trained_through":"2025-01-01","trained_at":"fixture","n_rows":12,"n_dates":1,
            "features":["x_ret_7"],"reward":"per_risk_abs","label_horizon_hours":24,"label_lag_hours":1,
            "target_contract":training::TARGET_CONTRACT,
            "tree_info":[{"tree_index":0,"num_leaves":1,"tree_structure":{"leaf_value":0.01}}]
        });
        let mut cfg = config();
        cfg.signal = "ml_ranker".into();
        cfg.constructor = "tsmom_vol_parity".into();
        cfg.model_path = Some(path.display().to_string());
        cfg.ranker_trade_assets = Some(vec!["BTC".into()]);
        let at = "2026-01-07".parse().unwrap();
        for (reward, horizon) in [
            ("return", Some(24)),
            ("per_risk", Some(24)),
            ("per_risk_abs", Some(120)),
            ("per_risk_abs", None),
            ("per_risk_abs", Some(24)),
        ] {
            doc["reward"] = reward.into();
            if let Some(h) = horizon {
                doc["label_horizon_hours"] = h.into();
            } else {
                doc.as_object_mut().unwrap().remove("label_horizon_hours");
            }
            std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
            let result = generate_signal(&[], &BTreeMap::new(), at, &cfg, None, None);
            if reward == "per_risk_abs" && horizon == Some(24) {
                assert!(result.unwrap().values.is_empty());
            } else {
                assert!(result.is_err());
            }
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn vol_parity_budget_preserves_default_and_doubles_only_when_caps_allow() {
        let mut cfg = config();
        let signals: Vec<_> = [
            ("LONG", Direction::Long, Decimal::new(4, 2)),
            ("SHORT", Direction::Short, Decimal::new(2, 2)),
            ("OTHER", Direction::Long, Decimal::new(2, 2)),
        ]
        .into_iter()
        .map(|(asset, direction, conviction)| Signal {
            asset: asset.into(),
            direction,
            conviction,
            volatility: Some(Decimal::new(5, 1)),
        })
        .collect();
        for constructor in ["tsmom_vol_parity", "ranker_cost_aware_vol_parity"] {
            cfg.constructor = constructor.into();
            cfg.vol_parity_budget = Decimal::new(25, 2);
            cfg.target_gross_exposure = Decimal::new(8, 1);
            cfg.limits.max_position = Decimal::new(5, 1);
            let default = construct(&signals, &cfg, Decimal::from(100000), None).unwrap();
            let mut explicit = cfg.clone();
            explicit.vol_parity_budget = "0.25".parse().unwrap();
            let explicit_default =
                construct(&signals, &explicit, Decimal::from(100000), None).unwrap();
            assert_eq!(default.weights, explicit_default.weights);
            assert_eq!(default.notes, explicit_default.notes);
            assert_eq!(default.weights["LONG"], Decimal::new(25, 2));
            assert_eq!(
                default.weights["SHORT"],
                -default.weights["LONG"] / Decimal::from(2)
            );
            let mut doubled = cfg.clone();
            doubled.vol_parity_budget = Decimal::new(5, 1);
            doubled.target_gross_exposure *= Decimal::from(2);
            doubled.limits.max_position *= Decimal::from(2);
            let out = construct(&signals, &doubled, Decimal::from(100000), None).unwrap();
            for (asset, weight) in &default.weights {
                assert_eq!(out.weights[asset], *weight * Decimal::from(2));
            }
            doubled.target_gross_exposure = Decimal::new(4, 1);
            doubled.limits.max_position = Decimal::new(1, 1);
            let capped = construct(&signals, &doubled, Decimal::from(100000), None).unwrap();
            assert!(capped
                .weights
                .values()
                .all(|w| w.abs() <= doubled.limits.max_position));
            assert!(
                capped.weights.values().map(|w| w.abs()).sum::<Decimal>()
                    <= doubled.target_gross_exposure
            );
            assert!(capped
                .notes
                .iter()
                .any(|note| note.contains("gross cap binds")));
            assert!(capped
                .notes
                .iter()
                .any(|note| note.contains("per-position cap binds")));
            assert!(
                (capped.weights["LONG"] + capped.weights["SHORT"] * Decimal::from(2)).abs()
                    < Decimal::new(1, 26)
            );
        }
        let base = config();
        let (_, ranker, momentum) = blend_component_configs(&base, Decimal::new(5, 1));
        assert_eq!(ranker.vol_parity_budget, Decimal::new(25, 2));
        assert_eq!(momentum.vol_parity_budget, Decimal::new(25, 2));
    }

    #[test]
    fn cost_aware_vol_parity_filters_before_sizing_and_preserves_caps_and_proportions() {
        let mut cfg = config();
        cfg.constructor = "ranker_cost_aware_vol_parity".into();
        cfg.costs.commission_bps = Decimal::ONE;
        cfg.costs.spread_bps = Decimal::from(2);
        let make = |asset: &str, conviction: &str, vol: &str| Signal {
            asset: asset.into(),
            direction: Direction::Long,
            conviction: conviction.parse().unwrap(),
            volatility: Some(vol.parse().unwrap()),
        };
        let boundary = make("BOUNDARY", "0.0006", "0.5");
        let above = make("ABOVE", "0.0012", "0.5");
        let below = make("BELOW", "0.000599999", "0.00001");
        let signals = vec![
            boundary.clone(),
            above.clone(),
            below.clone(),
            make("BAD_VOL", "0.002", "0"),
        ];
        let filtered = construct(&signals, &cfg, Decimal::from(100000), None).unwrap();
        let mut legacy = cfg.clone();
        legacy.constructor = "tsmom_vol_parity".into();
        let expected = construct(
            &[boundary.clone(), above.clone()],
            &legacy,
            Decimal::from(100000),
            None,
        )
        .unwrap();
        assert_eq!(filtered.weights, expected.weights);
        assert_eq!(filtered.weights.len(), 2);
        assert!(filtered.weights.contains_key("BOUNDARY"));
        assert!(
            (filtered.weights["ABOVE"] - filtered.weights["BOUNDARY"] * Decimal::from(2)).abs()
                < Decimal::new(1, 12)
        );
        assert_eq!(filtered.weights["ABOVE"], cfg.limits.max_position);
        let original = construct(&signals, &legacy, Decimal::from(100000), None).unwrap();
        assert!(original.weights.contains_key("BELOW"));
        assert_ne!(original.weights, filtered.weights);
        assert!(construct(&[below], &cfg, Decimal::from(100000), None)
            .unwrap()
            .weights
            .is_empty());
        let caps = [make("A", "0.006", "0.1"), make("B", "0.006", "0.2")];
        let capped = construct(&caps, &cfg, Decimal::from(100000), None).unwrap();
        assert_eq!(
            capped.weights,
            construct(&caps, &legacy, Decimal::from(100000), None)
                .unwrap()
                .weights
        );
        assert!(capped.notes.iter().any(|n| n.contains("gross cap binds")));
        assert!(capped
            .notes
            .iter()
            .any(|n| n.contains("per-position cap binds")));
        assert!(capped
            .weights
            .values()
            .all(|w| *w <= cfg.limits.max_position));
        cfg.costs.commission_bps = Decimal::ZERO;
        cfg.costs.spread_bps = Decimal::ZERO;
        assert!(construct(
            &[make("ZERO", "0", "0.5")],
            &cfg,
            Decimal::from(100000),
            None
        )
        .unwrap()
        .weights
        .is_empty());
    }

    #[test]
    fn cost_aware_vol_parity_requires_explicit_daily_model_horizon() {
        let path =
            std::env::temp_dir().join(format!("cost-aware-vol-model-{}.json", std::process::id()));
        let mut doc = serde_json::json!({
            "format_version":model::FORMAT_VERSION,"model_version":model::MODEL_VERSION,
            "feature_set_version":features_crypto::FEATURE_SET_VERSION,
            "trained_through":"2025-01-01","trained_at":"fixture","n_rows":12,"n_dates":1,
            "features":["x_ret_7"],"reward":"return",
            "tree_info":[{"tree_index":0,"num_leaves":1,"tree_structure":{"leaf_value":0.01}}]
        });
        let mut cfg = config();
        cfg.signal = "ml_ranker".into();
        cfg.constructor = "ranker_cost_aware_vol_parity".into();
        cfg.model_path = Some(path.display().to_string());
        let at: DateTime<Utc> = "2026-01-07T00:00:00Z".parse().unwrap();
        let rows: Vec<_> = (0..12).map(|i| row(&format!("A{i}"), at, 100.0)).collect();
        let refs: Vec<_> = rows.iter().collect();
        for horizon in [None, Some(120), Some(24)] {
            if let Some(hours) = horizon {
                doc["label_horizon_hours"] = hours.into();
            }
            std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
            let generated =
                generate_signal(&refs, &BTreeMap::new(), at.date_naive(), &cfg, None, None);
            if horizon == Some(24) {
                assert!(generated.is_ok());
            } else {
                assert!(generated.unwrap_err().contains("explicit 24h"));
            }
            let mut legacy = cfg.clone();
            legacy.constructor = "tsmom_vol_parity".into();
            assert!(generate_signal(
                &refs,
                &BTreeMap::new(),
                at.date_naive(),
                &legacy,
                None,
                None
            )
            .is_ok());
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn additive_directional_targets_sum_full_mandates_and_cancel_without_rescaling() {
        let make = |weights| Construction {
            weights,
            constructor: "fixture".into(),
            notes: Vec::new(),
        };
        let out = additive_directional_targets(
            make(BTreeMap::from([
                ("BTC".into(), Decimal::new(-2, 1)),
                ("ETH".into(), Decimal::new(2, 1)),
            ])),
            make(BTreeMap::from([
                ("BTC".into(), Decimal::new(2, 1)),
                ("ETH".into(), Decimal::new(1, 1)),
                ("SOL".into(), Decimal::new(-1, 1)),
            ])),
        );
        assert_eq!(
            out.weights,
            BTreeMap::from([
                ("ETH".into(), Decimal::new(3, 1)),
                ("SOL".into(), Decimal::new(-1, 1))
            ])
        );
        assert!(out
            .notes
            .iter()
            .any(|note| note.contains("relative full shared-NAV targets")));
        assert!(out
            .notes
            .iter()
            .any(|note| note.contains("directional full shared-NAV targets")));
    }

    #[test]
    fn directional_overlay_nets_one_plan_preserves_component_limits_and_model_embargoes() {
        let directory =
            std::env::temp_dir().join(format!("directional-overlay-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let primary_path = directory.join("relative.json");
        let secondary_path = directory.join("absolute.json");
        let primary = serde_json::json!({
            "format_version":model::FORMAT_VERSION,"model_version":model::MODEL_VERSION,
            "feature_set_version":features_crypto::FEATURE_SET_VERSION,
            "trained_through":"2025-01-01","trained_at":"fixture","n_rows":16,"n_dates":1,
            "features":["x_ret_7"],"reward":"per_risk","label_horizon_hours":24,"label_lag_hours":0,
            "target_contract":training::TARGET_CONTRACT,
            "tree_info":[{"tree_index":0,"num_leaves":2,"tree_structure":{
                "split_feature":0,"threshold":0.0,"decision_type":"<=",
                "left_child":{"leaf_value":-0.08},"right_child":{"leaf_value":0.08}
            }}]
        });
        let mut secondary = primary.clone();
        secondary["reward"] = "per_risk_abs".into();
        secondary["tree_info"][0]["tree_structure"]["left_child"]["leaf_value"] = 0.08.into();
        secondary["tree_info"][0]["tree_structure"]["right_child"]["leaf_value"] = (-0.08).into();
        std::fs::write(&primary_path, serde_json::to_vec(&primary).unwrap()).unwrap();
        std::fs::write(&secondary_path, serde_json::to_vec(&secondary).unwrap()).unwrap();
        let mut cfg = config();
        cfg.signal = "ml_ranker".into();
        cfg.constructor = "risk_adjusted_score_selected".into();
        cfg.model_path = Some(primary_path.display().to_string());
        cfg.ranker_directional_model_path = Some(secondary_path.display().to_string());
        cfg.target_gross_exposure = Decimal::new(16, 1);
        cfg.turnover_budget = Decimal::from(2);
        cfg.rebalance_drift_band = Decimal::ZERO;
        cfg.clusters.clear();
        cfg.limits.max_gross_exposure = Decimal::from(2);
        cfg.limits.max_position = Decimal::new(5, 1);
        cfg.limits.max_net_exposure = Some(Decimal::new(11, 1));
        cfg.limits.max_cluster_exposure = Some(Decimal::new(14, 1));
        cfg.limits.max_benchmark_beta = Some(Decimal::new(25, 1));
        let as_of: DateTime<Utc> = "2026-01-08T00:00:00Z".parse().unwrap();
        let mut rows: Vec<_> = (0..16)
            .map(|i| {
                let asset = if i == 0 {
                    "BTC".into()
                } else {
                    format!("A{i:02}")
                };
                let mut r = row(&asset, as_of - TimeDelta::days(1), 100.0);
                r.ret_7 = Some(i as f64 / 100.0);
                r
            })
            .collect();
        let universe: BTreeSet<_> = rows.iter().map(|r| r.asset.clone()).collect();
        let book = Portfolio {
            cash: Decimal::from(100000),
            positions: Vec::new(),
        };
        let run = |cfg: &Config, rows: &[DailyRow]| {
            decide(DecisionInput {
                native_funding: None,
                sizing_liquidity: None,
                as_of,
                created_at: as_of,
                mode: Mode::Dry,
                config: cfg,
                daily_features: rows,
                hourly_features: None,
                eligible_universe: &universe,
                portfolio: &book,
                inputs_hash: "directional-overlay-fixture",
            })
        };
        let components = directional_component_configs(&cfg, as_of.date_naive())
            .unwrap()
            .unwrap();
        assert!(components.relative.ranker_directional_model_path.is_none());
        for component in [&components.relative, &components.directional] {
            assert_eq!(component.target_gross_exposure, Decimal::new(8, 1));
            assert_eq!(component.limits.max_gross_exposure, Decimal::ONE);
            assert_eq!(component.limits.max_position, Decimal::new(25, 2));
            assert_eq!(
                component.limits.max_benchmark_beta,
                Some(Decimal::new(125, 2))
            );
        }
        assert_eq!(
            components.relative.limits.max_net_exposure,
            Some(Decimal::new(1, 1))
        );
        assert_eq!(
            components.relative.limits.max_cluster_exposure,
            Some(Decimal::new(4, 1))
        );
        assert_eq!(
            components.directional.limits.max_net_exposure,
            Some(Decimal::ONE)
        );
        assert_eq!(
            components.directional.limits.max_cluster_exposure,
            Some(Decimal::ONE)
        );
        let relative = run(&components.relative, &rows).unwrap();
        let directional = run(&components.directional, &rows).unwrap();
        let combined = run(&cfg, &rows).unwrap();
        assert_eq!(relative.plan.status, Status::Accepted);
        assert_eq!(directional.plan.status, Status::Accepted);
        assert_eq!(
            combined.plan.status,
            Status::Accepted,
            "{:?}",
            combined.plan.risk_report
        );
        let mut expected = BTreeMap::new();
        for target in relative
            .plan
            .targets
            .iter()
            .chain(&directional.plan.targets)
        {
            *expected
                .entry(target.asset.clone())
                .or_insert(Decimal::ZERO) += target.weight;
        }
        expected.retain(|_, weight| !weight.is_zero());
        assert_eq!(
            combined
                .plan
                .targets
                .iter()
                .map(|t| (t.asset.clone(), t.weight))
                .collect::<BTreeMap<_, _>>(),
            expected
        );
        assert!(combined.plan.targets.iter().all(|t| t.conviction.is_none()));
        let assets: BTreeSet<_> = combined
            .plan
            .orders
            .iter()
            .map(|o| o.asset.clone())
            .collect();
        assert_eq!(assets.len(), combined.plan.orders.len());
        assert!(
            combined.plan.cost_estimate.total_quote
                < relative.plan.cost_estimate.total_quote
                    + directional.plan.cost_estimate.total_quote
        );
        for id in [
            &components.relative_model_id,
            &components.directional_model_id,
        ] {
            assert!(combined
                .plan
                .warnings
                .iter()
                .any(|warning| warning.message.contains(id)));
        }
        let mut account_tight = cfg.clone();
        account_tight.limits.max_position = Decimal::new(1, 2);
        let rejected = run(&account_tight, &rows).unwrap();
        assert_eq!(rejected.plan.status, Status::Rejected);
        assert!(rejected.plan.orders.is_empty());
        assert!(rejected
            .plan
            .risk_report
            .checks
            .iter()
            .any(|c| c.name == "max_position" && !c.passed));
        assert!(rejected
            .plan
            .risk_report
            .checks
            .iter()
            .filter(|c| c.name.starts_with("relative.") || c.name.starts_with("directional."))
            .all(|c| c.passed));
        let mut component_tight = cfg.clone();
        component_tight.clusters = rows
            .iter()
            .take(9)
            .map(|r| (r.asset.clone(), "same".into()))
            .collect();
        let rejected = run(&component_tight, &rows).unwrap();
        assert_eq!(rejected.plan.status, Status::Rejected);
        assert!(rejected.plan.orders.is_empty());
        assert!(rejected
            .plan
            .risk_report
            .checks
            .iter()
            .any(|c| c.name == "relative.max_cluster_exposure" && !c.passed));
        assert!(rejected
            .plan
            .risk_report
            .checks
            .iter()
            .any(|c| c.name == "max_cluster_exposure" && c.passed));
        for mut future in rows.clone() {
            future.ts_utc = as_of;
            future.ret_7 = Some(-100.0);
            rows.push(future);
        }
        assert_eq!(
            canonical_json(&combined.plan).unwrap(),
            canonical_json(&run(&cfg, &rows).unwrap().plan).unwrap()
        );
        let original_secondary = secondary.clone();
        for (key, bad) in [
            ("reward", serde_json::json!("per_risk")),
            ("label_horizon_hours", serde_json::json!(120)),
            ("trained_through", serde_json::json!("2026-01-08")),
        ] {
            secondary = original_secondary.clone();
            secondary[key] = bad;
            std::fs::write(&secondary_path, serde_json::to_vec(&secondary).unwrap()).unwrap();
            assert!(run(&cfg, &rows).is_err());
            assert_eq!(
                canonical_json(&relative.plan).unwrap(),
                canonical_json(&run(&components.relative, &rows).unwrap().plan).unwrap(),
                "disabled overlay must ignore secondary model changes"
            );
        }
        std::fs::write(
            &secondary_path,
            serde_json::to_vec(&original_secondary).unwrap(),
        )
        .unwrap();
        let mut wrong_primary = primary.clone();
        wrong_primary["reward"] = "per_risk_abs".into();
        std::fs::write(&primary_path, serde_json::to_vec(&wrong_primary).unwrap()).unwrap();
        assert!(run(&cfg, &rows).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn short_trend_veto_filters_only_positive_shorts_and_fails_closed() {
        let at: DateTime<Utc> = "2026-01-07T00:00:00Z".parse().unwrap();
        let assets = ["POS", "ZERO", "NEG", "LONG"];
        let mut rows: Vec<_> = assets
            .iter()
            .zip([0.2, 0.0, -0.2, 0.8])
            .map(|(asset, ret)| {
                let mut r = row(asset, at, 100.0);
                r.ret_30 = Some(ret);
                r
            })
            .collect();
        let weights = BTreeMap::from([
            ("POS".into(), Decimal::new(-1, 1)),
            ("ZERO".into(), Decimal::new(-2, 1)),
            ("NEG".into(), Decimal::new(-3, 1)),
            ("LONG".into(), Decimal::new(4, 1)),
        ]);
        let make = || Construction {
            weights: weights.clone(),
            constructor: "fixture".into(),
            notes: Vec::new(),
        };
        let mut construction = make();
        let latest = rows.iter().map(|r| (r.asset.clone(), r)).collect();
        let notes = veto_positive_trend_shorts(&mut construction, &latest, at).unwrap();
        let mut expected = weights.clone();
        expected.remove("POS");
        assert_eq!(construction.weights, expected);
        assert_eq!(notes.len(), 1);
        assert!(
            notes[0].contains("POS") && notes[0].contains("0.2") && notes[0].contains("2026-01-07")
        );
        for bad in [None, Some(f64::NAN), Some(f64::INFINITY)] {
            rows[1].ret_30 = bad;
            let latest = rows.iter().map(|r| (r.asset.clone(), r)).collect();
            let mut construction = make();
            assert!(veto_positive_trend_shorts(&mut construction, &latest, at).is_err());
            assert_eq!(
                construction.weights, weights,
                "failure must not partially mutate targets"
            );
        }
        rows[1].ret_30 = Some(0.0);
        for delta in [-1, 1] {
            rows[1].ts_utc = at + TimeDelta::days(delta);
            let latest = rows.iter().map(|r| (r.asset.clone(), r)).collect();
            assert!(veto_positive_trend_shorts(&mut make(), &latest, at).is_err());
        }
        rows[1].ts_utc = at;
        let latest = rows
            .iter()
            .filter(|r| r.asset != "POS")
            .map(|r| (r.asset.clone(), r))
            .collect();
        assert!(veto_positive_trend_shorts(&mut make(), &latest, at).is_err());
    }

    #[test]
    fn short_trend_veto_plan_preserves_weights_closes_held_short_and_checks_net_risk() {
        let path =
            std::env::temp_dir().join(format!("short-veto-model-{}.json", std::process::id()));
        let model = serde_json::json!({
            "format_version":model::FORMAT_VERSION,"model_version":model::MODEL_VERSION,
            "feature_set_version":features_crypto::FEATURE_SET_VERSION,
            "trained_through":"2025-01-01","trained_at":"fixture","n_rows":12,"n_dates":1,
            "features":["x_ret_7"],"reward":"return",
            "tree_info":[{"tree_index":0,"num_leaves":2,"tree_structure":{
                "split_feature":0,"threshold":0.0,"decision_type":"<=",
                "left_child":{"leaf_value":-0.01},"right_child":{"leaf_value":0.01}
            }}]
        });
        std::fs::write(&path, serde_json::to_vec(&model).unwrap()).unwrap();
        let mut cfg = config();
        cfg.signal = "ml_ranker".into();
        cfg.constructor = "risk_adjusted_score_selected".into();
        cfg.model_path = Some(path.display().to_string());
        cfg.ranker_selection_count = 8;
        cfg.target_gross_exposure = Decimal::new(8, 1);
        cfg.turnover_budget = Decimal::from(2);
        cfg.rebalance_drift_band = Decimal::ZERO;
        cfg.limits.max_net_exposure = Some(Decimal::new(4, 1));
        cfg.limits.max_benchmark_beta = None;
        cfg.clusters.clear();
        let as_of: DateTime<Utc> = "2026-01-08T00:00:00Z".parse().unwrap();
        let mut rows: Vec<_> = (0..12)
            .map(|i| {
                let mut r = row(&format!("A{i}"), as_of - TimeDelta::days(1), 100.0);
                r.ret_7 = Some(i as f64 / 100.0);
                r
            })
            .collect();
        let universe: BTreeSet<_> = rows.iter().map(|r| r.asset.clone()).collect();
        let empty = Portfolio {
            cash: Decimal::from(100000),
            positions: Vec::new(),
        };
        let run = |cfg: &Config, rows: &[DailyRow], portfolio: &Portfolio| {
            decide(DecisionInput {
                native_funding: None,
                sizing_liquidity: None,
                as_of,
                created_at: as_of,
                mode: Mode::Dry,
                config: cfg,
                daily_features: rows,
                hourly_features: None,
                eligible_universe: &universe,
                portfolio,
                inputs_hash: "short-veto-fixture",
            })
        };
        let baseline = run(&cfg, &rows, &empty).unwrap();
        assert_eq!(baseline.plan.status, Status::Accepted);
        let mut explicit_false = cfg.clone();
        explicit_false.ranker_short_trend_veto = false;
        assert_eq!(
            canonical_json(&baseline.plan).unwrap(),
            canonical_json(&run(&explicit_false, &rows, &empty).unwrap().plan).unwrap()
        );
        let short = baseline
            .plan
            .targets
            .iter()
            .find(|t| t.weight < Decimal::ZERO)
            .unwrap()
            .asset
            .clone();
        cfg.ranker_short_trend_veto = true;
        let veto = run(&cfg, &rows, &empty).unwrap();
        assert_eq!(
            veto.plan.status,
            Status::Accepted,
            "{:?}",
            veto.plan.risk_report
        );
        let expected: Vec<_> = baseline
            .plan
            .targets
            .iter()
            .filter(|t| t.weight > Decimal::ZERO)
            .collect();
        assert_eq!(
            serde_json::to_value(&veto.plan.targets).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        assert!(veto
            .plan
            .provenance
            .scoring_version
            .ends_with("+short-trend-veto-ret30-1"));
        // Six equal long weights retain the constructor's Decimal division residual.
        // The account limit is explicit; the veto must not round or rescale targets.
        let mut six = cfg.clone();
        six.ranker_selection_count = 12;
        let exact_limit = run(&six, &rows, &empty).unwrap();
        assert_eq!(exact_limit.plan.status, Status::Rejected);
        let net = exact_limit
            .plan
            .risk_report
            .checks
            .iter()
            .find(|check| check.name == "max_net_exposure")
            .unwrap();
        assert_eq!(
            net.value,
            "0.4000000000000000000000000002".parse::<Decimal>().unwrap()
        );
        assert_eq!(net.limit, Decimal::new(4, 1));
        assert!(!net.passed);
        assert_eq!(exact_limit.plan.targets.len(), 6);
        six.limits.max_net_exposure = Some(Decimal::new(400001, 6));
        let explicit_quantum = run(&six, &rows, &empty).unwrap();
        assert_eq!(explicit_quantum.plan.status, Status::Accepted);
        assert_eq!(
            serde_json::to_value(&exact_limit.plan.targets).unwrap(),
            serde_json::to_value(&explicit_quantum.plan.targets).unwrap()
        );
        let accepted_net = explicit_quantum
            .plan
            .risk_report
            .checks
            .iter()
            .find(|check| check.name == "max_net_exposure")
            .unwrap();
        assert_eq!(accepted_net.value, net.value);
        assert!(accepted_net.passed);
        six.ranker_short_trend_veto = false;
        assert_eq!(
            run(&six, &rows, &empty).unwrap().plan.status,
            Status::Accepted
        );
        let held = Portfolio {
            cash: Decimal::from(110000),
            positions: vec![Position {
                asset: short.clone(),
                qty: Decimal::from(-100),
            }],
        };
        let exit = run(&cfg, &rows, &held).unwrap();
        assert_eq!(exit.plan.status, Status::Accepted);
        let exits: Vec<_> = exit
            .plan
            .orders
            .iter()
            .filter(|o| o.asset == short)
            .collect();
        assert_eq!(exits.len(), 1);
        assert_eq!(exits[0].side, Side::Buy);
        assert_eq!(exits[0].qty, Decimal::from(100));
        for r in rows.clone() {
            let mut future = r;
            future.ts_utc = as_of;
            future.ret_30 = Some(-1.0);
            rows.push(future);
        }
        assert_eq!(
            canonical_json(&veto.plan).unwrap(),
            canonical_json(&run(&cfg, &rows, &empty).unwrap().plan).unwrap()
        );
        cfg.limits.max_net_exposure = Some(Decimal::new(1, 1));
        let rejected = run(&cfg, &rows, &empty).unwrap();
        assert_eq!(rejected.plan.status, Status::Rejected);
        assert!(rejected
            .plan
            .risk_report
            .checks
            .iter()
            .any(|c| c.name == "max_net_exposure" && !c.passed));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn market_context_uses_exact_completed_btc_and_rejects_missing_or_nonfinite() {
        let stamp: DateTime<Utc> = "2026-01-07T00:00:00Z".parse().unwrap();
        let mut btc = row("BTC", stamp, 100.0);
        assert_eq!(
            market_context::values(Some(&btc), stamp.date_naive()).unwrap(),
            [0.1, 0.2, 0.3, 0.5]
        );
        assert!(market_context::values(None, stamp.date_naive()).is_err());
        for offset in [-1, 1] {
            btc.ts_utc = stamp + TimeDelta::days(offset);
            assert!(market_context::values(Some(&btc), stamp.date_naive()).is_err());
        }
        btc.ts_utc = stamp;
        for missing in [None, Some(f64::NAN), Some(f64::INFINITY)] {
            btc.ret_7 = missing;
            assert!(market_context::values(Some(&btc), stamp.date_naive()).is_err());
        }
        btc.ret_7 = Some(0.1);
        btc.vol_30 = Some(0.0);
        assert!(market_context::values(Some(&btc), stamp.date_naive()).is_err());
    }

    #[test]
    fn ranker_appends_common_btc_level_without_ranking_it_or_changing_legacy_models() {
        let path = std::env::temp_dir().join(format!(
            "crypto-market-inference-{}.json",
            std::process::id()
        ));
        let mut model = serde_json::json!({
            "format_version":model::FORMAT_VERSION,"model_version":model::MODEL_VERSION,
            "feature_set_version":features_crypto::FEATURE_SET_VERSION,
            "trained_through":"2025-01-01","trained_at":"fixture","n_rows":12,"n_dates":1,
            "features":["x_ret_7"],"reward":"return",
            "tree_info":[{"tree_index":0,"num_leaves":2,"tree_structure":{
                "split_feature":0,"threshold":0.0,"decision_type":"<=",
                "left_child":{"leaf_value":-0.01},"right_child":{"leaf_value":0.01}
            }}]
        });
        let mut cfg = config();
        cfg.signal = "ml_ranker".into();
        cfg.model_path = Some(path.display().to_string());
        let stamp: DateTime<Utc> = "2026-01-07T00:00:00Z".parse().unwrap();
        let rows: Vec<_> = (0..12)
            .map(|i| {
                let mut row = row(&format!("A{i}"), stamp, 100.0);
                row.ret_7 = Some(i as f64);
                row
            })
            .collect();
        let cross: Vec<_> = rows.iter().collect();
        let btc = row("BTC", stamp, 100.0);
        let generate = |context| {
            generate_signal(
                &cross,
                &BTreeMap::new(),
                stamp.date_naive(),
                &cfg,
                None,
                context,
            )
        };
        std::fs::write(&path, serde_json::to_vec(&model).unwrap()).unwrap();
        let without = generate(None).unwrap();
        let with = generate(Some(&btc)).unwrap();
        assert_eq!(without.values.len(), with.values.len());
        for (a, b) in without.values.iter().zip(&with.values) {
            assert_eq!(
                (&a.asset, a.direction, a.conviction),
                (&b.asset, b.direction, b.conviction)
            );
        }
        model["market_context"] = market_context::CONTRACT.into();
        model["features"] = serde_json::json!(std::iter::once("x_ret_7")
            .chain(market_context::FEATURES)
            .collect::<Vec<_>>());
        model["tree_info"][0]["tree_structure"]["split_feature"] = 1.into();
        std::fs::write(&path, serde_json::to_vec(&model).unwrap()).unwrap();
        assert!(generate(None).is_err());
        let positive = generate(Some(&btc)).unwrap();
        assert!(positive
            .values
            .iter()
            .all(|signal| signal.direction == Direction::Long));
        assert!(positive.values.iter().all(|signal| signal.asset != "BTC"));
        let mut negative_btc = btc.clone();
        negative_btc.ret_7 = Some(-0.1);
        let negative = generate(Some(&negative_btc)).unwrap();
        assert!(negative
            .values
            .iter()
            .all(|signal| signal.direction == Direction::Short));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn native_funding_adjusts_only_scored_convictions_and_preserves_unavailable() {
        let at = "2026-01-08T00:00:00Z".parse().unwrap();
        let values = [
            ("PAY", Direction::Long, Decimal::new(1, 2)),
            ("EARN", Direction::Short, Decimal::new(1, 2)),
            ("DROP", Direction::Long, Decimal::new(1, 2)),
            ("ZERO", Direction::Short, Decimal::ZERO),
            ("UNKNOWN", Direction::Long, Decimal::new(1, 2)),
        ]
        .into_iter()
        .map(|(asset, direction, conviction)| Signal {
            asset: asset.into(),
            direction,
            conviction,
            volatility: Some(Decimal::new(5, 1)),
        })
        .collect();
        let mut generated = Signals {
            values,
            notes: Vec::new(),
            warnings: Vec::new(),
            scoring_version: "fixture".into(),
            model_id: None,
        };
        let mut forecast = funding::NativeForecast {
            as_of: at,
            assets: BTreeMap::new(),
        };
        for asset in ["PAY", "EARN", "ZERO"] {
            forecast.assets.insert(
                asset.into(),
                funding::NativeEstimate::Available {
                    long_cost_24h: Decimal::new(1, 3),
                },
            );
        }
        forecast.assets.insert(
            "DROP".into(),
            funding::NativeEstimate::Available {
                long_cost_24h: Decimal::new(2, 2),
            },
        );
        forecast.assets.insert(
            "UNKNOWN".into(),
            funding::NativeEstimate::Unavailable {
                observed_hours: 167,
            },
        );
        adjust_native_funding(&mut generated, &forecast).unwrap();
        assert_eq!(generated.values.len(), 3);
        let values: BTreeMap<_, _> = generated
            .values
            .iter()
            .map(|s| (s.asset.as_str(), (s.direction, s.conviction)))
            .collect();
        assert_eq!(values["PAY"], (Direction::Long, Decimal::new(9, 3)));
        assert_eq!(values["EARN"], (Direction::Short, Decimal::new(11, 3)));
        assert_eq!(values["UNKNOWN"], (Direction::Long, Decimal::new(1, 2)));
        assert!(generated
            .warnings
            .iter()
            .any(|w| w.message.contains("UNKNOWN") && w.message.contains("167/168")));
    }

    #[test]
    fn native_funding_requires_explicit_24h_model_without_changing_raw_predictions() {
        let path =
            std::env::temp_dir().join(format!("native-carry-model-{}.json", std::process::id()));
        let mut model = serde_json::json!({
            "format_version":model::FORMAT_VERSION,"model_version":model::MODEL_VERSION,
            "feature_set_version":features_crypto::FEATURE_SET_VERSION,
            "trained_through":"2025-01-01","trained_at":"fixture","n_rows":12,"n_dates":1,
            "features":["x_ret_7"],"reward":"return",
            "tree_info":[{"tree_index":0,"num_leaves":1,"tree_structure":{"leaf_value":0.01}}]
        });
        let mut cfg = config();
        cfg.signal = "ml_ranker".into();
        cfg.model_path = Some(path.display().to_string());
        let stamp: DateTime<Utc> = "2026-01-07T00:00:00Z".parse().unwrap();
        let rows: Vec<_> = (0..12)
            .map(|i| row(&format!("A{i}"), stamp, 100.0))
            .collect();
        let cross: Vec<_> = rows.iter().collect();
        let generate = |cfg: &Config| {
            generate_signal(
                &cross,
                &BTreeMap::new(),
                stamp.date_naive(),
                cfg,
                None,
                None,
            )
        };
        std::fs::write(&path, serde_json::to_vec(&model).unwrap()).unwrap();
        let baseline = generate(&cfg).unwrap();
        cfg.native_funding_adjustment = true;
        assert!(generate(&cfg).err().unwrap().contains("explicit 24h"));
        model["label_horizon_hours"] = serde_json::json!(120);
        model["label_lag_hours"] = serde_json::json!(1);
        std::fs::write(&path, serde_json::to_vec(&model).unwrap()).unwrap();
        assert!(generate(&cfg).err().unwrap().contains("explicit 24h"));
        model["label_horizon_hours"] = serde_json::json!(24);
        std::fs::write(&path, serde_json::to_vec(&model).unwrap()).unwrap();
        let native = generate(&cfg).unwrap();
        assert_eq!(native.values.len(), baseline.values.len());
        for (left, right) in native.values.iter().zip(&baseline.values) {
            assert_eq!(
                (
                    &left.asset,
                    left.direction,
                    left.conviction,
                    left.volatility
                ),
                (
                    &right.asset,
                    right.direction,
                    right.conviction,
                    right.volatility
                )
            );
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn native_funding_input_is_ignored_when_disabled_and_required_when_enabled() {
        let mut cfg = config();
        cfg.limits.max_cluster_exposure = None;
        cfg.limits.max_net_exposure = None;
        let as_of: DateTime<Utc> = "2026-08-01T00:00:00Z".parse().unwrap();
        let features = vec![row("BTC", as_of - TimeDelta::days(1), 100.0)];
        let universe = ["BTC".into()].into_iter().collect();
        let portfolio = Portfolio {
            cash: Decimal::from(100000),
            positions: Vec::new(),
        };
        let run = |cfg: &Config, native_funding| {
            decide(DecisionInput {
                native_funding,
                sizing_liquidity: None,
                as_of,
                created_at: as_of,
                mode: Mode::Dry,
                config: cfg,
                daily_features: &features,
                hourly_features: None,
                eligible_universe: &universe,
                portfolio: &portfolio,
                inputs_hash: "native-carry-fixture",
            })
        };
        let future = funding::NativeForecast {
            as_of: as_of + TimeDelta::hours(1),
            assets: BTreeMap::new(),
        };
        let baseline = run(&cfg, None).unwrap();
        assert!(!baseline.plan.orders.is_empty());
        assert_eq!(
            canonical_json(&baseline.plan).unwrap(),
            canonical_json(&run(&cfg, Some(&future)).unwrap().plan).unwrap()
        );
        cfg.native_funding_adjustment = true;
        assert!(run(&cfg, None)
            .err()
            .unwrap()
            .contains("dated native forecast"));
        assert!(run(&cfg, Some(&future))
            .err()
            .unwrap()
            .contains("does not match decision"));
    }

    #[test]
    fn regime_requires_finite_completed_btc_inputs_and_zero_selects_ranker() {
        let at: DateTime<Utc> = "2026-01-07T00:00:00Z".parse().unwrap();
        let mut btc = row("BTC", at, 100.0);
        assert!(btc_regime_signal(None, at).is_err());
        assert!(btc_regime_signal(Some(&btc), at + TimeDelta::days(1)).is_err());
        for value in [
            None,
            Some(f64::NAN),
            Some(f64::INFINITY),
            Some(f64::NEG_INFINITY),
        ] {
            btc.ret_30 = value;
            assert!(btc_regime_signal(Some(&btc), at).is_err());
        }
        btc.ret_30 = Some(0.0);
        for value in [
            None,
            Some(0.0),
            Some(-0.5),
            Some(f64::NAN),
            Some(f64::INFINITY),
        ] {
            btc.vol_30 = value;
            assert!(btc_regime_signal(Some(&btc), at).is_err());
        }
        btc.vol_30 = Some(0.5);
        assert_eq!(btc_regime_signal(Some(&btc), at).unwrap().0, Decimal::ZERO);
        btc.ret_30 = Some(-0.001);
        assert_eq!(btc_regime_signal(Some(&btc), at).unwrap().0, Decimal::ZERO);
        btc.ret_30 = Some(0.001);
        let (fraction, signal) = btc_regime_signal(Some(&btc), at).unwrap();
        assert_eq!(fraction, Decimal::ONE);
        assert_eq!(signal.direction, Direction::Long);
        assert_eq!(signal.conviction, Decimal::ONE);
        assert_eq!(signal.volatility, Some(Decimal::new(5, 1)));
    }

    #[test]
    fn regime_zero_fraction_keeps_component_caps_and_shared_limits_explicit() {
        let mut cfg = config();
        cfg.ranker_btc_regime = true;
        cfg.limits.max_position = Decimal::new(8, 1);
        cfg.limits.max_net_exposure = Some(Decimal::new(8, 1));
        cfg.limits.max_cluster_exposure = Some(Decimal::new(8, 1));
        let (_, ranker, _) = blend_component_configs(&cfg, Decimal::ZERO);
        assert_eq!(ranker.limits.max_position, Decimal::new(25, 2));
        assert_eq!(ranker.limits.max_net_exposure, Some(Decimal::new(1, 1)));
        assert_eq!(ranker.limits.max_cluster_exposure, Some(Decimal::new(4, 1)));
        assert!(!ranker.ranker_btc_regime);
        assert_eq!(cfg.limits.max_position, Decimal::new(8, 1));
        assert_eq!(ranker.turnover_budget, cfg.turnover_budget);
    }

    #[test]
    fn regime_decision_is_causal_uses_one_book_and_checks_only_active_component() {
        let path =
            std::env::temp_dir().join(format!("crypto-regime-model-{}.json", std::process::id()));
        let model = serde_json::json!({
            "format_version":model::FORMAT_VERSION,"model_version":model::MODEL_VERSION,
            "feature_set_version":features_crypto::FEATURE_SET_VERSION,
            "trained_through":"2025-01-01","trained_at":"fixture","n_rows":12,"n_dates":1,
            "features":["x_ret_7"],"reward":"return",
            "tree_info":[{"tree_index":0,"num_leaves":2,"tree_structure":{
                "split_feature":0,"threshold":0.0,"decision_type":"<=",
                "left_child":{"leaf_value":-0.01},"right_child":{"leaf_value":0.01}
            }}]
        });
        std::fs::write(&path, serde_json::to_vec(&model).unwrap()).unwrap();
        let mut cfg = config();
        cfg.signal = "ml_ranker".into();
        cfg.constructor = "risk_adjusted_score_selected".into();
        cfg.model_path = Some(path.display().to_string());
        cfg.ranker_btc_regime = true;
        cfg.target_gross_exposure = Decimal::new(8, 1);
        cfg.turnover_budget = Decimal::from(2);
        cfg.limits.max_position = Decimal::new(8, 1);
        cfg.limits.max_net_exposure = Some(Decimal::new(8, 1));
        cfg.limits.max_cluster_exposure = Some(Decimal::new(8, 1));
        cfg.clusters.clear();
        let as_of: DateTime<Utc> = "2026-01-08T00:00:00Z".parse().unwrap();
        let mut features: Vec<_> = (0..12)
            .map(|i| {
                let asset = if i == 0 {
                    "BTC".into()
                } else {
                    format!("A{i}")
                };
                let mut feature = row(&asset, as_of - TimeDelta::days(1), 100.0);
                feature.ret_7 = Some(i as f64 / 100.0);
                feature
            })
            .collect();
        let universe: BTreeSet<_> = features.iter().map(|r| r.asset.clone()).collect();
        let portfolio = Portfolio {
            cash: Decimal::from(100000),
            positions: Vec::new(),
        };
        let run = |cfg: &Config, features: &[DailyRow]| {
            decide(DecisionInput {
                native_funding: None,
                sizing_liquidity: None,
                as_of,
                created_at: as_of,
                mode: Mode::Dry,
                config: cfg,
                daily_features: features,
                hourly_features: None,
                eligible_universe: &universe,
                portfolio: &portfolio,
                inputs_hash: "regime-fixture",
            })
        };
        let without_btc: BTreeSet<_> = universe
            .iter()
            .filter(|a| a.as_str() != "BTC")
            .cloned()
            .collect();
        let run_without_btc = |features: &[DailyRow]| {
            decide(DecisionInput {
                native_funding: None,
                sizing_liquidity: None,
                as_of,
                created_at: as_of,
                mode: Mode::Dry,
                config: &cfg,
                daily_features: features,
                hourly_features: None,
                eligible_universe: &without_btc,
                portfolio: &portfolio,
                inputs_hash: "regime-unavailable-btc",
            })
        };
        assert!(run_without_btc(&features)
            .unwrap_err()
            .contains("positive state requires eligible BTC"));
        let mut negative_context = features.clone();
        negative_context[0].ret_30 = Some(-0.01);
        let negative_without_btc = run_without_btc(&negative_context).unwrap();
        assert_eq!(negative_without_btc.plan.status, Status::Accepted);
        assert!(negative_without_btc
            .plan
            .targets
            .iter()
            .all(|target| target.asset != "BTC"));
        let long = run(&cfg, &features).unwrap();
        assert_eq!(long.plan.status, Status::Accepted);
        assert_eq!(long.plan.targets.len(), 1);
        assert_eq!(long.plan.targets[0].asset, "BTC");
        assert_eq!(long.plan.targets[0].weight, Decimal::new(8, 1));
        assert_eq!(long.plan.targets[0].conviction, Some(Decimal::ONE));
        assert_eq!(long.plan.orders.len(), 1);
        assert_eq!(long.plan.orders[0].side, Side::Buy);
        assert!(!long
            .plan
            .risk_report
            .checks
            .iter()
            .any(|c| c.name.starts_with("ranker.")));
        assert!(long
            .plan
            .risk_report
            .checks
            .iter()
            .any(|c| c.name == "btc_momentum.max_position"));
        let mut tight = cfg.clone();
        tight.limits.max_position = Decimal::new(25, 2);
        let rejected_long = run(&tight, &features).unwrap();
        assert_eq!(rejected_long.plan.status, Status::Rejected);
        assert!(rejected_long
            .plan
            .risk_report
            .checks
            .iter()
            .any(|c| c.name == "max_position" && !c.passed));
        let held = Portfolio {
            cash: portfolio.cash,
            positions: vec![Position {
                asset: "A1".into(),
                qty: Decimal::from(100),
            }],
        };
        let transition = decide(DecisionInput {
            native_funding: None,
            sizing_liquidity: None,
            as_of,
            created_at: as_of,
            mode: Mode::Dry,
            config: &cfg,
            daily_features: &features,
            hourly_features: None,
            eligible_universe: &universe,
            portfolio: &held,
            inputs_hash: "regime-transition",
        })
        .unwrap();
        assert_eq!(transition.plan.status, Status::Accepted);
        assert_eq!(transition.plan.targets.len(), 1);
        assert_eq!(transition.plan.orders.len(), 2);
        assert!(transition
            .plan
            .orders
            .iter()
            .any(|o| o.asset == "A1" && o.side == Side::Sell));
        assert!(transition
            .plan
            .orders
            .iter()
            .any(|o| o.asset == "BTC" && o.side == Side::Buy));
        let mut future = features[0].clone();
        future.ts_utc = as_of;
        future.ret_30 = Some(-0.9);
        future.vol_30 = None;
        features.push(future);
        assert_eq!(
            canonical_json(&long.plan).unwrap(),
            canonical_json(&run(&cfg, &features).unwrap().plan).unwrap()
        );
        features[0].ret_30 = Some(0.0);
        let zero = run(&cfg, &features).unwrap();
        assert_eq!(zero.plan.status, Status::Accepted);
        assert!(zero.plan.targets.len() > 1);
        assert!(!zero
            .plan
            .risk_report
            .checks
            .iter()
            .any(|c| c.name.starts_with("btc_momentum.")));
        for (name, limit) in [
            ("ranker.max_position", Decimal::new(25, 2)),
            ("ranker.max_net_exposure", Decimal::new(1, 1)),
            ("ranker.max_cluster_exposure", Decimal::new(4, 1)),
        ] {
            assert!(zero
                .plan
                .risk_report
                .checks
                .iter()
                .any(|c| c.name == name && c.limit == limit));
        }
        let (_, component, _) = blend_component_configs(&cfg, Decimal::ZERO);
        let expected = run(&component, &features).unwrap();
        assert_eq!(
            serde_json::to_value(&zero.plan.targets).unwrap(),
            serde_json::to_value(&expected.plan.targets).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&zero.plan.orders).unwrap(),
            serde_json::to_value(&expected.plan.orders).unwrap()
        );
        cfg.clusters = universe
            .iter()
            .map(|a| (a.clone(), "same".into()))
            .collect();
        let rejected = run(&cfg, &features).unwrap();
        assert_eq!(rejected.plan.status, Status::Rejected);
        assert!(rejected
            .plan
            .risk_report
            .checks
            .iter()
            .any(|c| c.name == "ranker.max_cluster_exposure" && !c.passed));
        for bad in [None, Some(f64::NAN)] {
            features[0].ret_30 = bad;
            assert!(run(&cfg, &features)
                .unwrap_err()
                .contains("finite BTC ret_30"));
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn blend_nets_opposing_btc_targets_without_reallocating_cancelled_gross() {
        let make = |weights| Construction {
            weights,
            notes: Vec::new(),
            constructor: "fixture".into(),
        };
        let ranker = BTreeMap::from([
            ("BTC".into(), Decimal::new(-2, 1)),
            ("ETH".into(), Decimal::new(2, 1)),
        ]);
        let momentum = BTreeMap::from([("BTC".into(), Decimal::new(6, 1))]);
        let blended = blend_targets(make(ranker.clone()), make(momentum), Decimal::new(5, 1));
        assert_eq!(blended.weights["BTC"], Decimal::new(2, 1));
        assert_eq!(blended.weights["ETH"], Decimal::new(1, 1));
        assert_eq!(
            blended.weights.values().copied().sum::<Decimal>(),
            Decimal::new(3, 1)
        );
        let cancelled = blend_targets(
            make(ranker.clone()),
            make(BTreeMap::from([("BTC".into(), Decimal::new(2, 1))])),
            Decimal::new(5, 1),
        );
        assert!(!cancelled.weights.contains_key("BTC"));
        assert_eq!(cancelled.weights["ETH"], Decimal::new(1, 1));
        assert_eq!(
            blend_targets(make(ranker.clone()), make(BTreeMap::new()), Decimal::ZERO).weights,
            ranker
        );
    }

    #[test]
    fn blend_component_limits_do_not_inherit_account_position_caps() {
        let mut cfg = config();
        cfg.max_holdings = 17;
        cfg.limits.max_position = Decimal::new(8, 1);
        assert!(blend_configs(&cfg).is_none());
        cfg.ranker_btc_momentum_fraction = Some(Decimal::ZERO);
        assert!(blend_configs(&cfg).is_none());
        cfg.ranker_btc_momentum_fraction = Some(Decimal::new(5, 1));
        let (_, ranker, momentum) = blend_configs(&cfg).unwrap();
        assert_eq!(ranker.max_holdings, 17);
        assert_eq!(ranker.limits.max_position, Decimal::new(25, 2));
        assert_eq!(ranker.limits.max_net_exposure, Some(Decimal::new(1, 1)));
        assert_eq!(ranker.limits.max_cluster_exposure, Some(Decimal::new(4, 1)));
        assert_eq!(momentum.max_holdings, 1);
        assert!(momentum.benchmark_only);
        assert_eq!(momentum.limits.max_position, Decimal::new(8, 1));
        assert_eq!(momentum.target_gross_exposure, Decimal::new(8, 1));
        assert_eq!(cfg.max_holdings, 17);
    }

    #[test]
    fn blend_decision_preserves_zero_control_and_nets_once_with_component_risk() {
        let path =
            std::env::temp_dir().join(format!("crypto-blend-model-{}.json", std::process::id()));
        let model = serde_json::json!({
            "format_version":model::FORMAT_VERSION,"model_version":model::MODEL_VERSION,
            "feature_set_version":features_crypto::FEATURE_SET_VERSION,
            "trained_through":"2025-01-01","trained_at":"fixture","n_rows":12,"n_dates":1,
            "features":["x_ret_7"],"reward":"return",
            "tree_info":[{"tree_index":0,"num_leaves":2,"tree_structure":{
                "split_feature":0,"threshold":0.0,"decision_type":"<=",
                "left_child":{"leaf_value":-0.01},"right_child":{"leaf_value":0.01}
            }}]
        });
        std::fs::write(&path, serde_json::to_vec(&model).unwrap()).unwrap();
        let mut cfg = config();
        cfg.signal = "ml_ranker".into();
        cfg.constructor = "risk_adjusted_score_selected".into();
        cfg.model_path = Some(path.display().to_string());
        cfg.target_gross_exposure = Decimal::new(8, 1);
        cfg.turnover_budget = Decimal::from(2);
        cfg.limits.max_position = Decimal::new(8, 1);
        cfg.limits.max_net_exposure = Some(Decimal::new(8, 1));
        cfg.limits.max_cluster_exposure = Some(Decimal::new(8, 1));
        cfg.clusters.clear();
        let as_of: DateTime<Utc> = "2026-01-08T00:00:00Z".parse().unwrap();
        let mut features: Vec<_> = (0..12)
            .map(|i| {
                let asset = if i == 0 {
                    "BTC".into()
                } else {
                    format!("A{i}")
                };
                let mut feature = row(&asset, as_of - TimeDelta::days(1), 100.0);
                feature.ret_7 = Some(i as f64 / 100.0);
                feature.ret_180 = Some(0.4);
                feature
            })
            .collect();
        let universe: BTreeSet<_> = features.iter().map(|row| row.asset.clone()).collect();
        let portfolio = Portfolio {
            cash: Decimal::from(100000),
            positions: Vec::new(),
        };
        let run = |cfg: &Config, features: &[DailyRow]| {
            decide(DecisionInput {
                native_funding: None,
                sizing_liquidity: None,
                as_of,
                created_at: as_of,
                mode: Mode::Dry,
                config: cfg,
                daily_features: features,
                hourly_features: None,
                eligible_universe: &universe,
                portfolio: &portfolio,
                inputs_hash: "blend-fixture",
            })
            .unwrap()
        };
        let original = run(&cfg, &features);
        assert_eq!(original.plan.status, Status::Accepted);
        assert!(!original.plan.orders.is_empty());
        cfg.ranker_btc_momentum_fraction = Some(Decimal::ZERO);
        assert_eq!(
            canonical_json(&run(&cfg, &features).plan).unwrap(),
            canonical_json(&original.plan).unwrap()
        );
        cfg.ranker_btc_momentum_fraction = Some(Decimal::new(5, 1));
        let blended = run(&cfg, &features);
        assert_eq!(blended.plan.status, Status::Accepted);
        let btc_orders: Vec<_> = blended
            .plan
            .orders
            .iter()
            .filter(|order| order.asset == "BTC")
            .collect();
        assert_eq!(btc_orders.len(), 1);
        assert_eq!(btc_orders[0].side, Side::Buy);
        assert!(blended
            .plan
            .targets
            .iter()
            .all(|target| target.conviction.is_none()));
        assert!(
            blended
                .plan
                .risk_report
                .checks
                .iter()
                .any(|check| check.name == "ranker.max_position"
                    && check.limit == Decimal::new(25, 2))
        );
        let mut future = features[0].clone();
        future.ts_utc = as_of;
        future.ret_30 = Some(-0.9);
        future.ret_90 = Some(-0.9);
        future.ret_180 = Some(-0.9);
        features.push(future);
        assert_eq!(
            canonical_json(&run(&cfg, &features).plan).unwrap(),
            canonical_json(&blended.plan).unwrap()
        );
        cfg.clusters = universe
            .iter()
            .map(|asset| (asset.clone(), "same".into()))
            .collect();
        let rejected = run(&cfg, &features);
        assert_eq!(rejected.plan.status, Status::Rejected);
        assert!(rejected.plan.orders.is_empty());
        assert!(rejected
            .plan
            .risk_report
            .checks
            .iter()
            .any(|check| check.name == "ranker.max_cluster_exposure" && !check.passed));
        assert!(rejected
            .plan
            .risk_report
            .checks
            .iter()
            .any(|check| check.name == "max_cluster_exposure" && check.passed));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn benchmark_direction_keeps_the_existing_signal_and_eligibility() {
        let at: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        let mut btc = row("BTC", at, 100.0);
        let mut eth = row("ETH", at, 100.0);
        btc.ret_180 = Some(0.4);
        eth.ret_180 = Some(0.4);
        let mut cfg = config();
        cfg.signal = "tsmom_ensemble".into();
        cfg.benchmark = Some("BTC".into());
        let both = generate_signal(
            &[&btc, &eth],
            &BTreeMap::new(),
            at.date_naive(),
            &cfg,
            None,
            None,
        )
        .unwrap();
        assert_eq!(both.values.len(), 2);
        cfg.benchmark_only = true;
        let one = generate_signal(
            &[&btc, &eth],
            &BTreeMap::new(),
            at.date_naive(),
            &cfg,
            None,
            None,
        )
        .unwrap();
        assert_eq!(one.values.len(), 1);
        assert_eq!(one.values[0].asset, "BTC");
        assert_eq!(
            one.values[0].conviction,
            both.values
                .iter()
                .find(|s| s.asset == "BTC")
                .unwrap()
                .conviction
        );
        btc.adv_quote = Some(0.0);
        assert!(generate_signal(
            &[&btc, &eth],
            &BTreeMap::new(),
            at.date_naive(),
            &cfg,
            None,
            None
        )
        .unwrap()
        .values
        .is_empty());
    }

    #[test]
    fn fast_momentum_responds_to_recovery_without_the_180_day_feature() {
        let at: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        let mut btc = row("BTC", at, 100.0);
        btc.ret_7 = Some(0.1);
        btc.ret_30 = Some(0.1);
        btc.ret_90 = Some(-0.2);
        btc.ret_180 = Some(-0.3);
        let mut cfg = config();
        cfg.signal = "tsmom_ensemble".into();
        let slow =
            generate_signal(&[&btc], &BTreeMap::new(), at.date_naive(), &cfg, None, None).unwrap();
        assert_eq!(slow.values[0].direction, Direction::Short);
        cfg.signal = "tsmom_fast_ensemble".into();
        btc.ret_180 = None;
        let fast =
            generate_signal(&[&btc], &BTreeMap::new(), at.date_naive(), &cfg, None, None).unwrap();
        assert_eq!(fast.values[0].direction, Direction::Long);
        btc.ret_7 = None;
        assert!(
            generate_signal(&[&btc], &BTreeMap::new(), at.date_naive(), &cfg, None, None)
                .unwrap()
                .values
                .is_empty()
        );
    }

    #[test]
    fn consensus_requires_the_same_asset_and_direction() {
        let signal = |asset: &str, direction| Signal {
            asset: asset.into(),
            direction,
            conviction: Decimal::ONE,
            volatility: Some(Decimal::new(5, 1)),
        };
        for (other, direction, count) in [
            ("BTC", Direction::Long, 1),
            ("BTC", Direction::Short, 0),
            ("ETH", Direction::Long, 0),
        ] {
            let output = directional_agreement(
                vec![signal("BTC", Direction::Long)],
                &[signal(other, direction)],
            );
            assert_eq!(output.len(), count);
        }
        assert!(directional_agreement(vec![signal("BTC", Direction::Long)], &[]).is_empty());
    }

    #[test]
    fn ml_funding_adjustment_is_opt_in_and_filters_net_price_conviction() {
        let path =
            std::env::temp_dir().join(format!("crypto-funding-model-{}.json", std::process::id()));
        let model = serde_json::json!({
            "format_version": model::FORMAT_VERSION, "model_version": model::MODEL_VERSION,
            "feature_set_version": features_crypto::FEATURE_SET_VERSION,
            "trained_through": "2025-01-01", "trained_at": "fixture", "n_rows": 12,
            "n_dates": 1, "features": ["x_ret_7"], "reward": "return",
            "tree_info": [{"tree_index": 0, "num_leaves": 1, "tree_structure": {"leaf_value": 0.01}}]
        });
        std::fs::write(&path, serde_json::to_vec(&model).unwrap()).unwrap();
        let mut cfg = config();
        cfg.signal = "ml_ranker".into();
        cfg.model_path = Some(path.display().to_string());
        let stamp: DateTime<Utc> = "2026-01-07T00:00:00Z".parse().unwrap();
        let mut rows: Vec<_> = (0..12)
            .map(|i| {
                let mut row = row(&format!("A{i:02}"), stamp, 100.0);
                row.funding_7d = Some(0.0);
                row
            })
            .collect();
        rows[0].funding_7d = Some(0.007);
        rows[1].funding_7d = Some(-0.007);
        rows[2].funding_7d = Some(0.07);
        let generate = |cfg: &Config, rows: &[DailyRow]| {
            generate_signal(
                &rows.iter().collect::<Vec<_>>(),
                &BTreeMap::new(),
                stamp.date_naive(),
                cfg,
                None,
                None,
            )
            .unwrap()
        };
        assert!(!cfg.funding_cost_adjustment);
        let baseline = generate(&cfg, &rows);
        assert_eq!(baseline.values.len(), 12);
        assert!(baseline
            .values
            .iter()
            .all(|signal| signal.conviction == Decimal::new(1, 2)));
        cfg.funding_cost_adjustment = true;
        let adjusted = generate(&cfg, &rows);
        let values: BTreeMap<_, _> = adjusted
            .values
            .iter()
            .map(|signal| (signal.asset.as_str(), signal.conviction))
            .collect();
        assert_eq!(values.len(), 11);
        assert_eq!(values["A00"], Decimal::new(9, 3));
        assert_eq!(values["A01"], Decimal::new(1, 2));
        assert!(!values.contains_key("A02"));
        assert!(adjusted
            .values
            .iter()
            .all(|signal| signal.direction == Direction::Long));
        rows[0].funding_7d = None;
        assert!(generate_signal(
            &rows.iter().collect::<Vec<_>>(),
            &BTreeMap::new(),
            stamp.date_naive(),
            &cfg,
            None,
            None
        )
        .is_err());
        cfg.funding_cost_adjustment = false;
        let unchanged = generate(&cfg, &rows);
        assert_eq!(unchanged.values.len(), baseline.values.len());
        assert_eq!(
            unchanged
                .values
                .iter()
                .map(|s| s.conviction)
                .collect::<Vec<_>>(),
            baseline
                .values
                .iter()
                .map(|s| s.conviction)
                .collect::<Vec<_>>()
        );
        for row in &mut rows {
            row.funding_7d = Some(0.0);
            row.vol_30 = Some(0.5);
        }
        rows[0].funding_7d = Some(0.007);
        cfg.funding_cost_adjustment = true;
        for reward in ["per_risk", "per_risk_abs"] {
            let mut scaled_model = model.clone();
            scaled_model["reward"] = reward.into();
            std::fs::write(&path, serde_json::to_vec(&scaled_model).unwrap()).unwrap();
            let scaled = generate(&cfg, &rows);
            assert_eq!(
                scaled
                    .values
                    .iter()
                    .find(|s| s.asset == "A00")
                    .unwrap()
                    .conviction,
                Decimal::new(4, 3)
            );
        }
        let mut longer_model = model.clone();
        longer_model["label_horizon_hours"] = 72.into();
        std::fs::write(&path, serde_json::to_vec(&longer_model).unwrap()).unwrap();
        let result = generate_signal(
            &rows.iter().collect::<Vec<_>>(),
            &BTreeMap::new(),
            stamp.date_naive(),
            &cfg,
            None,
            None,
        );
        assert!(result.unwrap_err().contains("24h model label horizon"));
        cfg.funding_cost_adjustment = false;
        assert_eq!(generate(&cfg, &rows).values.len(), 12);
        longer_model["label_horizon_hours"] = 24.into();
        std::fs::write(&path, serde_json::to_vec(&longer_model).unwrap()).unwrap();
        cfg.funding_cost_adjustment = true;
        assert_eq!(generate(&cfg, &rows).values.len(), 12);
        cfg.funding_cost_adjustment = false;
        longer_model["label_horizon_hours"] = 72.into();
        longer_model["label_lag_hours"] = 1.into();
        longer_model["trained_through"] = "2026-01-04".into();
        std::fs::write(&path, serde_json::to_vec(&longer_model).unwrap()).unwrap();
        // Jan 7 features are used at the Jan 8 decision, after labels ended.
        assert_eq!(generate(&cfg, &rows).values.len(), 12);
        assert!(generate_signal(
            &rows.iter().collect::<Vec<_>>(),
            &BTreeMap::new(),
            "2026-01-06".parse().unwrap(),
            &cfg,
            None,
            None
        )
        .unwrap_err()
        .contains("training contains the answer"));
        std::fs::remove_file(path).unwrap();
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
    fn capacity_balance_reduces_the_larger_sleeve_and_preserves_ratios() {
        let mut weights = BTreeMap::from([
            ("A".into(), Decimal::new(3, 1)),
            ("B".into(), Decimal::new(1, 1)),
            ("C".into(), -Decimal::new(2, 1)),
        ]);
        balance_capacity(&mut weights);
        assert_eq!(weights["A"], Decimal::new(15, 2));
        assert_eq!(weights["B"], Decimal::new(5, 2));
        assert_eq!(weights.values().copied().sum::<Decimal>(), Decimal::ZERO);
        weights.remove("C");
        balance_capacity(&mut weights);
        assert!(weights.values().all(Decimal::is_zero));
    }

    #[test]
    fn known_zero_capacity_cannot_receive_redistributed_weight() {
        let mut cfg = config();
        cfg.limits.participation_limit = Decimal::new(1, 2);
        let profile = liquidity::Profile {
            assets: BTreeMap::from([(
                "A".into(),
                liquidity::AssetLiquidity {
                    hourly_quote_volume: Some(Decimal::ZERO),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        let mut weights = BTreeMap::from([("A".into(), Decimal::new(1, 1))]);
        cap_by_participation(&mut weights, &cfg, Decimal::from(100000), Some(&profile));
        assert_eq!(weights["A"], Decimal::ZERO);
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

        // A: 0.30 -> 0.20 and C: 0.30 -> 0.10 are reductions, so the cap has
        // no say in them however tight it is. B: 0.10 -> 0.45 is an addition
        // and gets the whole 0.40 allowance to itself — the reductions do not
        // eat into it, or a heavy de-risking day would leave nothing to buy
        // with and the book would shed what the model dropped without ever
        // establishing what it now wants.
        let by: BTreeMap<_, _> = orders.iter().map(|o| (o.asset.as_str(), o)).collect();
        assert_eq!(
            orders.len(),
            3,
            "got {:?}",
            orders.iter().map(|o| &o.asset).collect::<Vec<_>>()
        );
        assert_eq!(by["A"].reason, OrderReason::Reduce);
        assert_eq!(by["C"].reason, OrderReason::Reduce);
        assert_eq!(by["B"].reason, OrderReason::Increase);
        assert_eq!(
            used.spent,
            Decimal::new(35, 2),
            "only B consumes the allowance"
        );
        assert_eq!(
            used.turned_over,
            Decimal::new(65, 2),
            "but the book really moved 0.65, and the disclosure must say so"
        );
        assert_eq!(dropped, Decimal::ZERO);
        assert!(
            skipped.iter().all(|m| !m.contains("turnover")),
            "{skipped:?}"
        );
    }

    /// A reversal day: the book is +0.4 long / -0.4 short; the model exits the
    /// longs (free) and wants two new longs and one new short (budgeted, 0.10
    /// allowance). Without the guard the plan sheds 0.4 of longs, adds 0.10 of
    /// the largest new position, and the projected book goes deeply net short.
    /// With `max_net_exposure` set, the guard releases the deferred LONG
    /// additions past the budget until the projected book is inside the band -
    /// and never a short one, since that would push it further out.
    #[test]
    fn net_exposure_guard_releases_deferred_additions_on_the_light_side_only() {
        let mut cfg = config();
        cfg.turnover_budget = Decimal::new(1, 1);
        cfg.limits.min_position_notional = Decimal::ZERO;
        cfg.limits.max_net_exposure = Some(Decimal::new(1, 1)); // |net| <= 0.10
                                                                // Book today: A,B long 0.2 each; S1,S2 short 0.2 each. Net 0.
        let current = BTreeMap::from([
            ("A".into(), Decimal::new(2, 1)),
            ("B".into(), Decimal::new(2, 1)),
            ("S1".into(), Decimal::new(-2, 1)),
            ("S2".into(), Decimal::new(-2, 1)),
        ]);
        // Target: A,B out; L1 0.25 long, L2 0.15 long; S1,S2 kept; S3 0.15 short.
        let weights = BTreeMap::from([
            ("L1".into(), Decimal::new(25, 2)),
            ("L2".into(), Decimal::new(15, 2)),
            ("S1".into(), Decimal::new(-2, 1)),
            ("S2".into(), Decimal::new(-2, 1)),
            ("S3".into(), Decimal::new(-15, 2)),
        ]);
        let names = ["A", "B", "L1", "L2", "S1", "S2", "S3"];
        let prices: BTreeMap<String, Decimal> = names
            .iter()
            .map(|n| (n.to_string(), Decimal::from(100)))
            .collect();
        let adv: BTreeMap<String, Option<Decimal>> = names
            .iter()
            .map(|n| (n.to_string(), Some(Decimal::from(100_000_000))))
            .collect();
        let vol: BTreeMap<String, Option<Decimal>> = names
            .iter()
            .map(|n| (n.to_string(), Some(Decimal::new(5, 1))))
            .collect();
        let market = Market {
            prices: &prices,
            adv: &adv,
            vol: &vol,
            profile: None,
        };

        // Without the limit: exits A,B (free); L1 (0.25) is the largest
        // addition but exceeds the 0.10 allowance, as do L2 (0.15) and S3
        // (0.15) - so NOTHING is added, and the projected book is 0 long /
        // 0.4 short: net -0.40.
        let mut cfg_off = cfg.clone();
        cfg_off.limits.max_net_exposure = None;
        let (orders, _, _, used, dropped) = diff(
            &weights,
            &current,
            &market,
            Decimal::from(100_000),
            &cfg_off,
        );
        assert_eq!(
            orders.len(),
            2,
            "{:?}",
            orders.iter().map(|o| &o.asset).collect::<Vec<_>>()
        );
        assert_eq!(used.net_before_guard, Decimal::new(-4, 1));
        assert_eq!(used.net_after_guard, Decimal::new(-4, 1));
        assert_eq!(used.net_guard_released, Decimal::ZERO);
        assert_eq!(dropped, Decimal::new(55, 2));

        // With the limit: the guard releases L1 (largest first): net -0.15,
        // still outside 0.10; then L2: net 0.00, inside. S3 is never released
        // (it would push net the wrong way) and stays deferred.
        let (orders, _, skipped, used, dropped) =
            diff(&weights, &current, &market, Decimal::from(100_000), &cfg);
        let names_out: Vec<&str> = orders.iter().map(|o| o.asset.as_str()).collect();
        assert!(
            names_out.contains(&"L1") && names_out.contains(&"L2"),
            "{names_out:?}"
        );
        assert!(!names_out.contains(&"S3"), "{names_out:?}");
        assert_eq!(orders.len(), 4);
        assert_eq!(used.net_before_guard, Decimal::new(-4, 1));
        assert_eq!(used.net_after_guard, Decimal::ZERO);
        assert_eq!(used.net_guard_released, Decimal::new(4, 1));
        assert_eq!(
            used.spent,
            Decimal::ZERO,
            "released additions do not consume the allowance"
        );
        assert_eq!(used.turned_over, Decimal::new(8, 1));
        assert_eq!(dropped, Decimal::new(15, 2), "S3 remains deferred");
        assert!(
            skipped
                .iter()
                .any(|m| m.starts_with("net exposure guard: released 2")),
            "{skipped:?}"
        );
        assert!(
            skipped
                .iter()
                .any(|m| m == "S3: trade deferred by turnover budget"),
            "{skipped:?}"
        );
        assert!(
            !skipped
                .iter()
                .any(|m| m == "L1: trade deferred by turnover budget"),
            "{skipped:?}"
        );
    }

    /// The guard is a limit, not a target: a projected book already inside the
    /// band releases nothing, and the turnover budget stays in charge.
    #[test]
    fn net_exposure_guard_is_idle_inside_the_band() {
        let mut cfg = config();
        cfg.turnover_budget = Decimal::new(1, 1);
        cfg.limits.min_position_notional = Decimal::ZERO;
        cfg.limits.max_net_exposure = Some(Decimal::new(1, 1));
        let current = BTreeMap::from([
            ("A".into(), Decimal::new(2, 1)),
            ("S".into(), Decimal::new(-2, 1)),
        ]);
        // Wants to add 0.15 long and 0.15 short: net stays 0 either way; both
        // exceed the 0.10 allowance and both stay deferred.
        let weights = BTreeMap::from([
            ("A".into(), Decimal::new(2, 1)),
            ("S".into(), Decimal::new(-2, 1)),
            ("L".into(), Decimal::new(15, 2)),
            ("T".into(), Decimal::new(-15, 2)),
        ]);
        let names = ["A", "S", "L", "T"];
        let prices: BTreeMap<String, Decimal> = names
            .iter()
            .map(|n| (n.to_string(), Decimal::from(100)))
            .collect();
        let adv: BTreeMap<String, Option<Decimal>> = names
            .iter()
            .map(|n| (n.to_string(), Some(Decimal::from(100_000_000))))
            .collect();
        let vol: BTreeMap<String, Option<Decimal>> = names
            .iter()
            .map(|n| (n.to_string(), Some(Decimal::new(5, 1))))
            .collect();
        let market = Market {
            prices: &prices,
            adv: &adv,
            vol: &vol,
            profile: None,
        };
        let (orders, _, _, used, dropped) =
            diff(&weights, &current, &market, Decimal::from(100_000), &cfg);
        assert!(orders.is_empty());
        assert_eq!(used.net_guard_released, Decimal::ZERO);
        assert_eq!(dropped, Decimal::new(3, 1));
    }

    /// The cap still binds on additions, which is the whole point of keeping it.
    #[test]
    fn additions_still_compete_for_the_allowance() {
        let mut cfg = config();
        cfg.turnover_budget = Decimal::new(4, 1);
        cfg.limits.min_position_notional = Decimal::ZERO;
        // Three names the book does not hold, wanting 0.3 each: 0.9 of
        // additions against an allowance of 0.4.
        let weights = BTreeMap::from([
            ("A".into(), Decimal::new(3, 1)),
            ("B".into(), Decimal::new(3, 1)),
            ("C".into(), Decimal::new(3, 1)),
        ]);
        let current = BTreeMap::new();
        let three = |v: Decimal| {
            BTreeMap::from([
                ("A".to_string(), v),
                ("B".to_string(), v),
                ("C".to_string(), v),
            ])
        };
        let three_opt = |v: Decimal| {
            BTreeMap::from([
                ("A".to_string(), Some(v)),
                ("B".to_string(), Some(v)),
                ("C".to_string(), Some(v)),
            ])
        };
        let prices = three(Decimal::from(100));
        let adv = three_opt(Decimal::from(100_000_000));
        let vol = three_opt(Decimal::new(5, 1));

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
        assert_eq!(orders.len(), 1, "0.4 buys one 0.3 entry and no more");
        assert_eq!(used.spent, Decimal::new(3, 1));
        assert_eq!(used.turned_over, Decimal::new(3, 1), "no reductions here");
        assert_eq!(dropped, Decimal::new(6, 1));
        assert_eq!(skipped.iter().filter(|m| m.contains("turnover")).count(), 2);
    }

    /// The case that cost real money: the model abandons a name overnight and
    /// the book cannot follow because the day's allowance is already gone.
    #[test]
    fn a_name_the_model_abandoned_is_sold_even_with_the_allowance_exhausted() {
        let mut cfg = config();
        // No allowance at all: the harshest possible cap.
        cfg.turnover_budget = Decimal::ZERO;
        cfg.limits.min_position_notional = Decimal::ZERO;
        // KAITO's actual shape on 2026-08-14: held 4.54%, wanted 0.22%.
        let weights = BTreeMap::from([("KAITO".to_string(), Decimal::new(22, 4))]);
        let current = BTreeMap::from([("KAITO".to_string(), Decimal::new(454, 4))]);
        let prices = BTreeMap::from([("KAITO".to_string(), Decimal::new(41, 2))]);
        let adv = BTreeMap::from([("KAITO".to_string(), Some(Decimal::from(50_000_000)))]);
        let vol = BTreeMap::from([("KAITO".to_string(), Some(Decimal::from(1)))]);

        let (orders, _, _, used, dropped) = diff(
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
        assert_eq!(orders.len(), 1, "shedding risk is never deferred");
        assert_eq!(orders[0].reason, OrderReason::Reduce);
        assert_eq!(orders[0].side, Side::Sell);
        assert_eq!(used.spent, Decimal::ZERO, "it cost no allowance");
        assert_eq!(dropped, Decimal::ZERO);
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
        let out =
            generate_signal(&refs, &BTreeMap::new(), ts.date_naive(), &cfg, None, None).unwrap();
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
        let out =
            generate_signal(&refs, &BTreeMap::new(), ts.date_naive(), &cfg, None, None).unwrap();
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
    fn ranker_allocations_change_sleeves_without_changing_names_or_gross() {
        let mut cfg = config();
        cfg.constructor = "risk_adjusted_score_selected".into();
        cfg.target_gross_exposure = Decimal::new(8, 1);
        cfg.limits.max_position = Decimal::ONE;
        let signals: Vec<_> = (0..8)
            .map(|i| Signal {
                asset: format!("A{i}"),
                direction: if i < 4 {
                    Direction::Long
                } else {
                    Direction::Short
                },
                conviction: Decimal::new(if i < 4 { 4 } else { 2 }, 2),
                volatility: Some(Decimal::ONE),
            })
            .collect();
        let baseline = construct(&signals, &cfg, Decimal::from(100000), None).unwrap();
        for allocation in [
            config::RankerAllocation::Breadth,
            config::RankerAllocation::Timing,
            config::RankerAllocation::Confidence,
        ] {
            cfg.ranker_allocation = allocation;
            let out = construct_with_tilt(
                &signals,
                &cfg,
                Decimal::from(100000),
                None,
                Some(Decimal::ONE),
            )
            .unwrap();
            assert_eq!(
                out.weights.keys().collect::<Vec<_>>(),
                baseline.weights.keys().collect::<Vec<_>>()
            );
            let gross: Decimal = out.weights.values().map(|v| v.abs()).sum();
            let net: Decimal = out.weights.values().sum();
            assert!((gross - cfg.target_gross_exposure).abs() < Decimal::new(1, 24));
            assert!(net > Decimal::ZERO && net <= cfg.ranker_net_band);
            if allocation != config::RankerAllocation::Confidence {
                assert!((net - cfg.ranker_net_band).abs() < Decimal::new(1, 24));
                assert!(construct(&signals, &cfg, Decimal::from(100000), None).is_err());
            }
        }
    }

    #[test]
    fn capacity_selection_replaces_unreachable_high_scores_before_truncation() {
        let mut cfg = config();
        cfg.constructor = "risk_adjusted_score_selected".into();
        cfg.ranker_selection_count = 4;
        cfg.ranker_capacity_selection = true;
        cfg.limits.capacity_balance = true;
        cfg.limits.participation_limit = Decimal::new(1, 2);
        cfg.limits.max_position = Decimal::ONE;
        let signals: Vec<_> = (0..6)
            .map(|i| Signal {
                asset: format!("A{i}"),
                direction: if i % 2 == 0 {
                    Direction::Long
                } else {
                    Direction::Short
                },
                conviction: Decimal::new(8 - i, 2),
                volatility: Some(Decimal::ONE),
            })
            .collect();
        let profile = liquidity::Profile {
            assets: signals
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    (
                        s.asset.clone(),
                        liquidity::AssetLiquidity {
                            hourly_quote_volume: Some(if i < 2 {
                                Decimal::ZERO
                            } else {
                                Decimal::from(10000000)
                            }),
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            ..Default::default()
        };
        let out = construct(&signals, &cfg, Decimal::from(100000), Some(&profile)).unwrap();
        assert_eq!(
            out.weights.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["A2", "A3", "A4", "A5"]
        );
        assert!(out.weights.values().sum::<Decimal>().abs() < Decimal::new(1, 24));
    }

    #[test]
    fn per_side_selection_retains_a_two_sided_book_when_global_selection_cannot() {
        let mut cfg = config();
        cfg.constructor = "risk_adjusted_score_selected".into();
        cfg.ranker_selection_count = 4;
        cfg.limits.max_position = Decimal::ONE;
        let signals: Vec<_> = (0..6)
            .map(|i| Signal {
                asset: format!("A{i}"),
                direction: if i < 4 {
                    Direction::Long
                } else {
                    Direction::Short
                },
                conviction: Decimal::new(8 - i, 2),
                volatility: Some(Decimal::ONE),
            })
            .collect();
        assert!(construct(&signals, &cfg, Decimal::from(100000), None)
            .unwrap()
            .weights
            .is_empty());
        cfg.ranker_selection = config::RankerSelection::PerSide;
        let out = construct(&signals, &cfg, Decimal::from(100000), None).unwrap();
        assert_eq!(
            out.weights.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["A0", "A1", "A4", "A5"]
        );
        assert!(out.weights.values().sum::<Decimal>().abs() < Decimal::new(1, 24));
    }

    #[test]
    fn htf_conviction_is_bounded_directional_and_neutral_when_missing() {
        let edge = Decimal::new(1, 2);
        let weight = Decimal::new(25, 2);
        assert_eq!(
            htf_conviction(edge, Direction::Long, Some(0.2), Some(0.1), weight).unwrap(),
            Decimal::new(125, 4)
        );
        assert_eq!(
            htf_conviction(edge, Direction::Short, Some(0.2), Some(0.1), weight).unwrap(),
            Decimal::new(75, 4)
        );
        assert_eq!(
            htf_conviction(edge, Direction::Long, Some(0.2), Some(-0.1), weight).unwrap(),
            edge
        );
        assert_eq!(
            htf_conviction(edge, Direction::Long, None, Some(0.1), weight).unwrap(),
            edge
        );
        assert!(htf_conviction(edge, Direction::Long, Some(f64::NAN), Some(0.1), weight).is_err());
    }

    #[test]
    fn ranker_breadth_changes_membership_with_the_same_balanced_weights_and_caps() {
        let mut cfg = config();
        cfg.target_gross_exposure = Decimal::new(8, 1);
        cfg.limits.max_position = Decimal::new(2, 2);
        let mut signals: Vec<_> = (0..20)
            .flat_map(|i| {
                [Direction::Long, Direction::Short].map(|direction| Signal {
                    asset: format!(
                        "{}{i:02}",
                        if direction == Direction::Long {
                            "L"
                        } else {
                            "S"
                        }
                    ),
                    direction,
                    conviction: Decimal::new(40 - i, 3),
                    volatility: Some(Decimal::new(1, 1)),
                })
            })
            .collect();
        signals.push(Signal {
            asset: "BELOW_COST".into(),
            direction: Direction::Long,
            conviction: Decimal::new(1, 4),
            volatility: Some(Decimal::new(1, 5)),
        });
        signals.push(Signal {
            asset: "ZERO_VOL".into(),
            direction: Direction::Short,
            conviction: Decimal::ONE,
            volatility: Some(Decimal::ZERO),
        });
        for constructor in ["risk_adjusted", "risk_adjusted_score_selected"] {
            cfg.constructor = constructor.into();
            for count in [24, 40] {
                cfg.ranker_selection_count = count;
                let result = construct(&signals, &cfg, Decimal::from(100_000), None).unwrap();
                assert_eq!(result.weights.len(), count);
                assert_eq!(result.weights.contains_key("L19"), count == 40);
                assert!(!result.weights.contains_key("BELOW_COST"));
                assert!(!result.weights.contains_key("ZERO_VOL"));
                assert!(result
                    .weights
                    .values()
                    .all(|weight| weight.abs() <= cfg.limits.max_position));
                assert_eq!(result.weights["L00"], cfg.limits.max_position);
                let net: Decimal = result.weights.values().sum();
                assert!(net.abs() < Decimal::new(1, 12));
                let gross: Decimal = result.weights.values().map(|weight| weight.abs()).sum();
                assert!(gross <= cfg.target_gross_exposure);
                assert!(
                    (result.weights["L00"] * Decimal::from(39)
                        - result.weights["L01"] * Decimal::from(40))
                    .abs()
                        < Decimal::new(1, 12)
                );
                assert_eq!(result.weights["L00"], -result.weights["S00"]);
            }
        }
        cfg.constructor = "risk_adjusted_unbalanced".into();
        let unaffected = construct(&signals, &cfg, Decimal::from(100_000), None).unwrap();
        assert_eq!(unaffected.weights.len(), 24);
    }

    #[test]
    fn score_selection_keeps_stronger_per_risk_candidates_and_existing_caps() {
        let mut cfg = config();
        cfg.constructor = "risk_adjusted".into();
        cfg.target_gross_exposure = Decimal::new(8, 1);
        cfg.limits.max_position = Decimal::new(3, 2);
        let mut signals: Vec<_> = (0..23)
            .map(|i| Signal {
                asset: format!("ANCHOR{i:02}"),
                direction: if i % 2 == 0 {
                    Direction::Long
                } else {
                    Direction::Short
                },
                conviction: Decimal::new(4, 2),
                volatility: Some(Decimal::new(1, 1)),
            })
            .collect();
        signals.extend([
            Signal {
                asset: "HIGH_VOL".into(),
                direction: Direction::Long,
                conviction: Decimal::new(3, 2),
                volatility: Some(Decimal::new(3, 1)),
            },
            Signal {
                asset: "HIGH_SCORE".into(),
                direction: Direction::Long,
                conviction: Decimal::new(2, 2),
                volatility: Some(Decimal::new(1, 1)),
            },
            Signal {
                asset: "BELOW_COST".into(),
                direction: Direction::Long,
                conviction: Decimal::new(1, 4),
                volatility: Some(Decimal::new(1, 5)),
            },
            Signal {
                asset: "ZERO_VOL".into(),
                direction: Direction::Short,
                conviction: Decimal::ONE,
                volatility: Some(Decimal::ZERO),
            },
        ]);
        let original = construct(&signals, &cfg, Decimal::from(100_000), None).unwrap();
        assert!(original.weights.contains_key("HIGH_VOL"));
        assert!(!original.weights.contains_key("HIGH_SCORE"));
        cfg.constructor = "risk_adjusted_score_selected".into();
        let selected = construct(&signals, &cfg, Decimal::from(100_000), None).unwrap();
        assert!(selected.weights.contains_key("HIGH_SCORE"));
        assert!(!selected.weights.contains_key("HIGH_VOL"));
        assert!(!selected.weights.contains_key("BELOW_COST"));
        assert!(!selected.weights.contains_key("ZERO_VOL"));
        assert_eq!(selected.weights.len(), 24);
        for result in [&original, &selected] {
            assert!(result
                .weights
                .values()
                .all(|weight| weight.abs() <= cfg.limits.max_position));
            let net: Decimal = result.weights.values().sum();
            assert!(net.abs() < Decimal::new(1, 12));
            let gross: Decimal = result.weights.values().map(|weight| weight.abs()).sum();
            assert!(gross <= cfg.target_gross_exposure);
            assert!(result
                .notes
                .iter()
                .any(|note| note.contains("whole book scaled")));
        }
        cfg.constructor = "risk_adjusted_unbalanced".into();
        let old_joint = construct(&signals, &cfg, Decimal::from(100_000), None).unwrap();
        assert!(old_joint.weights.contains_key("HIGH_VOL"));
        assert!(!old_joint.weights.contains_key("HIGH_SCORE"));
        cfg.constructor = "risk_adjusted_score_unbalanced".into();
        let joint = construct(&signals, &cfg, Decimal::from(100_000), None).unwrap();
        assert_eq!(joint.weights.len(), 24);
        assert!(joint.weights.contains_key("HIGH_SCORE"));
        for excluded in ["HIGH_VOL", "BELOW_COST", "ZERO_VOL"] {
            assert!(!joint.weights.contains_key(excluded));
        }
        assert!(joint
            .weights
            .values()
            .all(|w| w.abs() <= cfg.limits.max_position));
        assert!(joint
            .notes
            .iter()
            .any(|note| note.contains("whole book scaled")));
        assert!(
            (joint.weights["ANCHOR00"] - joint.weights["HIGH_SCORE"] * Decimal::from(2)).abs()
                < Decimal::new(1, 12)
        );
        let mut tie = signals
            .iter()
            .find(|s| s.asset == "HIGH_SCORE")
            .unwrap()
            .clone();
        tie.asset = "AAA_TIE".into();
        signals.push(tie);
        let tied = construct(&signals, &cfg, Decimal::from(100_000), None).unwrap();
        assert!(tied.weights.contains_key("AAA_TIE"));
        assert!(!tied.weights.contains_key("HIGH_SCORE"));
        signals.reverse();
        assert_eq!(
            tied.weights,
            construct(&signals, &cfg, Decimal::from(100_000), None)
                .unwrap()
                .weights
        );
    }

    #[test]
    fn score_selection_still_requires_two_names_per_side_after_top_24() {
        let mut cfg = config();
        cfg.constructor = "risk_adjusted_score_selected".into();
        let mut signals: Vec<_> = (0..24)
            .map(|i| Signal {
                asset: format!("L{i:02}"),
                direction: Direction::Long,
                conviction: Decimal::new(2, 2),
                volatility: Some(Decimal::new(1, 2)),
            })
            .collect();
        signals.extend((0..2).map(|i| Signal {
            asset: format!("S{i:02}"),
            direction: Direction::Short,
            conviction: Decimal::new(3, 2),
            volatility: Some(Decimal::new(1, 1)),
        }));
        let selected = construct(&signals, &cfg, Decimal::from(100_000), None).unwrap();
        assert!(selected.weights.is_empty());
        assert!(selected
            .notes
            .iter()
            .any(|note| note.contains("top-24 book has 24 long and 0 short")));
        cfg.constructor = "risk_adjusted".into();
        let original = construct(&signals, &cfg, Decimal::from(100_000), None).unwrap();
        assert_eq!(original.weights.len(), 24);
        assert_eq!(
            original
                .weights
                .values()
                .filter(|value| **value < Decimal::ZERO)
                .count(),
            2
        );
    }

    /// 3 longs / 1 short of equal edge and vol: `risk_adjusted` gives the lone
    /// short 0.40 and each long 0.133; `risk_adjusted_unbalanced` gives every
    /// name 0.20 and the book is 0.60 long / 0.20 short - the ranked list, not
    /// a manufactured balance. (Two shorts are needed for the balanced one to
    /// form at all, so it gets a second short of tiny edge.)
    #[test]
    fn unbalanced_constructor_sizes_the_ranked_list_across_sides() {
        let mut cfg = config();
        cfg.constructor = "risk_adjusted_unbalanced".into();
        cfg.target_gross_exposure = Decimal::new(8, 1);
        cfg.limits.max_position = Decimal::new(25, 2);
        let mk = |a: &str, dir: Direction, conv: Decimal| Signal {
            asset: a.into(),
            direction: dir,
            conviction: conv,
            volatility: Some(Decimal::new(1, 1)),
        };
        let signals = vec![
            mk("L1", Direction::Long, Decimal::new(4, 2)),
            mk("L2", Direction::Long, Decimal::new(4, 2)),
            mk("L3", Direction::Long, Decimal::new(4, 2)),
            mk("S1", Direction::Short, Decimal::new(4, 2)),
        ];
        let out = construct(&signals, &cfg, Decimal::from(100_000), None).unwrap();
        assert_eq!(out.weights.len(), 4);
        for w in out.weights.values() {
            assert_eq!(w.abs(), Decimal::new(2, 1), "{:?}", out.weights);
        }
        let net: Decimal = out.weights.values().sum();
        assert_eq!(net, Decimal::new(4, 1));
        assert!(
            out.notes
                .iter()
                .any(|n| n.contains("3 long") && n.contains("1 short")),
            "{:?}",
            out.notes
        );

        let mut score_cfg = cfg.clone();
        score_cfg.constructor = "risk_adjusted_score_unbalanced".into();
        let score = construct(&signals, &score_cfg, Decimal::from(100_000), None).unwrap();
        assert_eq!(out.weights, score.weights);
        let mut one_side = signals.clone();
        for signal in &mut one_side {
            signal.direction = Direction::Long;
        }
        let all_long = construct(&one_side, &score_cfg, Decimal::from(100_000), None).unwrap();
        assert_eq!(
            all_long.weights.values().copied().sum::<Decimal>(),
            Decimal::new(8, 1)
        );
        assert!(all_long.weights.values().all(|w| *w == Decimal::new(2, 1)));
        for signal in &mut one_side {
            signal.direction = Direction::Short;
        }
        let all_short = construct(&one_side, &score_cfg, Decimal::from(100_000), None).unwrap();
        assert_eq!(
            all_short.weights.values().copied().sum::<Decimal>(),
            Decimal::new(-8, 1)
        );

        // The balanced constructor on the same list plus a second, weak short:
        // the short side still gets 0.40 between two names.
        let mut balanced = cfg.clone();
        balanced.constructor = "risk_adjusted".into();
        let mut two_shorts = signals.clone();
        two_shorts.push(mk("S2", Direction::Short, Decimal::new(1, 2)));
        let out_b = construct(&two_shorts, &balanced, Decimal::from(100_000), None).unwrap();
        let short_side: Decimal = out_b
            .weights
            .values()
            .filter(|v| **v < Decimal::ZERO)
            .map(|v| v.abs())
            .sum();
        let long_side: Decimal = out_b.weights.values().filter(|v| **v > Decimal::ZERO).sum();
        // Equal sides by construction (both 0.40, then scaled together when
        // the 0.25 per-position cap binds on the concentrated short) - two
        // weak shorts carry as much of the book as three strong longs.
        assert!(
            (short_side - long_side).abs() < Decimal::new(1, 6),
            "{:?}",
            out_b.weights
        );
        let biggest_short = out_b
            .weights
            .values()
            .filter(|v| **v < Decimal::ZERO)
            .map(|v| v.abs())
            .max()
            .unwrap();
        let biggest_long = out_b
            .weights
            .values()
            .filter(|v| **v > Decimal::ZERO)
            .copied()
            .max()
            .unwrap();
        assert!(biggest_short > biggest_long, "{:?}", out_b.weights);
    }

    /// Trend overlay: breadth -1 -> N = -0.30 -> sleeves 0.25 long / 0.55
    /// short on 0.80 gross; breadth 0 -> the frozen 0.40/0.40; the ranker's
    /// selection is identical either way. Without a tilt the constructor
    /// refuses rather than assuming flat.
    #[test]
    fn tilted_constructor_sets_sleeves_from_the_breadth_trend() {
        let mut cfg = config();
        cfg.constructor = "risk_adjusted_tilted".into();
        cfg.target_gross_exposure = Decimal::new(8, 1);
        // Wide per-name cap so the sleeve arithmetic is visible unscaled.
        cfg.limits.max_position = Decimal::new(5, 1);
        let mk = |a: &str, dir: Direction, conv: Decimal| Signal {
            asset: a.into(),
            direction: dir,
            conviction: conv,
            volatility: Some(Decimal::new(1, 1)),
        };
        let signals = vec![
            mk("L1", Direction::Long, Decimal::new(4, 2)),
            mk("L2", Direction::Long, Decimal::new(2, 2)),
            mk("S1", Direction::Short, Decimal::new(4, 2)),
            mk("S2", Direction::Short, Decimal::new(2, 2)),
        ];
        let out = construct_with_tilt(
            &signals,
            &cfg,
            Decimal::from(100_000),
            None,
            Some(-Decimal::ONE),
        )
        .unwrap();
        let long: Decimal = out.weights.values().filter(|v| **v > Decimal::ZERO).sum();
        let short: Decimal = out
            .weights
            .values()
            .filter(|v| **v < Decimal::ZERO)
            .map(|v| v.abs())
            .sum();
        assert_eq!(long, Decimal::new(25, 2), "{:?}", out.weights);
        assert_eq!(short, Decimal::new(55, 2), "{:?}", out.weights);
        // within the short side, 2:1 by edge/vol
        assert_eq!(
            out.weights["S1"],
            -Decimal::new(55, 2) * Decimal::from(2) / Decimal::from(3)
        );
        let flat = construct_with_tilt(
            &signals,
            &cfg,
            Decimal::from(100_000),
            None,
            Some(Decimal::ZERO),
        )
        .unwrap();
        let long0: Decimal = flat.weights.values().filter(|v| **v > Decimal::ZERO).sum();
        assert_eq!(long0, Decimal::new(4, 1));
        assert!(construct_with_tilt(&signals, &cfg, Decimal::from(100_000), None, None).is_err());
    }

    #[test]
    fn selection_hysteresis_keeps_agreeing_incumbents_and_fills_only_vacated_slots() {
        let mut cfg = config();
        cfg.signal = "ml_ranker".into();
        cfg.constructor = "risk_adjusted_score_selected".into();
        cfg.ranker_selection_count = 4;
        cfg.ranker_hold_rank_multiple = Some(Decimal::from(2));
        cfg.limits.max_position = Decimal::new(5, 1);
        let mk = |a: &str, dir: Direction, conv: i64| Signal {
            asset: a.into(),
            direction: dir,
            conviction: Decimal::new(conv, 3),
            volatility: Some(Decimal::new(1, 1)),
        };
        // Best-first order: L1 90, S1 80, L2 70, S2 60, L3 50, S3 40, L4 30, S4 20.
        let signals = vec![
            mk("L1", Direction::Long, 90),
            mk("S1", Direction::Short, 80),
            mk("L2", Direction::Long, 70),
            mk("S2", Direction::Short, 60),
            mk("L3", Direction::Long, 50),
            mk("S3", Direction::Short, 40),
            mk("L4", Direction::Long, 30),
            mk("S4", Direction::Short, 20),
        ];
        let nav = Decimal::from(100_000);
        // No book: plain top-4.
        let fresh = construct_with_tilt_held(&signals, &cfg, nav, None, None, &BTreeMap::new())
            .unwrap();
        let names = |c: &Construction| c.weights.keys().cloned().collect::<Vec<_>>();
        assert_eq!(names(&fresh), ["L1", "L2", "S1", "S2"]);
        // Held S3 (rank 6, inside 4 x 2 = 8) stays and displaces S2; held L4
        // (rank 7) is held on the wrong side, so it is not an incumbent.
        let mut held = BTreeMap::new();
        held.insert("S3".to_string(), -Decimal::new(1, 1));
        held.insert("L4".to_string(), -Decimal::new(1, 1));
        let kept = construct_with_tilt_held(&signals, &cfg, nav, None, None, &held).unwrap();
        assert_eq!(names(&kept), ["L1", "L2", "S1", "S3"], "{:?}", kept.notes);
        assert!(kept.notes.iter().any(|n| n.contains("kept 1 incumbent")));
        // Outside the window (rank 9 of 8 would be S4 at rank 8: inside), so
        // widen the check: with multiple 1 the window is exactly the top 4 and
        // S3 falls out.
        cfg.ranker_hold_rank_multiple = Some(Decimal::ONE);
        let tight = construct_with_tilt_held(&signals, &cfg, nav, None, None, &held).unwrap();
        assert_eq!(names(&tight), ["L1", "L2", "S1", "S2"]);
        // The wrapper without a book is unchanged.
        let plain = construct_with_tilt(&signals, &cfg, nav, None, None).unwrap();
        assert_eq!(names(&plain), names(&fresh));
    }

    #[test]
    fn tranches_let_only_one_sleeve_change_and_carry_pending_exits() {
        let mut construction = Construction {
            weights: BTreeMap::new(),
            constructor: "risk_adjusted_score_selected".into(),
            notes: Vec::new(),
        };
        let mut current = BTreeMap::new();
        // These symbols hash into sleeves 1, 0, 1, 1, 2, 0 under the
        // stable 31-multiplier bucket, so every sleeve is represented.
        let names = ["BTC", "ETH", "SOL", "XRP", "DOGE", "ADA"];
        for (i, name) in names.iter().enumerate() {
            // Target every name at +0.10; currently hold every name at -0.05
            // except the last, which the constructor wants to drop.
            construction
                .weights
                .insert(name.to_string(), Decimal::new(10, 2));
            if i < names.len() - 1 {
                current.insert(name.to_string(), -Decimal::new(5, 2));
            }
        }
        construction.weights.remove("DOGE");
        current.insert("DOGE".to_string(), -Decimal::new(5, 2));
        let as_of: DateTime<Utc> = "2026-08-01T00:00:00Z".parse().unwrap();
        let tranches = 3;
        let mut cfg = config();
        cfg.ranker_tranches = tranches;
        cfg.limits.max_position_count = 40;
        cfg.limits.max_net_exposure = Some(Decimal::new(1, 1));
        let notes = apply_tranches(&mut construction, &current, &cfg, as_of);
        assert!(!notes.is_empty(), "{notes:?}");
        let today = (as_of.timestamp() / 86_400).rem_euclid(tranches as i64) as usize;
        let sleeve = |asset: &str| {
            asset
                .bytes()
                .fold(0usize, |acc, b| (acc * 31 + b as usize) % tranches)
        };
        let mut changed = 0;
        for name in names {
            let target = construction.weights.get(name).copied();
            if sleeve(name) == today {
                changed += 1;
                // In-sleeve names take the constructor's word, including a drop.
                if name == "DOGE" {
                    assert_eq!(target, None);
                } else {
                    assert_eq!(target, Some(Decimal::new(10, 2)));
                }
            } else {
                // Out-of-sleeve names keep exactly their current weight; a
                // name not held and not in the sleeve is absent.
                assert_eq!(target, current.get(name).copied(), "{name}");
            }
        }
        assert!(changed >= 1 && changed < names.len());
        // The next day rotates to another sleeve.
        let mut again = Construction {
            weights: BTreeMap::new(),
            constructor: "risk_adjusted_score_selected".into(),
            notes: Vec::new(),
        };
        let tomorrow = as_of + chrono::Duration::days(1);
        let notes = apply_tranches(&mut again, &current, &cfg, tomorrow);
        assert!(notes[0].starts_with(&format!("tranche {} of 3", (today + 1) % 3 + 1)));
        assert!(again.weights.len() < current.len());
    }

    #[test]
    fn breadth_trend_is_the_mean_of_two_horizons_breadths() {
        let as_of: DateTime<Utc> = "2026-08-01T00:00:00Z".parse().unwrap();
        let mut rows: Vec<DailyRow> = (0..4)
            .map(|i| row(&format!("A{i}"), as_of, 100.0))
            .collect();
        // ret_30: 3 of 4 up -> b30 = 0.5 ; ret_90: 1 of 4 up -> b90 = -0.5 ; trend 0
        for (i, r) in rows.iter_mut().enumerate() {
            r.ret_30 = Some(if i < 3 { 0.1 } else { -0.1 });
            r.ret_90 = Some(if i < 1 { 0.1 } else { -0.1 });
        }
        let refs: Vec<&DailyRow> = rows.iter().collect();
        assert_eq!(
            breadth_trend(&refs, &config::Overlay::default()),
            Some(Decimal::ZERO)
        );
        for r in rows.iter_mut() {
            r.ret_90 = Some(-0.2);
        }
        let refs: Vec<&DailyRow> = rows.iter().collect();
        assert_eq!(
            breadth_trend(&refs, &config::Overlay::default()),
            Some(Decimal::new(-25, 2))
        ); // (0.5 + -1)/2
        for r in rows.iter_mut() {
            r.ret_90 = None;
        }
        let refs: Vec<&DailyRow> = rows.iter().collect();
        assert_eq!(breadth_trend(&refs, &config::Overlay::default()), None);
    }

    /// crypto-directional round 1: mixed-sign trend book. Two coins in a
    /// full uptrend (s=+1), one in a full downtrend (s=-1), one with mixed
    /// horizons (s=+1/3), sized 1/sigma to an ex-ante vol budget of 0.25.
    #[test]
    fn tsmom_vol_parity_sizes_signed_trends_to_the_vol_budget() {
        let mut cfg = config();
        cfg.constructor = "tsmom_vol_parity".into();
        cfg.target_gross_exposure = Decimal::ONE;
        // Wide cap so the budget arithmetic is visible unscaled; the cap
        // path is exercised by the shared whole-book-scale code elsewhere.
        cfg.limits.max_position = Decimal::new(5, 1);
        let mk = |a: &str, dir: Direction, conv: &str, vol: &str| Signal {
            asset: a.into(),
            direction: dir,
            conviction: conv.parse().unwrap(),
            volatility: Some(vol.parse().unwrap()),
        };
        let signals = vec![
            mk("UP1", Direction::Long, "1", "0.5"),
            mk("UP2", Direction::Long, "1", "1.0"),
            mk("DN1", Direction::Short, "1", "0.5"),
            mk("MIX", Direction::Long, "0.3333", "0.5"),
        ];
        let out = construct(&signals, &cfg, Decimal::from(100_000), None).unwrap();
        // sum conviction = 3.3333 -> k = 0.25/3.3333 = 0.075;
        // |w|*sigma per name = k*conviction: 0.075, 0.075, 0.075, 0.025 -> sums to 0.25.
        let vols = BTreeMap::from([("UP1", 0.5), ("UP2", 1.0), ("DN1", 0.5), ("MIX", 0.5)]);
        let exante: f64 = out
            .weights
            .iter()
            .map(|(a, w)| {
                let wf: f64 = w.abs().to_string().parse().unwrap();
                wf * vols[a.as_str()]
            })
            .sum();
        assert!((exante - 0.25).abs() < 1e-6, "{exante} {:?}", out.weights);
        assert!(out.weights["UP1"] > Decimal::ZERO && out.weights["DN1"] < Decimal::ZERO);
        assert_eq!(out.weights["UP1"], -out.weights["DN1"]);
        assert_eq!(
            out.weights["UP1"],
            out.weights["UP2"] * Decimal::from(2),
            "half the vol, twice the weight"
        );
        assert!(
            out.weights["MIX"] < out.weights["UP1"],
            "weaker trend, smaller weight"
        );
        // A one-sided book is legal: all-long input stays all-long.
        let all_long: Vec<Signal> = signals
            .iter()
            .cloned()
            .map(|mut s| {
                s.direction = Direction::Long;
                s
            })
            .collect();
        let out2 = construct(&all_long, &cfg, Decimal::from(100_000), None).unwrap();
        assert!(out2.weights.values().all(|w| *w > Decimal::ZERO));
    }

    #[test]
    fn rust_plan_round_trips_through_the_executor_contract() {
        let mut cfg = config();
        cfg.limits.max_cluster_exposure = None;
        // A two-name long-only fixture is net-long by construction; this test
        // is about the plan's shape surviving the executor contract, not the
        // book's neutrality, so the net limit is lifted the same way the
        // cluster limit is.
        cfg.limits.max_net_exposure = None;
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
            native_funding: None,
            sizing_liquidity: None,
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
        // Every non-tilted plan records what the shadow overlay would have
        // asked for - the live-evidence line the trend-overlay study left
        // behind. Two rising fixtures: breadth +1, net target = band.
        assert!(
            out.plan
                .warnings
                .iter()
                .any(|w| w.message.starts_with("shadow overlay: breadth trend")),
            "{:?}",
            out.plan.warnings
        );
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
            native_funding: None,
            sizing_liquidity: None,
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
