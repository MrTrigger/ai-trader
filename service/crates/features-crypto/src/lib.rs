//! The sole implementation of crypto features used by training and trading.
//!
//! Python may fit a model, but it must consume rows emitted by this crate. It
//! may not recreate, normalise, impute, rank, or otherwise transform model
//! inputs. Live inference calls these same functions, so train/runtime feature
//! parity is structural rather than a convention maintained in two languages.
//!
//! The implementation is deliberately incremental. [`daily`] and [`hourly`]
//! accept historical batches, but each asset is evaluated in timestamp order
//! with trailing state only. Appending future bars therefore cannot change a
//! previously emitted row; tests assert that property directly.

use std::collections::{BTreeMap, VecDeque};

use chrono::{DateTime, NaiveDate, Timelike, Utc};
use serde::{Deserialize, Serialize};

pub const FEATURE_SET_VERSION: &str = "fs-rust-crypto-4";
pub const DAILY_FEATURE_NAMES: &[&str] = &[
    "ret_7",
    "ret_30",
    "ret_90",
    "ret_30_skip_7",
    "vol_30",
    "adv_quote",
    "beta_bench",
    "gc_regime_slope",
    // The slow daily block, restored. The migration carried over only eight
    // daily features while the research record's strongest signals were slow
    // daily ones; fold-level IC halved (0.064 -> 0.032). These are the standard
    // cross-sectional factors that block covered. Levels are fine everywhere a
    // ratio might seem needed: inputs are rank-normalised within each date, so
    // any monotone transform of a feature scores identically.
    "ret_180",
    "vol_7",
    "vol_90",
    "skew_30",
    "semivol_30",
    "dist_high_90",
    "dist_low_90",
    "amihud_30",
    "turnover_20",
    "range_frac_14",
    // The one missing feature the record actually names: IC -0.124 in the
    // Phase 1 screen, second only to vol_30. Its inputs were computed here all
    // along and then dropped on the floor.
    "band_width",
    // The rest of the recovered harness's daily block, ported from the
    // reconstructed dataset builder (var/research/recovered/dataset.py).
    // Channel geometry the EMA cascade already pays for:
    "dist_upper",
    "dist_lower",
    "breakout_age",
    "vol_ratio",
    // The fast-daily block - refresh every day, several screened better at the
    // 1-day horizon than anything slow:
    "f_gap",
    "f_range",
    "f_clv",
    "f_volsurp",
    "f_trsurp",
    "f_amihud",
    "f_ret2",
    "f_ret3",
    "f_accel",
    "f_dvol",
    // Funding, STRICTLY TRAILING. The original harness summed these windows
    // FORWARD - realised funding from days that had not happened yet - and
    // that one character (+k for -k) was the whole difference between the
    // recorded Sharpe 2.70 and the honest 1.05. The replay proves it:
    // flip the window and the backtest collapses from +760% to +135%.
    "funding_7d",
    "funding_30d",
    "funding_chg",
    "funding_z",
];
pub const HOURLY_FEATURE_NAMES: &[&str] = &[
    "rv_6h",
    "rv_24h",
    "rv_72h",
    "rv_168h",
    "ret_6h",
    "ret_24h",
    "ret_72h",
    "ret_168h",
    "eff_6h",
    "eff_24h",
    "eff_72h",
    "eff_168h",
    "jump_6h",
    "jump_24h",
    "jump_72h",
    "jump_168h",
    "dv_6h",
    "dv_24h",
    "dv_72h",
    "dv_168h",
    "semi_dn",
    "semi_up",
    "semi_ratio",
    "skew_24h",
    "trade_size_surp",
    "trades_24h",
    "dd_168h",
    "vol_concentration",
    "rel_ret_24h",
    "rel_vol_24h",
];

const VOL_WINDOW: usize = 30;
const ADV_WINDOW: usize = 20;
const BETA_WINDOW: usize = 90;
const GC_PERIOD: usize = 144;
const GC_POLES: usize = 4;
const GC_MULTIPLIER: f64 = 1.414;
const GC_REGIME_PERIOD: usize = 48;
const GC_REGIME_SLOPE_BARS: usize = 20;
const MAX_BAR_LOG_RETURN: f64 = std::f64::consts::LN_10;

/// Canonical completed bar. `ts_utc` is the bar OPEN time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bar {
    pub ts_utc: DateTime<Utc>,
    pub asset: String,
    pub interval_s: i32,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    #[serde(default)]
    pub quote_volume: Option<f64>,
    #[serde(default)]
    pub trades: Option<i64>,
}

impl Bar {
    pub fn validate(&self) -> Result<(), String> {
        if self.asset.is_empty()
            || self.asset.len() > 20
            || !self
                .asset
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        {
            return Err(format!("non-canonical asset {:?}", self.asset));
        }
        if self.interval_s <= 0 {
            return Err(format!("{} has non-positive interval", self.asset));
        }
        if ![self.open, self.high, self.low, self.close, self.volume]
            .iter()
            .all(|v| v.is_finite())
        {
            return Err(format!("{} has a non-finite required value", self.asset));
        }
        if self.open <= 0.0 || self.high <= 0.0 || self.low <= 0.0 || self.close <= 0.0 {
            return Err(format!("{} has a non-positive price", self.asset));
        }
        if self.volume < 0.0 {
            return Err(format!("{} has negative volume", self.asset));
        }
        if self.high < self.low
            || self.open > self.high
            || self.open < self.low
            || self.close > self.high
            || self.close < self.low
        {
            return Err(format!("{} has inconsistent OHLC", self.asset));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DailyRow {
    pub ts_utc: DateTime<Utc>,
    pub asset: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub mark_open: f64,
    pub mark_close: f64,
    pub had_discontinuity: bool,
    pub bars_available: u32,
    pub perp_listed: bool,
    pub ret_1: Option<f64>,
    pub ret_7: Option<f64>,
    pub ret_30: Option<f64>,
    pub ret_90: Option<f64>,
    pub ret_30_skip_7: Option<f64>,
    pub vol_30: Option<f64>,
    pub adv_quote: Option<f64>,
    pub ret_180: Option<f64>,
    pub vol_7: Option<f64>,
    pub vol_90: Option<f64>,
    pub skew_30: Option<f64>,
    pub semivol_30: Option<f64>,
    pub dist_high_90: Option<f64>,
    pub dist_low_90: Option<f64>,
    pub amihud_30: Option<f64>,
    pub turnover_20: Option<f64>,
    pub range_frac_14: Option<f64>,
    pub band_width: Option<f64>,
    pub dist_upper: Option<f64>,
    pub dist_lower: Option<f64>,
    pub breakout_age_f: Option<f64>,
    pub vol_ratio: Option<f64>,
    pub f_gap: Option<f64>,
    pub f_range: Option<f64>,
    pub f_clv: Option<f64>,
    pub f_volsurp: Option<f64>,
    pub f_trsurp: Option<f64>,
    pub f_amihud: Option<f64>,
    pub f_ret2: Option<f64>,
    pub f_ret3: Option<f64>,
    pub f_accel: Option<f64>,
    pub f_dvol: Option<f64>,
    pub funding_7d: Option<f64>,
    pub funding_30d: Option<f64>,
    pub funding_chg: Option<f64>,
    pub funding_z: Option<f64>,
    pub beta_bench: Option<f64>,
    pub gc_filter: Option<f64>,
    pub gc_upper: Option<f64>,
    pub gc_lower: Option<f64>,
    pub gc_breakout_age: Option<u32>,
    pub gc_regime_filter: Option<f64>,
    pub gc_regime_upper: Option<f64>,
    pub gc_regime_slope: Option<f64>,
}

impl DailyRow {
    pub fn value(&self, name: &str) -> Option<f64> {
        match name.strip_prefix("x_").unwrap_or(name) {
            "ret_7" => self.ret_7,
            "ret_30" => self.ret_30,
            "ret_90" => self.ret_90,
            "ret_30_skip_7" => self.ret_30_skip_7,
            "vol_30" => self.vol_30,
            "adv_quote" => self.adv_quote,
            "ret_180" => self.ret_180,
            "vol_7" => self.vol_7,
            "vol_90" => self.vol_90,
            "skew_30" => self.skew_30,
            "semivol_30" => self.semivol_30,
            "dist_high_90" => self.dist_high_90,
            "dist_low_90" => self.dist_low_90,
            "amihud_30" => self.amihud_30,
            "turnover_20" => self.turnover_20,
            "range_frac_14" => self.range_frac_14,
            "band_width" => self.band_width,
            "dist_upper" => self.dist_upper,
            "dist_lower" => self.dist_lower,
            "breakout_age" => self.breakout_age_f,
            "vol_ratio" => self.vol_ratio,
            "f_gap" => self.f_gap,
            "f_range" => self.f_range,
            "f_clv" => self.f_clv,
            "f_volsurp" => self.f_volsurp,
            "f_trsurp" => self.f_trsurp,
            "f_amihud" => self.f_amihud,
            "f_ret2" => self.f_ret2,
            "f_ret3" => self.f_ret3,
            "f_accel" => self.f_accel,
            "f_dvol" => self.f_dvol,
            "funding_7d" => self.funding_7d,
            "funding_30d" => self.funding_30d,
            "funding_chg" => self.funding_chg,
            "funding_z" => self.funding_z,
            "beta_bench" => self.beta_bench,
            "gc_regime_slope" => self.gc_regime_slope,
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HourlyRow {
    pub ts_utc: DateTime<Utc>,
    pub asset: String,
    pub rv_6h: Option<f64>,
    pub rv_24h: Option<f64>,
    pub rv_72h: Option<f64>,
    pub rv_168h: Option<f64>,
    pub ret_6h: Option<f64>,
    pub ret_24h: Option<f64>,
    pub ret_72h: Option<f64>,
    pub ret_168h: Option<f64>,
    pub eff_6h: Option<f64>,
    pub eff_24h: Option<f64>,
    pub eff_72h: Option<f64>,
    pub eff_168h: Option<f64>,
    pub jump_6h: Option<f64>,
    pub jump_24h: Option<f64>,
    pub jump_72h: Option<f64>,
    pub jump_168h: Option<f64>,
    pub dv_6h: Option<f64>,
    pub dv_24h: Option<f64>,
    pub dv_72h: Option<f64>,
    pub dv_168h: Option<f64>,
    pub semi_dn: Option<f64>,
    pub semi_up: Option<f64>,
    pub semi_ratio: Option<f64>,
    pub skew_24h: Option<f64>,
    pub trade_size_surp: Option<f64>,
    pub trades_24h: Option<f64>,
    pub dd_168h: Option<f64>,
    pub vol_concentration: Option<f64>,
    pub rel_ret_24h: Option<f64>,
    pub rel_vol_24h: Option<f64>,
}

impl HourlyRow {
    pub fn value(&self, name: &str) -> Option<f64> {
        match name.strip_prefix("x_").unwrap_or(name) {
            "rv_6h" => self.rv_6h,
            "rv_24h" => self.rv_24h,
            "rv_72h" => self.rv_72h,
            "rv_168h" => self.rv_168h,
            "ret_6h" => self.ret_6h,
            "ret_24h" => self.ret_24h,
            "ret_72h" => self.ret_72h,
            "ret_168h" => self.ret_168h,
            "eff_6h" => self.eff_6h,
            "eff_24h" => self.eff_24h,
            "eff_72h" => self.eff_72h,
            "eff_168h" => self.eff_168h,
            "jump_6h" => self.jump_6h,
            "jump_24h" => self.jump_24h,
            "jump_72h" => self.jump_72h,
            "jump_168h" => self.jump_168h,
            "dv_6h" => self.dv_6h,
            "dv_24h" => self.dv_24h,
            "dv_72h" => self.dv_72h,
            "dv_168h" => self.dv_168h,
            "semi_dn" => self.semi_dn,
            "semi_up" => self.semi_up,
            "semi_ratio" => self.semi_ratio,
            "skew_24h" => self.skew_24h,
            "trade_size_surp" => self.trade_size_surp,
            "trades_24h" => self.trades_24h,
            "dd_168h" => self.dd_168h,
            "vol_concentration" => self.vol_concentration,
            "rel_ret_24h" => self.rel_ret_24h,
            "rel_vol_24h" => self.rel_vol_24h,
            _ => None,
        }
    }
}

/// Rank-normalise a point-in-time cross-section exactly as model training and
/// inference require: average ascending rank mapped to [-1, 1], null to zero.
/// This preprocessing is part of feature semantics and therefore lives here,
/// never in a Python trainer.
pub use features_common::rank_normalise;

fn checked_grouped(
    bars: &[Bar],
    expected_interval: i32,
) -> Result<BTreeMap<String, Vec<&Bar>>, String> {
    let mut out: BTreeMap<String, Vec<&Bar>> = BTreeMap::new();
    for bar in bars {
        bar.validate()?;
        if bar.interval_s != expected_interval {
            return Err(format!(
                "{} at {} has interval {}, expected {}",
                bar.asset, bar.ts_utc, bar.interval_s, expected_interval
            ));
        }
        out.entry(bar.asset.clone()).or_default().push(bar);
    }
    for (asset, rows) in &mut out {
        rows.sort_by_key(|b| b.ts_utc);
        for pair in rows.windows(2) {
            if pair[0].ts_utc == pair[1].ts_utc {
                return Err(format!("duplicate bar for {asset} at {}", pair[0].ts_utc));
            }
        }
    }
    Ok(out)
}

fn shifted_return(closes: &[f64], i: usize, n: usize) -> Option<f64> {
    i.checked_sub(n).map(|j| closes[i] / closes[j] - 1.0)
}

fn finite(v: f64) -> Option<f64> {
    v.is_finite().then_some(v)
}

/// Sample skewness (g1). None below three points or under zero variance.
fn sample_skew(values: &[f64]) -> Option<f64> {
    let n = values.len();
    if n < 3 {
        return None;
    }
    let mean = values.iter().sum::<f64>() / n as f64;
    let m2 = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n as f64;
    let m3 = values.iter().map(|v| (v - mean).powi(3)).sum::<f64>() / n as f64;
    if m2 <= 0.0 {
        return None;
    }
    finite(m3 / m2.powf(1.5))
}

/// Downside deviation: dispersion of the losing days only, against zero.
fn downside_std(values: &[f64]) -> Option<f64> {
    let n = values.len();
    if n < 2 {
        return None;
    }
    let sum: f64 = values.iter().map(|v| v.min(0.0).powi(2)).sum();
    finite((sum / (n - 1) as f64).sqrt())
}

fn sample_std(values: &[f64]) -> Option<f64> {
    if values.len() < 2 || values.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let ss = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>();
    finite((ss / (values.len() - 1) as f64).sqrt())
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() || values.iter().any(|v| !v.is_finite()) {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let n = values.len();
    Some(if n % 2 == 0 {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    } else {
        values[n / 2]
    })
}

#[derive(Debug, Clone)]
struct EmaCascade {
    alpha: f64,
    poles: [Option<f64>; GC_POLES],
}

impl EmaCascade {
    fn new(period: usize) -> Self {
        let beta = (1.0 - (2.0 * std::f64::consts::PI / period as f64).cos())
            / (2.0_f64.powf(1.0 / GC_POLES as f64) - 1.0);
        let alpha = -beta + (beta * beta + 2.0 * beta).sqrt();
        Self {
            alpha,
            poles: [None; GC_POLES],
        }
    }

    fn push(&mut self, mut value: f64) -> f64 {
        for state in &mut self.poles {
            value = match *state {
                None => value,
                Some(prev) => self.alpha * value + (1.0 - self.alpha) * prev,
            };
            *state = Some(value);
        }
        value
    }
}

/// Compute the complete daily feature frame.
/// Per-asset funding: day -> realised daily rate. Missing assets or days read
/// as 0.0, matching the harness (funding was patchy for small names).
pub type FundingTable = BTreeMap<String, BTreeMap<NaiveDate, f64>>;

/// Which way the funding feature windows run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FundingWindow {
    /// Days up to and including the feature row's own day. The only honest
    /// choice: everything in the window has already printed.
    #[default]
    Trailing,
    /// The original harness's bug, kept ONLY as a parity diagnostic: windows
    /// run forward over realised rates from days that have not happened at
    /// decision time. Reproducing the recorded +760%/Sharpe 2.5 through this
    /// path is the proof that the Rust port is computationally equivalent to
    /// the Python harness. Nothing outside that test may use it.
    ForwardLeakyDiagnostic,
}

pub fn daily(
    bars: &[Bar],
    benchmark: Option<&str>,
    perp_listed_from: &BTreeMap<String, NaiveDate>,
    funding: &FundingTable,
    funding_window: FundingWindow,
) -> Result<Vec<DailyRow>, String> {
    let grouped = checked_grouped(bars, 86_400)?;
    let mut rows = Vec::new();

    for (asset, bars) in grouped {
        let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
        let mut returns: Vec<Option<f64>> = Vec::with_capacity(bars.len());
        let mut gc_price = EmaCascade::new(GC_PERIOD);
        let mut gc_tr = EmaCascade::new(GC_PERIOD);
        let mut regime_price = EmaCascade::new(GC_REGIME_PERIOD);
        let mut regime_tr = EmaCascade::new(GC_REGIME_PERIOD);
        let mut regime_values: Vec<f64> = Vec::new();
        let mut broken = false;
        let mut frozen_open = None;
        let mut frozen_close = None;
        let mut breakout_age = None;
        // Histories for the fast-daily rolling windows.
        let mut trs: Vec<f64> = Vec::with_capacity(bars.len());
        let asset_funding = funding.get(&asset);

        for (i, bar) in bars.iter().enumerate() {
            let ret_1 = i
                .checked_sub(1)
                .and_then(|j| finite((bar.close / bars[j].close).ln()));
            returns.push(ret_1);

            if !broken && ret_1.is_some_and(|r| r > MAX_BAR_LOG_RETURN) {
                broken = true;
                frozen_open = Some(bars[i - 1].open);
                frozen_close = Some(bars[i - 1].close);
            }
            let bars_available = if broken { 0 } else { (i + 1) as u32 };

            let typical = (bar.high + bar.low + bar.close) / 3.0;
            let tr = if i == 0 {
                bar.high - bar.low
            } else {
                (bar.high - bar.low)
                    .max((bar.high - bars[i - 1].close).abs())
                    .max((bar.low - bars[i - 1].close).abs())
            };
            trs.push(tr);
            let gf = gc_price.push(typical);
            let gtr = gc_tr.push(tr);
            let rf = regime_price.push(typical);
            let rtr = regime_tr.push(tr);
            regime_values.push(rf);

            let (gc_filter, gc_upper, gc_lower) = if bars_available as usize >= GC_PERIOD {
                (
                    Some(gf),
                    Some(gf + gtr * GC_MULTIPLIER),
                    Some(gf - gtr * GC_MULTIPLIER),
                )
            } else {
                (None, None, None)
            };
            breakout_age = match gc_upper {
                Some(upper) if bar.close > upper => Some(breakout_age.unwrap_or(0) + 1),
                _ => None,
            };

            let (gc_regime_filter, gc_regime_upper, gc_regime_slope) =
                if bars_available as usize >= GC_REGIME_PERIOD {
                    let slope = i
                        .checked_sub(GC_REGIME_SLOPE_BARS)
                        .and_then(|j| finite(rf / regime_values[j] - 1.0));
                    (Some(rf), Some(rf + rtr * GC_MULTIPLIER), slope)
                } else {
                    (None, None, None)
                };

            let vol_30 = if i + 1 > VOL_WINDOW {
                let window: Option<Vec<f64>> =
                    returns[i + 1 - VOL_WINDOW..=i].iter().copied().collect();
                window
                    .and_then(|w| sample_std(&w))
                    .map(|v| v * 365.0_f64.sqrt())
            } else {
                None
            };
            let adv_quote = if i + 1 >= ADV_WINDOW {
                bars[i + 1 - ADV_WINDOW..=i]
                    .iter()
                    .map(|b| b.quote_volume)
                    .collect::<Option<Vec<_>>>()
                    .and_then(median)
            } else {
                None
            };

            // The restored slow daily block. Same conventions as vol_30: a
            // window is only computed once fully populated, gaps poison it to
            // None rather than shortening it, and vols are annualised.
            let ret_window = |n: usize| -> Option<Vec<f64>> {
                (i + 1 > n)
                    .then(|| returns[i + 1 - n..=i].iter().copied().collect())
                    .flatten()
            };
            let vol_7 = ret_window(7)
                .and_then(|w| sample_std(&w))
                .map(|v| v * 365.0_f64.sqrt());
            let vol_90 = ret_window(90)
                .and_then(|w| sample_std(&w))
                .map(|v| v * 365.0_f64.sqrt());
            let skew_30 = ret_window(30).and_then(|w| sample_skew(&w));
            let semivol_30 = ret_window(30)
                .and_then(|w| downside_std(&w))
                .map(|v| v * 365.0_f64.sqrt());
            let (dist_high_90, dist_low_90) = if i + 1 >= 90 {
                let w = &closes[i + 1 - 90..=i];
                let hi = w.iter().copied().fold(f64::MIN, f64::max);
                let lo = w.iter().copied().fold(f64::MAX, f64::min);
                (finite(bar.close / hi - 1.0), finite(bar.close / lo - 1.0))
            } else {
                (None, None)
            };
            let amihud_30 = if i + 1 >= 30 {
                let terms: Option<Vec<f64>> = (i + 1 - 30..=i)
                    .map(|j| {
                        returns[j]
                            .zip(bars[j].quote_volume)
                            .and_then(|(r, qv)| (qv > 0.0).then(|| r.abs() / qv))
                    })
                    .collect();
                terms.and_then(|t| finite(t.iter().sum::<f64>() / t.len() as f64))
            } else {
                None
            };
            let turnover_20 = if i + 1 >= 20 {
                bars[i + 1 - 20..=i]
                    .iter()
                    .map(|b| b.quote_volume)
                    .collect::<Option<Vec<_>>>()
                    .and_then(|w| finite(w.iter().sum::<f64>() / w.len() as f64))
            } else {
                None
            };
            let range_frac_14 = if i + 1 >= 14 {
                let terms: Vec<f64> = bars[i + 1 - 14..=i]
                    .iter()
                    .filter_map(|b| finite((b.high - b.low) / b.close))
                    .collect();
                (terms.len() == 14).then(|| terms.iter().sum::<f64>() / 14.0)
            } else {
                None
            };

            // --- the recovered harness's fast-daily block ------------------
            // Simple returns here, not log: the originals were close/prev - 1
            // and f_accel is their first difference.
            let pc = (i >= 1).then(|| bars[i - 1].close);
            let sret = |k: usize| -> Option<f64> {
                i.checked_sub(k)
                    .and_then(|j| finite(bar.close / bars[j].close - 1.0))
            };
            let f_gap = pc.and_then(|pc| finite((bar.open - pc) / pc));
            let f_range = finite((bar.high - bar.low) / bar.close);
            let f_clv = if bar.high > bar.low {
                finite((bar.close - bar.low) / (bar.high - bar.low))
            } else {
                Some(0.5)
            };
            // rolling windows of 20 INCLUDING today, at least 10 present -
            // polars rolling_*(20, min_samples=10) semantics.
            let window20 = |xs: &dyn Fn(usize) -> Option<f64>| -> Vec<f64> {
                (i.saturating_sub(19)..=i).filter_map(xs).collect()
            };
            let qv_window = window20(&|j| bars[j].quote_volume);
            let f_volsurp = bar.quote_volume.and_then(|qv| {
                (qv_window.len() >= 10)
                    .then(|| median(qv_window.clone()))
                    .flatten()
                    .and_then(|m| (m > 0.0).then(|| finite(qv / m)).flatten())
            });
            let tr_window = window20(&|j| Some(trs[j]));
            let f_trsurp = (tr_window.len() >= 10)
                .then(|| tr_window.iter().sum::<f64>() / tr_window.len() as f64)
                .and_then(|m| finite(tr / m));
            let f_amihud = pc.zip(bar.quote_volume).and_then(|(pc, qv)| {
                (qv > 0.0)
                    .then(|| finite((bar.close / pc - 1.0).abs() / qv))
                    .flatten()
            });
            let f_ret2 = sret(2);
            let f_ret3 = sret(3);
            let f_accel = match (sret(1), i.checked_sub(2)) {
                (Some(r1), Some(j)) => finite(r1 - (bars[i - 1].close / bars[j].close - 1.0)),
                _ => None,
            };
            let f_dvol = (i >= 1)
                .then(|| bars[i - 1].quote_volume.zip(bar.quote_volume))
                .flatten()
                .and_then(|(pv, qv)| (pv > 0.0).then(|| finite(qv / pv - 1.0)).flatten());

            // --- channel geometry --------------------------------------------
            let dist_upper = gc_upper
                .filter(|u| *u != 0.0)
                .and_then(|u| finite((bar.close - u) / u.abs()));
            let dist_lower = gc_lower
                .filter(|l| *l != 0.0)
                .and_then(|l| finite((bar.close - l) / l.abs()));

            // --- funding ------------------------------------------------------
            // A sum over days relative to this row's own date. Trailing k runs
            // 0..n (today backwards); the leaky diagnostic runs 1..=n forward.
            let day0 = bar.ts_utc.date_naive();
            let fsum = |n: i64| -> f64 {
                let Some(t) = asset_funding else { return 0.0 };
                (0..n)
                    .map(|k| {
                        let d = match funding_window {
                            FundingWindow::Trailing => day0 - chrono::Duration::days(k),
                            FundingWindow::ForwardLeakyDiagnostic => {
                                day0 + chrono::Duration::days(k + 1)
                            }
                        };
                        t.get(&d).copied().unwrap_or(0.0)
                    })
                    .sum()
            };
            let fsum_prev7 = |_: ()| -> f64 {
                let Some(t) = asset_funding else { return 0.0 };
                (0..7)
                    .map(|k| {
                        let d = match funding_window {
                            FundingWindow::Trailing => day0 - chrono::Duration::days(7 + k),
                            FundingWindow::ForwardLeakyDiagnostic => {
                                day0 - chrono::Duration::days(6 - k)
                            }
                        };
                        t.get(&d).copied().unwrap_or(0.0)
                    })
                    .sum()
            };
            let f7 = fsum(7);
            let f30 = fsum(30);
            let funding_chg = f7 - fsum_prev7(());
            let funding_z = (f7 - f30 / 30.0 * 7.0) / (f30.abs() / 30.0 * 7.0 + 1e-6);
            let ret_90_now = shifted_return(&closes, i, 90);
            let vol_ratio = vol_30
                .zip(ret_90_now.filter(|r| *r != 0.0))
                .and_then(|(v, r)| finite(v / r.abs()));

            rows.push(DailyRow {
                ts_utc: bar.ts_utc,
                asset: asset.clone(),
                open: bar.open,
                high: bar.high,
                low: bar.low,
                close: bar.close,
                mark_open: frozen_open.unwrap_or(bar.open),
                mark_close: frozen_close.unwrap_or(bar.close),
                had_discontinuity: broken,
                bars_available,
                perp_listed: perp_listed_from
                    .get(&asset)
                    .is_some_and(|d| *d <= bar.ts_utc.date_naive()),
                ret_1,
                ret_7: shifted_return(&closes, i, 7),
                ret_30: shifted_return(&closes, i, 30),
                ret_90: shifted_return(&closes, i, 90),
                ret_30_skip_7: i.checked_sub(30).map(|j| closes[i - 7] / closes[j] - 1.0),
                vol_30,
                adv_quote,
                ret_180: shifted_return(&closes, i, 180),
                vol_7,
                vol_90,
                skew_30,
                semivol_30,
                dist_high_90,
                dist_low_90,
                amihud_30,
                turnover_20,
                range_frac_14,
                // Channel width relative to its centre: a volatility regime
                // reading the EMA cascade already paid for.
                band_width: match (gc_upper, gc_lower, gc_filter) {
                    (Some(u), Some(l), Some(f)) if f != 0.0 => finite((u - l) / f),
                    _ => None,
                },
                dist_upper,
                dist_lower,
                // The harness read a missing age as zero, not as missing.
                breakout_age_f: Some(breakout_age.map(|a: u32| a as f64).unwrap_or(0.0)),
                vol_ratio,
                f_gap,
                f_range,
                f_clv,
                f_volsurp,
                f_trsurp,
                f_amihud,
                f_ret2,
                f_ret3,
                f_accel,
                f_dvol,
                funding_7d: finite(f7),
                funding_30d: finite(f30),
                funding_chg: finite(funding_chg),
                funding_z: finite(funding_z),
                beta_bench: None,
                gc_filter,
                gc_upper,
                gc_lower,
                gc_breakout_age: breakout_age,
                gc_regime_filter,
                gc_regime_upper,
                gc_regime_slope,
            });
        }
    }

    rows.sort_by(|a, b| a.asset.cmp(&b.asset).then_with(|| a.ts_utc.cmp(&b.ts_utc)));
    attach_beta(&mut rows, benchmark);
    Ok(rows)
}

fn attach_beta(rows: &mut [DailyRow], benchmark: Option<&str>) {
    let Some(benchmark) = benchmark else { return };
    let bench: BTreeMap<DateTime<Utc>, f64> = rows
        .iter()
        .filter(|r| r.asset == benchmark)
        .filter_map(|r| r.ret_1.map(|v| (r.ts_utc, v)))
        .collect();
    if bench.is_empty() {
        return;
    }

    let mut start = 0;
    while start < rows.len() {
        let asset = rows[start].asset.clone();
        let end = rows[start..]
            .iter()
            .position(|r| r.asset != asset)
            .map(|n| start + n)
            .unwrap_or(rows.len());
        let mut pairs: VecDeque<(f64, f64)> = VecDeque::new();
        for row in &mut rows[start..end] {
            match (row.ret_1, bench.get(&row.ts_utc).copied()) {
                (Some(x), Some(y)) => pairs.push_back((x, y)),
                _ => pairs.push_back((f64::NAN, f64::NAN)),
            }
            if pairs.len() > BETA_WINDOW {
                pairs.pop_front();
            }
            if pairs.len() == BETA_WINDOW
                && pairs.iter().all(|(x, y)| x.is_finite() && y.is_finite())
            {
                let n = pairs.len() as f64;
                let mx = pairs.iter().map(|p| p.0).sum::<f64>() / n;
                let my = pairs.iter().map(|p| p.1).sum::<f64>() / n;
                let cov = pairs.iter().map(|(x, y)| (x - mx) * (y - my)).sum::<f64>();
                let var = pairs.iter().map(|(_, y)| (y - my).powi(2)).sum::<f64>();
                row.beta_bench = (var > 0.0).then(|| cov / var).and_then(finite);
            }
        }
        start = end;
    }
}

fn window(values: &[Option<f64>], i: usize, n: usize) -> Option<Vec<f64>> {
    if i + 1 < n {
        return None;
    }
    values[i + 1 - n..=i].iter().copied().collect()
}

fn sum_window(values: &[Option<f64>], i: usize, n: usize) -> Option<f64> {
    window(values, i, n).and_then(|v| finite(v.iter().sum()))
}

fn max_window(values: &[Option<f64>], i: usize, n: usize) -> Option<f64> {
    window(values, i, n)
        .and_then(|v| v.into_iter().reduce(f64::max))
        .and_then(finite)
}

fn skew(values: &[f64]) -> Option<f64> {
    // Polars' rolling_skew default is the uncorrected third standard moment.
    let n = values.len();
    if n < 3 {
        return None;
    }
    let mean = values.iter().sum::<f64>() / n as f64;
    let m2 = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n as f64;
    if m2 == 0.0 {
        return Some(0.0);
    }
    let m3 = values.iter().map(|v| (v - mean).powi(3)).sum::<f64>() / n as f64;
    finite(m3 / m2.powf(1.5))
}

/// Compute all hourly model inputs. These rows are the only permitted source
/// for a training matrix and the exact same function feeds live inference.
pub fn hourly(bars: &[Bar], benchmark: Option<&str>) -> Result<Vec<HourlyRow>, String> {
    hourly_with(bars, benchmark, |_| true)
}

/// One snapshot per daily decision, stamped on the final fully closed hourly
/// bar before 00:00 UTC. Both training and runtime call this exact function:
/// using the 00:00 bar would train on an hour that was still forming when the
/// decision was made, while using a daily cutoff would discard 23 known bars.
pub fn hourly_before_daily_decision(
    bars: &[Bar],
    benchmark: Option<&str>,
) -> Result<Vec<HourlyRow>, String> {
    hourly_with(bars, benchmark, |ts| ts.hour() == 23)
}

fn hourly_with<F: Fn(DateTime<Utc>) -> bool>(
    bars: &[Bar],
    benchmark: Option<&str>,
    keep: F,
) -> Result<Vec<HourlyRow>, String> {
    let grouped = checked_grouped(bars, 3_600)?;
    let mut rows = Vec::new();

    for (asset, bars) in grouped {
        let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
        let returns: Vec<Option<f64>> = bars
            .iter()
            .enumerate()
            .map(|(i, b)| {
                i.checked_sub(1)
                    .and_then(|j| finite(b.close / bars[j].close - 1.0))
            })
            .collect();
        let abs_returns: Vec<Option<f64>> = returns.iter().map(|v| v.map(f64::abs)).collect();
        let quote: Vec<Option<f64>> = bars.iter().map(|b| b.quote_volume).collect();
        let trade_sizes: Vec<Option<f64>> = bars
            .iter()
            .map(|b| {
                b.quote_volume
                    .zip(b.trades)
                    .and_then(|(q, t)| finite(q / (t + 1) as f64))
            })
            .collect();

        for (i, bar) in bars.iter().enumerate() {
            let calc = |n: usize| {
                let rv = window(&returns, i, n)
                    .and_then(|v| sample_std(&v))
                    .map(|v| v * (24.0_f64 * 365.0).sqrt());
                let ret = i
                    .checked_sub(n)
                    .and_then(|j| finite(closes[i] / closes[j] - 1.0));
                let path = sum_window(&abs_returns, i, n);
                let eff = ret
                    .zip(path)
                    .and_then(|(r, p)| finite(r.abs() / (p + 1e-12)));
                let jump = max_window(&abs_returns, i, n);
                let dv = sum_window(&quote, i, n);
                (rv, ret, eff, jump, dv)
            };
            let w6 = calc(6);
            let w24 = calc(24);
            let w72 = calc(72);
            let w168 = calc(168);

            let r24 = window(&returns, i, 24);
            let semi_dn = r24.as_ref().map(|v| {
                v.iter()
                    .filter(|x| **x < 0.0)
                    .map(|x| x * x)
                    .sum::<f64>()
                    .sqrt()
            });
            let semi_up = r24.as_ref().map(|v| {
                v.iter()
                    .filter(|x| **x > 0.0)
                    .map(|x| x * x)
                    .sum::<f64>()
                    .sqrt()
            });
            let semi_ratio = semi_up
                .zip(semi_dn)
                .and_then(|(u, d)| finite(u / (d + 1e-12)));
            let skew_24h = r24.as_deref().and_then(skew);
            let trade_size_surp = trade_sizes[i].and_then(|now| {
                if i + 1 < 48 {
                    return None;
                }
                let from = (i + 1).saturating_sub(168);
                let values: Option<Vec<f64>> = trade_sizes[from..=i].iter().copied().collect();
                values
                    .and_then(median)
                    .and_then(|m| finite(now / (m + 1e-12)))
            });
            let trades_24h = if i + 1 >= 24 {
                bars[i + 1 - 24..=i]
                    .iter()
                    .map(|b| b.trades.map(|v| v as f64))
                    .collect::<Option<Vec<_>>>()
                    .map(|v| v.iter().sum())
            } else {
                None
            };
            let dd_168h = if i + 1 >= 168 {
                let high = bars[i + 1 - 168..=i]
                    .iter()
                    .map(|b| b.close)
                    .reduce(f64::max)
                    .unwrap();
                finite(bar.close / high - 1.0)
            } else {
                None
            };
            let vol_concentration = if i + 1 >= 24 {
                let values: Option<Vec<f64>> = quote[i + 1 - 24..=i].iter().copied().collect();
                values.and_then(|v| {
                    finite(
                        v.iter().copied().reduce(f64::max).unwrap()
                            / (v.iter().sum::<f64>() + 1e-12),
                    )
                })
            } else {
                None
            };

            if keep(bar.ts_utc) {
                rows.push(HourlyRow {
                    ts_utc: bar.ts_utc,
                    asset: asset.clone(),
                    rv_6h: w6.0,
                    rv_24h: w24.0,
                    rv_72h: w72.0,
                    rv_168h: w168.0,
                    ret_6h: w6.1,
                    ret_24h: w24.1,
                    ret_72h: w72.1,
                    ret_168h: w168.1,
                    eff_6h: w6.2,
                    eff_24h: w24.2,
                    eff_72h: w72.2,
                    eff_168h: w168.2,
                    jump_6h: w6.3,
                    jump_24h: w24.3,
                    jump_72h: w72.3,
                    jump_168h: w168.3,
                    dv_6h: w6.4,
                    dv_24h: w24.4,
                    dv_72h: w72.4,
                    dv_168h: w168.4,
                    semi_dn,
                    semi_up,
                    semi_ratio,
                    skew_24h,
                    trade_size_surp,
                    trades_24h,
                    dd_168h,
                    vol_concentration,
                    rel_ret_24h: None,
                    rel_vol_24h: None,
                });
            }
        }
    }
    rows.sort_by(|a, b| a.asset.cmp(&b.asset).then_with(|| a.ts_utc.cmp(&b.ts_utc)));
    attach_relative(&mut rows, benchmark);
    Ok(rows)
}

fn attach_relative(rows: &mut [HourlyRow], benchmark: Option<&str>) {
    let Some(benchmark) = benchmark else { return };
    let bench: BTreeMap<DateTime<Utc>, (Option<f64>, Option<f64>)> = rows
        .iter()
        .filter(|r| r.asset == benchmark)
        .map(|r| (r.ts_utc, (r.ret_24h, r.rv_24h)))
        .collect();
    for row in rows {
        if let Some((br, bv)) = bench.get(&row.ts_utc) {
            row.rel_ret_24h = row.ret_24h.zip(*br).and_then(|(x, b)| finite(x - b));
            row.rel_vol_24h = row
                .rv_24h
                .zip(*bv)
                .and_then(|(x, b)| finite(x / (b + 1e-12)));
        }
    }
}

/// Stable, ordered model feature catalogue. A model manifest must name an
/// exact subset of this list; unknown or duplicate names are refused.
pub fn validate_selection(names: &[String]) -> Result<(), String> {
    if names.is_empty() {
        return Err("model selects no features".into());
    }
    let all: std::collections::BTreeSet<&str> = DAILY_FEATURE_NAMES
        .iter()
        .chain(HOURLY_FEATURE_NAMES.iter())
        .copied()
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    for name in names {
        if !all.contains(name.as_str()) {
            return Err(format!("unknown crypto feature {name:?}"));
        }
        if !seen.insert(name) {
            return Err(format!("duplicate crypto feature {name:?}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;

    fn bars(interval_s: i32, n: usize, asset: &str, scale: f64) -> Vec<Bar> {
        let start = "2025-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let mut close = 100.0 * scale;
        (0..n)
            .map(|i| {
                let open = close;
                close *= ((i as f64 * 0.7).sin() * 0.01 + 0.001).exp();
                Bar {
                    ts_utc: start + TimeDelta::seconds(interval_s as i64 * i as i64),
                    asset: asset.into(),
                    interval_s,
                    open,
                    high: open.max(close),
                    low: open.min(close),
                    close,
                    volume: 1_000.0,
                    quote_volume: Some(1_000.0 * close),
                    trades: Some(100),
                }
            })
            .collect()
    }

    #[test]
    fn daily_features_are_prefix_invariant() {
        let mut input = bars(86_400, 200, "BTC", 1.0);
        input.extend(bars(86_400, 200, "ETH", 20.0));
        let full = daily(
            &input,
            Some("BTC"),
            &BTreeMap::new(),
            &BTreeMap::new(),
            FundingWindow::Trailing,
        )
        .unwrap();
        let cutoff = input.iter().map(|b| b.ts_utc).min().unwrap() + TimeDelta::days(159);
        let prefix_bars: Vec<_> = input.into_iter().filter(|b| b.ts_utc <= cutoff).collect();
        let prefix = daily(
            &prefix_bars,
            Some("BTC"),
            &BTreeMap::new(),
            &BTreeMap::new(),
            FundingWindow::Trailing,
        )
        .unwrap();
        let expected: Vec<_> = full.into_iter().filter(|r| r.ts_utc <= cutoff).collect();
        assert_eq!(prefix, expected);
    }

    #[test]
    fn hourly_features_are_prefix_invariant() {
        let input = bars(3_600, 240, "BTC", 1.0);
        let full = hourly(&input, Some("BTC")).unwrap();
        let prefix = hourly(&input[..200], Some("BTC")).unwrap();
        assert_eq!(prefix, full[..200]);
    }

    #[test]
    fn daily_decision_projection_is_the_exact_full_feature_subset() {
        let mut input = bars(3_600, 240, "BTC", 1.0);
        input.extend(bars(3_600, 240, "ETH", 2.0));
        let expected: Vec<_> = hourly(&input, Some("BTC"))
            .unwrap()
            .into_iter()
            .filter(|row| row.ts_utc.hour() == 23)
            .collect();
        assert_eq!(
            hourly_before_daily_decision(&input, Some("BTC")).unwrap(),
            expected
        );
    }

    #[test]
    fn beta_of_benchmark_is_one() {
        let input = bars(86_400, 200, "BTC", 1.0);
        let out = daily(
            &input,
            Some("BTC"),
            &BTreeMap::new(),
            &BTreeMap::new(),
            FundingWindow::Trailing,
        )
        .unwrap();
        assert!((out.last().unwrap().beta_bench.unwrap() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn feature_selection_is_closed() {
        validate_selection(&["ret_30".into(), "rv_24h".into()]).unwrap();
        assert!(validate_selection(&["made_up".into()]).is_err());
        assert!(validate_selection(&["ret_30".into(), "ret_30".into()]).is_err());
    }

    #[test]
    fn rank_normalisation_is_shared_and_nulls_are_neutral() {
        let out = rank_normalise(&[
            vec![Some(10.0), None],
            vec![Some(20.0), Some(5.0)],
            vec![Some(20.0), Some(9.0)],
        ])
        .unwrap();
        assert_eq!(out[0], vec![-1.0, 0.0]);
        assert_eq!(out[1], vec![0.5, -1.0]);
        assert_eq!(out[2], vec![0.5, 0.0]);
    }

    fn close(actual: Option<f64>, expected: f64) {
        let actual = actual.expect("feature is warm");
        let tolerance = 1e-11_f64.max(expected.abs() * 1e-11);
        assert!(
            (actual - expected).abs() <= tolerance,
            "{actual} != {expected}"
        );
    }

    #[test]
    fn matches_the_frozen_python_daily_reference() {
        // Generated once from the former Python feature code using the
        // deterministic `bars` series above. This fixture keeps
        // the old validated semantics without keeping a second implementation.
        let input = bars(86_400, 200, "BTC", 1.0);
        let out = daily(
            &input,
            Some("BTC"),
            &BTreeMap::new(),
            &BTreeMap::new(),
            FundingWindow::Trailing,
        )
        .unwrap();
        let r = out.last().unwrap();
        close(r.ret_1, 0.009771636483385665);
        close(r.ret_7, -0.008905970393982998);
        close(r.ret_30, 0.0394265189892371);
        close(r.ret_90, 0.09678336639244578);
        close(r.ret_30_skip_7, 0.04876680510570042);
        close(r.vol_30, 0.1349708626702849);
        close(r.adv_quote, 122516.45522627141);
        close(r.beta_bench, 1.0000000000000002);
        close(r.gc_filter, 119.20708458482461);
        close(r.gc_upper, 120.29455075585516);
        close(r.gc_lower, 118.11961841379407);
        assert_eq!(r.gc_breakout_age, Some(57));
        close(r.gc_regime_filter, 122.4073589752605);
        close(r.gc_regime_upper, 123.50992125568415);
        close(r.gc_regime_slope, 0.020065011441803282);
    }

    #[test]
    fn matches_the_frozen_python_hourly_reference() {
        let input = bars(3_600, 240, "BTC", 1.0);
        let out = hourly(&input, Some("BTC")).unwrap();
        let r = out.last().unwrap();
        close(r.rv_6h, 0.6040378463486975);
        close(r.rv_24h, 0.6647523622098441);
        close(r.rv_72h, 0.6676620157853556);
        close(r.rv_168h, 0.6625958662964266);
        close(r.ret_6h, 0.026896627812829754);
        close(r.ret_24h, 0.04558417391078984);
        close(r.ret_72h, 0.07280091370594999);
        close(r.ret_168h, 0.2071466421775976);
        close(r.eff_6h, 0.6905092567523887);
        close(r.eff_24h, 0.2989258893640806);
        close(r.eff_72h, 0.1585596756525952);
        close(r.eff_168h, 0.19152625593685202);
        close(r.jump_6h, 0.010704170565662974);
        close(r.jump_24h, 0.010788119828054832);
        close(r.jump_72h, 0.010949178820337302);
        close(r.jump_168h, 0.01106068695998319);
        close(r.dv_6h, 775268.7386583609);
        close(r.dv_24h, 3061731.613263807);
        close(r.dv_72h, 8958243.246010592);
        close(r.dv_168h, 19944593.58754141);
        close(r.semi_dn, 0.019420139975029255);
        close(r.semi_up, 0.029465246615687533);
        close(r.semi_ratio, 1.5172520204312223);
        close(r.skew_24h, -0.17538151838987906);
        close(r.trade_size_surp, 1.0927806107637197);
        close(r.trades_24h, 2400.0);
        close(r.dd_168h, -0.006123933815370908);
        close(r.vol_concentration, 0.042608122913021906);
        close(r.rel_ret_24h, 0.0);
        close(r.rel_vol_24h, 0.9999999999984958);
    }
}
