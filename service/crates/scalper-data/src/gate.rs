//! The gate: `scalper-data gate` predicts every walk-forward fold's test
//! window through `lightgbm-json` (the same artifact-loading path the
//! plan-4 live bot will use), simulates a threshold-gated long/short
//! strategy net of measured round-trip costs, and reports the annualized
//! Sharpe gate. This module IS the live bot's trading logic in miniature -
//! precision over speed, no shortcuts.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use features_crypto::Bar;

use crate::binance_costs::DayCost;
use crate::binance_micro;
use crate::costs::{percentile, CostSummary};
use crate::exits::{self, ExitKind};
use crate::matrix::MatrixRow;
use crate::{get, need};

/// The one artifact format the gate (and the eventual live bot) will ever
/// load - same constant Task 3/4a's Python side stamps into every fold.
pub const MODEL_FORMAT_VERSION: &str = "crypto-lightgbm-json-1";

/// Taker fee assumed for a round-trip (open + close), in bps one-way.
pub const TAKER_FEE_BPS: f64 = 4.5;

/// Round-trip cost charged to an asset the plan-2 cost summary never saw -
/// conservative rather than free, so a data gap can't manufacture edge.
pub const DEFAULT_ROUND_TRIP_BPS: f64 = 20.0;

pub const DEFAULT_THRESHOLD_MULT: f64 = 1.5;
pub const DEFAULT_NOTIONAL: &str = "5000";

/// Sharpe a fold set must clear to PASS the gate.
pub const GATE_THRESHOLD: f64 = 2.0;

/// A single scored row: model output and the realized forward return the
/// matrix already computed for it (the trade's gross return by
/// construction, since `fwd_bps` at horizon H IS "what happened H minutes
/// later").
#[derive(Debug, Clone, PartialEq)]
pub struct Pred {
    pub ts: i64,
    pub asset: String,
    pub pred_bps: f64,
    pub fwd_bps: f64,
}

/// A booked trade: `side` is +1 (long) or -1 (short), `net_bps` already has
/// round-trip cost subtracted.
#[derive(Debug, Clone, PartialEq)]
pub struct Trade {
    pub asset: String,
    pub entry_ts: i64,
    pub side: i8,
    pub net_bps: f64,
}

/// The house artifact envelope (Task 3's `train_scalper.py` /
/// `walk_forward_scalper.py` output): metadata plus LightGBM's raw
/// `dump_model()`. Unknown fields (trained_through, trained_at, n_rows,
/// label, model_version, horizon_min, and everything LightGBM stuffs into
/// `model` besides `tree_info`) are intentionally ignored here - this
/// loader's only job is trees, the feature order they were fit against, and
/// (as of the version cross-check below) the feature-set version they were
/// fit against, mirroring `crypto_portfolio::model::Model::load`'s parsing
/// idiom without inheriting its crypto-portfolio-specific checks
/// (model_version, x_-prefix convention) that don't apply to the scalper's
/// artifact.
#[derive(Debug, Deserialize)]
struct ModelArtifact {
    format_version: String,
    feature_set_version: String,
    features: Vec<String>,
    model: ModelDump,
}

#[derive(Debug, Deserialize)]
struct ModelDump {
    tree_info: Vec<lightgbm_json::Tree>,
}

/// Load a fold artifact and return its trees, having verified the artifact
/// is the expected format, that its `feature_set_version` matches
/// `expected_version` (the matrix manifest's), and that its feature order
/// matches `expected_features` exactly - order-exact, because
/// `lightgbm_json::Tree` addresses features by index, not name. Every
/// mismatch is a hard error: a stale fold artifact scored against a matrix
/// built from a different feature set would silently score the wrong
/// inputs, or the right inputs under the wrong semantics.
pub fn load_model(
    path: &Path,
    expected_features: &[String],
    expected_version: &str,
) -> Result<Vec<lightgbm_json::Tree>, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("cannot read model {}: {e}", path.display()))?;
    let art: ModelArtifact =
        serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    if art.format_version != MODEL_FORMAT_VERSION {
        return Err(format!(
            "{}: model format {:?}, expected {MODEL_FORMAT_VERSION:?}",
            path.display(),
            art.format_version
        ));
    }
    if art.feature_set_version != expected_version {
        return Err(format!(
            "{}: model feature_set_version {:?} != matrix feature_set_version {:?}",
            path.display(),
            art.feature_set_version,
            expected_version
        ));
    }
    if art.features != expected_features {
        return Err(format!(
            "{}: model features {:?} != matrix features {:?} (order-exact match required)",
            path.display(),
            art.features,
            expected_features
        ));
    }
    if art.model.tree_info.is_empty() {
        return Err(format!("{}: model contains no trees", path.display()));
    }
    Ok(art.model.tree_info)
}

/// Per-asset threshold-gated long/short simulation, ignorant of any position
/// held before `preds` starts - convenience wrapper over
/// `simulate_with_state` for callers (and tests) that only ever see one
/// contiguous batch of predictions. The walk-forward gate does NOT use this
/// directly: it must carry hold state across fold seams, see
/// `simulate_with_state`. (This crate has no lib target, so `pub` doesn't
/// exempt it from `dead_code` once `cmd_gate` stops calling it directly -
/// the tests are its only caller, which is the point of keeping it.)
#[allow(dead_code)]
pub fn simulate(
    preds: &[Pred],
    round_trip_bps: &BTreeMap<String, f64>,
    horizon_min: i64,
    threshold_mult: f64,
) -> Vec<Trade> {
    simulate_with_state(
        preds,
        round_trip_bps,
        horizon_min,
        threshold_mult,
        &BTreeMap::new(),
    )
    .0
}

/// Per-asset threshold-gated long/short simulation, carrying and returning
/// per-asset hold state (`asset -> ts the position becomes flat again`).
///
/// `preds` need not arrive pre-sorted - this sorts a copy by (asset, ts)
/// itself so a caller's ordering mistake can't silently corrupt the hold
/// logic. Assets absent from `round_trip_bps` are untradeable and skipped
/// entirely (the caller decides default-cost vs thin-book exclusion before
/// calling this).
///
/// While flat: `pred_bps > threshold_mult * round_trip` opens long,
/// `pred_bps < -threshold_mult * round_trip` opens short, otherwise no
/// signal. A position holds for exactly `horizon_min` minutes; rows with
/// `ts` inside that hold window are skipped (enforced via `ts >=
/// flat_after`), so at most one open position per asset at a time. Net
/// trade bps = the realized `fwd_bps` on the entry row (correctly signed
/// for the side) minus the round-trip cost - both sides pay it once.
///
/// `carry` seeds `flat_after` for assets already holding a position when
/// `preds` starts (e.g. the walk-forward gate stitching fold k+1's
/// predictions onto fold k's open positions) - the live bot has one
/// continuous position per asset, and the simulation has to model that
/// instead of forgetting every open position at each fold boundary. Assets
/// not in `carry` start flat, same as `simulate`.
///
/// A thin wrapper over `simulate_with_state_by_day` (see that function's doc
/// for the shared trading logic): a flat per-asset cost is the same value
/// looked up regardless of entry day.
pub fn simulate_with_state(
    preds: &[Pred],
    round_trip_bps: &BTreeMap<String, f64>,
    horizon_min: i64,
    threshold_mult: f64,
    carry: &BTreeMap<String, i64>,
) -> (Vec<Trade>, BTreeMap<String, i64>) {
    simulate_with_state_impl(
        preds,
        |asset, _ts| round_trip_bps.get(asset).copied(),
        horizon_min,
        threshold_mult,
        carry,
    )
}

/// Same as `simulate`, but the round-trip cost is looked up per (asset,
/// entry day) instead of a flat per-asset value - what the `--binance-costs`
/// gate path needs, since a real Binance round-trip cost varies day to day.
/// (Kept alongside `simulate_with_state_by_day` for the same reason
/// `simulate` is kept alongside `simulate_with_state`: a convenience for
/// callers and tests that don't need to carry hold state across a fold
/// seam.)
#[allow(dead_code)]
pub fn simulate_by_day(
    preds: &[Pred],
    round_trip_by_asset_day: &BTreeMap<String, BTreeMap<NaiveDate, f64>>,
    horizon_min: i64,
    threshold_mult: f64,
) -> Vec<Trade> {
    simulate_with_state_by_day(
        preds,
        round_trip_by_asset_day,
        horizon_min,
        threshold_mult,
        &BTreeMap::new(),
    )
    .0
}

/// `simulate_with_state`'s per-day-cost sibling: `round_trip_by_asset_day`
/// maps `asset -> (entry day -> round_trip_bps)`, already fully resolved -
/// the day-lookup fallback (nearest prior day within 14 days) and the
/// thin-book/missing-data exclusion both happen upstream, in `cmd_gate` via
/// `resolve_round_trip`, before this function ever sees a `Pred`. A pred
/// whose (asset, entry day) has no entry in the map is untradeable and is
/// skipped, exactly like a `simulate_with_state` miss on
/// `round_trip_bps.get(asset)`.
pub fn simulate_with_state_by_day(
    preds: &[Pred],
    round_trip_by_asset_day: &BTreeMap<String, BTreeMap<NaiveDate, f64>>,
    horizon_min: i64,
    threshold_mult: f64,
    carry: &BTreeMap<String, i64>,
) -> (Vec<Trade>, BTreeMap<String, i64>) {
    simulate_with_state_impl(
        preds,
        |asset, ts| {
            round_trip_by_asset_day
                .get(asset)
                .and_then(|by_day| by_day.get(&date_of(ts)))
                .copied()
        },
        horizon_min,
        threshold_mult,
        carry,
    )
}

/// The shared simulation core `simulate_with_state` (flat per-asset cost)
/// and `simulate_with_state_by_day` (per-asset-per-day cost) both reduce to:
/// `round_trip_lookup(asset, entry_ts)` returns `None` for an untradeable
/// (asset, entry) pair, which is skipped exactly like a pred that arrives
/// inside an already-open hold window - the two callers differ only in how
/// they answer that one question, so the actual threshold/hold/net-bps
/// logic lives here exactly once.
fn simulate_with_state_impl<F>(
    preds: &[Pred],
    round_trip_lookup: F,
    horizon_min: i64,
    threshold_mult: f64,
    carry: &BTreeMap<String, i64>,
) -> (Vec<Trade>, BTreeMap<String, i64>)
where
    F: Fn(&str, i64) -> Option<f64>,
{
    let mut sorted: Vec<&Pred> = preds.iter().collect();
    sorted.sort_by(|a, b| (a.asset.as_str(), a.ts).cmp(&(b.asset.as_str(), b.ts)));

    let hold_secs = horizon_min * 60;
    let mut flat_after: BTreeMap<String, i64> = carry.clone();
    let mut trades = Vec::new();

    for p in sorted {
        let Some(round_trip) = round_trip_lookup(p.asset.as_str(), p.ts) else {
            continue;
        };
        let next_free = *flat_after.get(p.asset.as_str()).unwrap_or(&i64::MIN);
        if p.ts < next_free {
            continue; // still held
        }
        let threshold = threshold_mult * round_trip;
        let side: i8 = if p.pred_bps > threshold {
            1
        } else if p.pred_bps < -threshold {
            -1
        } else {
            continue; // no signal
        };
        let net_bps = side as f64 * p.fwd_bps - round_trip;
        trades.push(Trade {
            asset: p.asset.clone(),
            entry_ts: p.ts,
            side,
            net_bps,
        });
        flat_after.insert(p.asset.clone(), p.ts + hold_secs);
    }
    (trades, flat_after)
}

/// One asset's 1m bar path plus its Wilder ATR(14) (`exits::atr14`,
/// index-aligned to `bars`) and a `ts -> index` lookup - everything
/// `simulate_with_state_atr` needs to resolve an accepted entry's Amendment
/// 3 stop/target exit against that asset's actual path. Built once per
/// `cmd_gate` invocation under `--exit atr`, not per fold.
struct AssetPath {
    bars: Vec<Bar>,
    atr: Vec<Option<f64>>,
    index_by_ts: BTreeMap<i64, usize>,
}

impl AssetPath {
    fn new(bars: Vec<Bar>) -> Self {
        let atr = exits::atr14(&bars);
        let index_by_ts = bars
            .iter()
            .enumerate()
            .map(|(i, b)| (b.ts_utc.timestamp(), i))
            .collect();
        AssetPath {
            bars,
            atr,
            index_by_ts,
        }
    }
}

/// Tallies of how `simulate_with_state_atr`'s accepted entries resolved,
/// summed across folds by `cmd_gate` into `Report::exit_stats`.
#[derive(Debug, Clone, Copy, Default)]
struct ExitStatsAccum {
    stops: usize,
    targets: usize,
    time_exits: usize,
    bars_held_sum: u64,
    /// An accepted entry (same threshold rule as `--exit time`) whose asset
    /// had no ATR value yet at the entry bar - cold warm-up, or the entry
    /// bar landed right after a >120s gap - so no stop/target could be
    /// computed. Skipped rather than priced with a fabricated stop/target,
    /// same discipline as every other "can't resolve, don't fabricate"
    /// skip in this module (a missing round-trip cost, a thin book).
    skipped_no_atr: usize,
}

/// `simulate_with_state_by_day`'s Amendment 3 sibling: the entry rule
/// (signal via `pred_bps` vs `threshold_mult * round_trip`, hold-window
/// blocking via `flat_after`, per-(asset, entry day) round-trip cost
/// lookup) is IDENTICAL - only what happens after an entry is accepted
/// differs. Under `--exit time`, the trade's outcome is the matrix's own
/// `fwd_bps` at the fold's fixed horizon; here, it is
/// `exits::resolve_exit` walking the asset's real 1m bar path forward from
/// the entry bar against a `k=4` ATR(14) stop and a `1.2x` target
/// (`exits::stop_target`), stop-wins-ties, with the Amendment-1 fixed-time
/// exit surviving only as a fallback when neither level is touched by
/// `horizon_min` bars later.
///
/// Because of that, `flat_after` is set to the EXIT bar's own ts, not
/// `entry_ts + horizon_min*60s` - a stop or target can free the position
/// before the full horizon elapses, and (matching the live bot's one
/// continuous position per asset) the very next signal after that earlier
/// exit is eligible to enter, not blocked until the original horizon would
/// have expired.
///
/// The only future data this reads beyond what `--exit time` already reads
/// (via `fwd_bps`, itself already a forward-looking label) is `path[1..]` -
/// bars strictly AFTER the entry bar - inside `resolve_exit`, which is
/// exactly the trade's own realized outcome, not information used to
/// accept or size the entry itself. ATR at the entry bar (`path.atr[idx]`)
/// is computed only from bars up to and including the entry bar.
fn simulate_with_state_atr(
    preds: &[Pred],
    round_trip_by_asset_day: &BTreeMap<String, BTreeMap<NaiveDate, f64>>,
    paths: &BTreeMap<String, AssetPath>,
    horizon_min: i64,
    threshold_mult: f64,
    carry: &BTreeMap<String, i64>,
) -> (Vec<Trade>, BTreeMap<String, i64>, ExitStatsAccum) {
    let mut sorted: Vec<&Pred> = preds.iter().collect();
    sorted.sort_by(|a, b| (a.asset.as_str(), a.ts).cmp(&(b.asset.as_str(), b.ts)));

    let horizon_bars = horizon_min.max(0) as usize;
    let mut flat_after: BTreeMap<String, i64> = carry.clone();
    let mut trades = Vec::new();
    let mut stats = ExitStatsAccum::default();

    for p in sorted {
        let Some(round_trip) = round_trip_by_asset_day
            .get(p.asset.as_str())
            .and_then(|by_day| by_day.get(&date_of(p.ts)))
            .copied()
        else {
            continue;
        };
        let next_free = *flat_after.get(p.asset.as_str()).unwrap_or(&i64::MIN);
        if p.ts < next_free {
            continue; // still held
        }
        let threshold = threshold_mult * round_trip;
        let side: i8 = if p.pred_bps > threshold {
            1
        } else if p.pred_bps < -threshold {
            -1
        } else {
            continue; // no signal
        };

        // An asset with a resolvable round-trip cost but no bar path at all
        // (shouldn't happen when --data-root matches the matrix's source,
        // but not assumed) is untradeable in atr mode - skip exactly like a
        // missing cost lookup, not a fabricated exit.
        let Some(path) = paths.get(p.asset.as_str()) else {
            continue;
        };
        let Some(&idx) = path.index_by_ts.get(&p.ts) else {
            continue; // matrix row's ts has no matching bar - can't resolve
        };
        let Some(atr) = path.atr[idx] else {
            stats.skipped_no_atr += 1;
            continue;
        };

        let entry_px = path.bars[idx].close;
        let (stop_px, target_px) = exits::stop_target(entry_px, side, atr);
        let exit = exits::resolve_exit(
            entry_px,
            side,
            stop_px,
            target_px,
            &path.bars[idx..],
            horizon_bars,
        );

        let gross_bps = side as f64 * (exit.exit_px / entry_px - 1.0) * 1e4;
        let net_bps = gross_bps - round_trip;
        trades.push(Trade {
            asset: p.asset.clone(),
            entry_ts: p.ts,
            side,
            net_bps,
        });

        match exit.exit_kind {
            ExitKind::Stop => stats.stops += 1,
            ExitKind::Target => stats.targets += 1,
            ExitKind::Time => stats.time_exits += 1,
        }
        stats.bars_held_sum += exit.bars_held as u64;

        let exit_ts = path.bars[idx + exit.bars_held].ts_utc.timestamp();
        flat_after.insert(p.asset.clone(), exit_ts);
    }
    (trades, flat_after, stats)
}

/// UTC calendar date a unix timestamp falls on.
fn date_of(ts: i64) -> NaiveDate {
    DateTime::<Utc>::from_timestamp(ts, 0)
        .expect("timestamp in range")
        .date_naive()
}

/// Maximum days to walk backward from a missing/thin entry day before
/// giving up - the pre-registered "nearest PRIOR day within 14 days" rule.
const COST_LOOKBACK_DAYS: i64 = 14;

/// `2*fee_taker_bps + spread_bps_p75 + 2*impact_bps` for one already-known
/// `DayCost`.
fn round_trip_formula(fee_taker_bps: f64, dc: &DayCost, impact_bps: f64) -> f64 {
    2.0 * fee_taker_bps + dc.spread_bps_p75 + 2.0 * impact_bps
}

/// Resolve `asset`'s round-trip cost for entry `day` from its time-varying
/// Binance day costs, per the pre-registered rule: the prior-day fallback
/// exists ONLY for a day with NO cost data at all, never for a day that WAS
/// measured and found thin.
///
/// So: if `day` itself has a `DayCost` entry, that entry decides the
/// outcome outright, with no fallback - `Some` if its `impact_bps` is
/// `Some` (the book was measured and could absorb the notional), `None` if
/// `impact_bps` is `None` (the book was measured and was too thin). A
/// measured-and-thin entry day is untradeable on ITS OWN evidence; walking
/// back to a calmer prior day would paper over the one piece of information
/// this whole mechanism exists to surface - that today's book couldn't take
/// the clip.
///
/// Only when `day` has NO entry at all does the walk kick in: backward
/// day-by-day up to `COST_LOOKBACK_DAYS`, returning the nearest prior day
/// that itself has a usable (`impact_bps: Some`) entry - a thin prior day
/// encountered along the way is skipped exactly like an absent one (there
/// is no impact number to price a walk with either way), so the walk keeps
/// going rather than stopping on it. `None` when no usable day turns up
/// anywhere in the window (no `day_costs` for the asset at all, the entry
/// day is thin, or every prior day in the window is itself missing/thin) -
/// the asset is untradeable on this entry day.
pub(crate) fn resolve_round_trip(
    day_costs: Option<&BTreeMap<NaiveDate, DayCost>>,
    day: NaiveDate,
    fee_taker_bps: f64,
) -> Option<f64> {
    let day_costs = day_costs?;

    if let Some(dc) = day_costs.get(&day) {
        return dc
            .impact_bps
            .map(|impact| round_trip_formula(fee_taker_bps, dc, impact));
    }

    for offset in 1..=COST_LOOKBACK_DAYS {
        let d = day - chrono::Duration::days(offset);
        if let Some(dc) = day_costs.get(&d) {
            if let Some(impact) = dc.impact_bps {
                return Some(round_trip_formula(fee_taker_bps, dc, impact));
            }
        }
    }
    None
}

/// Projected 30-day USD volume implied by the observed trade rate over the
/// test window: `total_trades * 2 (both legs) * notional_usd /
/// test_span_days * 30`. `0.0` when `test_span_days` is `0` - nothing to
/// annualize from, not a divide-by-zero `inf`/`NaN`.
pub(crate) fn projected_30d_volume_usd(
    n_trades: usize,
    notional_usd: f64,
    test_span_days: usize,
) -> f64 {
    if test_span_days == 0 {
        return 0.0;
    }
    (n_trades as f64) * 2.0 * notional_usd / (test_span_days as f64) * 30.0
}

/// Every UTC day in the half-open `span` (`[start, end)`, matching the
/// matrix/fold convention elsewhere in this crate) gets an entry, 0.0 by
/// default - a day with no trades is a real zero-return day, not a missing
/// one. Each trade's `net_bps` is booked on the UTC day of its entry
/// timestamp and divided by `n_assets` (equal-weight capital slots: idle
/// slots that didn't trade that day earn 0, they don't vanish from the
/// denominator).
pub fn daily_returns_bps(
    trades: &[Trade],
    n_assets: usize,
    span: (i64, i64),
) -> BTreeMap<NaiveDate, f64> {
    let (start, end) = span;
    let mut by_day: BTreeMap<NaiveDate, f64> = BTreeMap::new();
    if end > start {
        let start_date = date_of(start);
        let last_date = date_of(end - 1);
        let mut day = start_date;
        loop {
            by_day.insert(day, 0.0);
            if day >= last_date {
                break;
            }
            day = day.succ_opt().expect("date within range");
        }
    }

    let slots = n_assets.max(1) as f64;
    for trade in trades {
        let day = date_of(trade.entry_ts);
        *by_day.entry(day).or_insert(0.0) += trade.net_bps / slots;
    }
    by_day
}

/// `mean(daily) / std(daily) * sqrt(365)`, sample standard deviation (n-1),
/// matching the annualization convention `crypto-portfolio`'s own `sharpe`
/// helper uses. `None` when there are fewer than 2 trading days (sample std
/// is undefined) or the series has zero variance (a flat/no-trade series
/// must report "no evidence", not a divide-by-zero NaN or a fabricated 0).
pub fn annualized_sharpe(daily: &BTreeMap<NaiveDate, f64>) -> Option<f64> {
    let n = daily.len();
    if n < 2 {
        return None;
    }
    let n_f = n as f64;
    let mean = daily.values().sum::<f64>() / n_f;
    let var = daily.values().map(|v| (v - mean).powi(2)).sum::<f64>() / (n_f - 1.0);
    let sd = var.sqrt();
    // `sd.is_nan()` guards a variance computation gone wrong (it shouldn't,
    // but a Sharpe silently reporting Some(NaN) would be worse than None).
    if sd.is_nan() || sd <= 0.0 {
        return None;
    }
    Some(mean / sd * 365f64.sqrt())
}

/// Pearson correlation of two equal-length series, `None` when either has
/// zero variance (a flat series has no correlation to report, not a
/// divide-by-zero NaN).
fn pearson(xs: &[f64], ys: &[f64]) -> Option<f64> {
    let n = xs.len() as f64;
    let mean_x = xs.iter().sum::<f64>() / n;
    let mean_y = ys.iter().sum::<f64>() / n;
    let mut cov = 0.0;
    let mut var_x = 0.0;
    let mut var_y = 0.0;
    for (&x, &y) in xs.iter().zip(ys) {
        let dx = x - mean_x;
        let dy = y - mean_y;
        cov += dx * dy;
        var_x += dx * dx;
        var_y += dy * dy;
    }
    if var_x <= 0.0 || var_y <= 0.0 {
        return None;
    }
    Some(cov / (var_x.sqrt() * var_y.sqrt()))
}

/// Average (fractional, tie-averaged) rank of each value, 1-based - the
/// standard input to Spearman's rank correlation.
fn average_ranks(values: &[f64]) -> Vec<f64> {
    let n = values.len();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| values[a].total_cmp(&values[b]));
    let mut ranks = vec![0.0; n];
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j + 1 < n && values[order[j + 1]] == values[order[i]] {
            j += 1;
        }
        // Ranks i..=j (0-based) tie; their shared rank is the average of the
        // 1-based ranks they'd otherwise occupy.
        let avg_rank = (i + j) as f64 / 2.0 + 1.0;
        for &k in &order[i..=j] {
            ranks[k] = avg_rank;
        }
        i = j + 1;
    }
    ranks
}

/// Minimum sample size below which an IC number is noise dressed up as a
/// finding, not a real correlation estimate.
const MIN_IC_PREDS: usize = 30;

/// Pearson correlation of `pred_bps` against realized `fwd_bps` - the
/// simplest "does the model's score track what actually happened" check.
/// `None` under `MIN_IC_PREDS` predictions or zero variance on either side
/// (a flat prediction stream or a flat market has no correlation to
/// report).
pub fn pearson_ic(preds: &[Pred]) -> Option<f64> {
    if preds.len() < MIN_IC_PREDS {
        return None;
    }
    let xs: Vec<f64> = preds.iter().map(|p| p.pred_bps).collect();
    let ys: Vec<f64> = preds.iter().map(|p| p.fwd_bps).collect();
    pearson(&xs, &ys)
}

/// Spearman rank correlation of `pred_bps` against `fwd_bps`: Pearson
/// correlation over average ranks (ties averaged), which cares only that the
/// model's ordering tracks reality, not the scale of its predictions. Same
/// `None` rules as `pearson_ic`.
pub fn rank_ic(preds: &[Pred]) -> Option<f64> {
    if preds.len() < MIN_IC_PREDS {
        return None;
    }
    let xs: Vec<f64> = preds.iter().map(|p| p.pred_bps).collect();
    let ys: Vec<f64> = preds.iter().map(|p| p.fwd_bps).collect();
    pearson(&average_ranks(&xs), &average_ranks(&ys))
}

/// (p50, p90, p99) of `|pred_bps|` across `preds` - how large the model's
/// signals actually get, which is what makes a NO-TRADES report
/// self-explaining next to the entry threshold. `None` when `preds` is
/// empty (there's no distribution to summarize).
pub fn pred_magnitude_quantiles(preds: &[Pred]) -> Option<(f64, f64, f64)> {
    if preds.is_empty() {
        return None;
    }
    let abs_vals: Vec<f64> = preds.iter().map(|p| p.pred_bps.abs()).collect();
    Some((
        percentile(&abs_vals, 0.50),
        percentile(&abs_vals, 0.90),
        percentile(&abs_vals, 0.99),
    ))
}

// ---------------------------------------------------------------------
// CLI driver: everything below touches the filesystem. The pure functions
// above (`load_model`, `simulate`, `daily_returns_bps`, `annualized_sharpe`)
// carry the actual simulation semantics and are exercised directly by the
// tests above; this is just wiring.
// ---------------------------------------------------------------------

/// `folds.json` (Task 4a's output).
#[derive(Debug, Deserialize)]
struct FoldsDocument {
    horizon_min: i64,
    folds: Vec<FoldSpec>,
}

#[derive(Debug, Deserialize)]
struct FoldSpec {
    i: usize,
    train_start_ts: i64,
    train_end_ts: i64,
    test_start_ts: i64,
    test_end_ts: i64,
    model: String,
}

/// Sort `folds` by `test_start_ts` (the order the gate must process them in
/// to carry hold state correctly, whatever order they appear in the file)
/// and check for a REAL ts-window overlap: `next.test_start_ts <
/// prev.test_end_ts`.
///
/// This is deliberately not "no two folds may zero-fill the same calendar
/// day" - a fold's test window almost never starts at UTC midnight (feature
/// warmup eats the first ~65 minutes of the matrix), so every contiguous
/// fold pair shares one boundary calendar day via `daily_returns_bps`'s
/// zero-fill. That sharing is expected and handled by `merge_daily`, not an
/// error condition.
fn validate_no_ts_overlap(folds: &[FoldSpec]) -> Result<Vec<&FoldSpec>, String> {
    let mut ordered: Vec<&FoldSpec> = folds.iter().collect();
    ordered.sort_by_key(|f| f.test_start_ts);
    for pair in ordered.windows(2) {
        let (prev, next) = (pair[0], pair[1]);
        if next.test_start_ts < prev.test_end_ts {
            return Err(format!(
                "fold {} test window [{}, {}) overlaps fold {}'s [{}, {}) - folds.json is not a \
                 valid walk-forward split",
                next.i,
                next.test_start_ts,
                next.test_end_ts,
                prev.i,
                prev.test_start_ts,
                prev.test_end_ts
            ));
        }
    }
    Ok(ordered)
}

/// Fold daily maps into the stitched series ADDITIVELY, not by insertion.
/// Fold prediction windows are disjoint half-open ranges (enforced by
/// `validate_no_ts_overlap`), so a day two folds both zero-fill can only
/// ever contribute `0.0 + 0.0`, `0.0 + trade`, or (if trades from both
/// folds land on the shared boundary day) `trade + trade` - never a
/// double-count of the same trade.
fn merge_daily(stitched: &mut BTreeMap<NaiveDate, f64>, fold_daily: &BTreeMap<NaiveDate, f64>) {
    for (day, bps) in fold_daily {
        *stitched.entry(*day).or_insert(0.0) += *bps;
    }
}

/// The training matrix: manifest feature order, the feature-set version it
/// was built against, and every row's fields split into parallel columns
/// instead of a `Vec<MatrixRow>` of heap-allocated per-row maps - a
/// 2.4M-row x 38-feature matrix as `Vec<MatrixRow>` was observed to SIGKILL
/// (OOM) at 7GB; this columnar form holds the same data in a handful of
/// flat `Vec`s (`x` alone is `n * features.len()` `f64`s, no per-row
/// allocation at all).
///
/// `ts[i]`/`asset_idx[i]`/`x[i*F..(i+1)*F]` are row `i`'s fields, `F =
/// features.len()`, in manifest feature order. `asset_idx[i]` indexes
/// `assets`, an interned vocabulary of asset names in first-seen (file)
/// order - a row's asset is looked up once per distinct string instead of
/// cloned into every row.
///
/// A feature absent from a row's `features` map (deferred to prediction
/// time, exactly like the old `Vec<MatrixRow>` reader: `load_matrix` itself
/// never errors on it) is stored as `f64::NAN` - safe as a "missing"
/// sentinel because the matrix writer (`matrix::matrix_rows`) guarantees
/// every feature it writes is finite, and JSON itself cannot express a
/// literal NaN, so a NAN in `x` can only mean "this row's JSON had no entry
/// for this feature name", never a real value.
///
/// `fwd[horizon][i]` is row `i`'s forward return at that horizon, or
/// `f64::NAN` if that row's `fwd_bps` had no entry for it - same missing
/// sentinel, same justification (`matrix::forward_returns_bps` never writes
/// a fabricated/NaN label; a horizon is a fully absent key or a real
/// number). The horizon key set is discovered from the rows themselves (a
/// key seen on any row gets a column; rows seen before that key first
/// appeared are backfilled with NAN for it), not assumed from the
/// manifest - the manifest's own `horizons_min` doesn't gate which keys a
/// row's `fwd_bps` may carry, so trusting it here could disagree with what
/// `MatrixRow.fwd_bps.get(...)` would have found.
struct Matrix {
    features: Vec<String>,
    feature_set_version: String,
    ts: Vec<i64>,
    asset_idx: Vec<u32>,
    assets: Vec<String>,
    x: Vec<f64>,
    fwd: BTreeMap<String, Vec<f64>>,
}

impl Matrix {
    fn n(&self) -> usize {
        self.ts.len()
    }

    /// Row `i`'s feature values, in `features` (manifest) order - a `NAN`
    /// entry means that feature was absent from the row's JSON.
    fn row_features(&self, i: usize) -> &[f64] {
        let f = self.features.len();
        &self.x[i * f..(i + 1) * f]
    }

    fn asset(&self, i: usize) -> &str {
        &self.assets[self.asset_idx[i] as usize]
    }
}

fn load_matrix(path: &Path) -> Result<Matrix, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);

    let mut manifest_line = String::new();
    std::io::BufRead::read_line(&mut reader, &mut manifest_line)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if manifest_line.trim().is_empty() {
        return Err(format!("{}: empty matrix", path.display()));
    }
    let manifest: serde_json::Value = serde_json::from_str(manifest_line.trim_end())
        .map_err(|e| format!("{}: manifest line: {e}", path.display()))?;
    if manifest.get("kind").and_then(|v| v.as_str()) != Some("manifest") {
        return Err(format!(
            "{}: first line is not the feature manifest",
            path.display()
        ));
    }
    let features: Vec<String> = manifest
        .get("features")
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("{}: manifest has no features array", path.display()))?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{}: feature name not a string", path.display()))
        })
        .collect::<Result<_, _>>()?;
    let feature_set_version = manifest
        .get("feature_set_version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("{}: manifest has no feature_set_version", path.display()))?
        .to_string();
    let n_features = features.len();

    let mut ts: Vec<i64> = Vec::new();
    let mut asset_idx: Vec<u32> = Vec::new();
    let mut assets: Vec<String> = Vec::new();
    let mut asset_lookup: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut x: Vec<f64> = Vec::new();
    let mut fwd: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut n: usize = 0;

    for line in std::io::BufRead::lines(reader) {
        let line = line.map_err(|e| format!("{}: {e}", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        // A transient per-line parse: `MatrixRow`'s `BTreeMap`s are
        // allocated and dropped once per iteration, never retained past the
        // column-unpacking below - the whole point of streaming instead of
        // `Vec<MatrixRow>`.
        let row: MatrixRow = serde_json::from_str(&line)
            .map_err(|e| format!("{}: row: {e}", path.display()))?;

        ts.push(row.ts);
        let idx = *asset_lookup.entry(row.asset.clone()).or_insert_with(|| {
            let idx = assets.len() as u32;
            assets.push(row.asset.clone());
            idx
        });
        asset_idx.push(idx);

        for name in &features {
            x.push(row.features.get(name).copied().unwrap_or(f64::NAN));
        }

        for (h, v) in &row.fwd_bps {
            fwd.entry(h.clone())
                .or_insert_with(|| vec![f64::NAN; n])
                .push(*v);
        }
        for col in fwd.values_mut() {
            if col.len() == n {
                col.push(f64::NAN);
            }
        }

        n += 1;
    }
    debug_assert_eq!(x.len(), n * n_features);

    Ok(Matrix {
        features,
        feature_set_version,
        ts,
        asset_idx,
        assets,
        x,
        fwd,
    })
}

/// `round_trip_bps(asset) = 2 * (taker_fee_bps + spread_bps_median / 2 +
/// cross_bps[notional])`. An asset entirely absent from `costs` gets
/// `DEFAULT_ROUND_TRIP_BPS`. An asset present but whose `cross_bps[notional]`
/// is `null` (book too thin to walk) or whose costs summary simply has no
/// entry for `notional` is excluded from simulation and returned separately.
fn compute_round_trip_bps(
    assets: &[String],
    costs: &BTreeMap<String, CostSummary>,
    notional: &str,
) -> (BTreeMap<String, f64>, Vec<String>) {
    let mut round_trip = BTreeMap::new();
    let mut excluded = Vec::new();
    for asset in assets {
        match costs.get(asset) {
            None => {
                round_trip.insert(asset.clone(), DEFAULT_ROUND_TRIP_BPS);
            }
            Some(summary) => match summary.cross_bps.get(notional) {
                Some(Some(cross)) => {
                    let rt = 2.0 * (TAKER_FEE_BPS + summary.spread_bps_median / 2.0 + cross);
                    round_trip.insert(asset.clone(), rt);
                }
                _ => excluded.push(asset.clone()),
            },
        }
    }
    (round_trip, excluded)
}

#[derive(Serialize)]
struct FoldReport {
    i: usize,
    train_span: [i64; 2],
    test_span: [i64; 2],
    n_trades: usize,
    sharpe: Option<f64>,
    ic: Option<f64>,
    rank_ic: Option<f64>,
    pred_abs_p50: Option<f64>,
    pred_abs_p90: Option<f64>,
    pred_abs_p99: Option<f64>,
    n_preds: usize,
}

#[derive(Serialize)]
struct AssetReport {
    n_trades: usize,
    total_net_bps: f64,
    hit_rate: f64,
}

#[derive(Serialize)]
struct OverallReport {
    n_trades: usize,
    sharpe_annualized: Option<f64>,
    gate: String,
    gate_threshold: f64,
    /// Pearson IC over test-window predictions POOLED across every fold -
    /// and every fold is a DIFFERENT model (walk-forward refits per fold).
    /// This is deliberately a walk-forward-aggregate skill estimate, not any
    /// single model's skill: a reader wanting "how good is fold N's model"
    /// wants `FoldReport::ic`, not this field.
    ic: Option<f64>,
    /// Same pooling caveat as `ic`: Spearman rank IC over ALL folds' test
    /// predictions combined, not one model's. See `FoldReport::rank_ic` for
    /// the per-model number.
    rank_ic: Option<f64>,
    /// `threshold_mult * round_trip_bps` per asset. Under `--costs` this is
    /// `threshold_mult` times the exact flat per-asset round-trip
    /// `compute_round_trip_bps` produced - bit-for-bit what it always was,
    /// not derived from `round_trip_by_asset_day`'s day-keyed copies of that
    /// same number (summing N identical floats and dividing back by N can
    /// drift by ULPs off the direct value, which would have been a silent
    /// regression for this field). Under `--binance-costs` the round-trip
    /// cost genuinely varies day to day, so there IS no single exact number;
    /// this reports the MEAN round-trip across every day that asset actually
    /// had a resolved cost instead - a representative number for a
    /// NO-TRADES report to explain itself against, not the exact threshold
    /// on any one day (see the per-trade `daily_returns_bps` for what
    /// actually got charged).
    threshold_bps_by_asset: BTreeMap<String, f64>,
    /// `total_trades * 2 * notional / test_span_days * 30` - the volume this
    /// trade rate implies over 30 days, which is what the protocol's fee
    /// fixed-point rule (Task 4) maps to a VIP tier.
    projected_30d_volume_usd: f64,
    /// The one-way taker fee bps actually charged on each leg: the
    /// `--binance-costs` path's `--fee-taker-bps`, or the `--costs` path's
    /// fixed `TAKER_FEE_BPS` constant. `None` only if a future cost source
    /// adds a mode that doesn't fit either shape.
    fee_bps_used: Option<f64>,
    /// `"atr"` under `--exit atr` (Amendment 3); omitted entirely under
    /// `--exit time` (the default) - that omission is deliberate and load
    /// bearing: `--exit time`'s report must stay byte-identical to every
    /// gate run before Amendment 3, which never had this field at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_mode: Option<String>,
}

#[derive(Serialize)]
struct Report {
    generated_utc: String,
    matrix: String,
    /// Set when run against the plan-2 flat cost summary (`--costs`); `None`
    /// under `--binance-costs`.
    costs: Option<String>,
    /// Set when run against time-varying Binance costs (`--binance-costs`);
    /// `None` under `--costs`.
    binance_costs: Option<String>,
    horizon_min: i64,
    threshold_mult: f64,
    notional: String,
    /// Recorded from `--fee-taker-bps`; `None` under the plan-2 `--costs`
    /// path, which uses its own fixed `TAKER_FEE_BPS` instead.
    fee_taker_bps: Option<f64>,
    /// Recorded from `--fee-maker-bps` for the protocol's future use -
    /// UNUSED by today's simulation, which charges the taker fee on both
    /// legs of every trade regardless of this value.
    fee_maker_bps: Option<f64>,
    folds: Vec<FoldReport>,
    per_asset: BTreeMap<String, AssetReport>,
    /// Populated only under `--costs`: assets whose plan-2 cost summary has
    /// no fillable `cross_bps` at all for `--notional`, excluded from every
    /// day rather than day-by-day (`--binance-costs`'s finer-grained
    /// equivalent is `days_without_costs` below).
    excluded_thin_books: Vec<String>,
    /// Populated only under `--binance-costs`: per asset, how many calendar
    /// days among its entry-day predictions had no resolvable round-trip
    /// cost (missing or thin-book, even after the 14-day prior-day
    /// fallback) - those predictions were dropped from simulation, not
    /// silently priced at a default.
    days_without_costs: BTreeMap<String, usize>,
    daily_returns_bps: BTreeMap<String, f64>,
    /// Populated only under `--exit atr` (Amendment 3): summed across every
    /// fold. Omitted entirely under `--exit time` - same byte-identity
    /// reasoning as `OverallReport::exit_mode`.
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_stats: Option<ExitStatsReport>,
    overall: OverallReport,
}

#[derive(Serialize)]
struct ExitStatsReport {
    stops: usize,
    targets: usize,
    time_exits: usize,
    /// Mean `bars_held` over `stops + targets + time_exits` trades; `0.0`
    /// when that count is `0` (nothing traded, not a divide-by-zero NaN).
    mean_bars_held: f64,
    /// Accepted entries skipped because the asset had no ATR value yet at
    /// the entry bar (cold warm-up or right after a >120s gap) - see
    /// `ExitStatsAccum::skipped_no_atr`.
    skipped_no_atr: usize,
}

pub fn cmd_gate(args: &[String]) -> Result<(), String> {
    let matrix_path = PathBuf::from(need(args, "--matrix")?);
    let folds_path = PathBuf::from(need(args, "--folds")?);
    let out_path = PathBuf::from(need(args, "--out")?);
    let threshold_mult: f64 = match get(args, "--threshold-mult") {
        Some(v) => v
            .parse()
            .map_err(|e| format!("bad --threshold-mult: {e}"))?,
        None => DEFAULT_THRESHOLD_MULT,
    };
    let notional = get(args, "--notional").unwrap_or_else(|| DEFAULT_NOTIONAL.to_string());
    let notional_usd: f64 = notional
        .parse()
        .map_err(|e| format!("bad --notional {notional:?}: {e}"))?;

    // `--exit time` (default) is Amendment 1's unchanged fixed-horizon
    // exit, byte-identical to every gate run before Amendment 3. `--exit
    // atr` is Amendment 3's pre-registered ATR stop/target, which needs
    // `--data-root` to read each asset's 1m bars for exit resolution -
    // `--matrix`/`--folds`/costs alone aren't enough, those never carried
    // OHLC.
    let exit_mode = get(args, "--exit").unwrap_or_else(|| "time".to_string());
    if exit_mode != "time" && exit_mode != "atr" {
        return Err(format!(
            "--exit must be \"time\" or \"atr\", got {exit_mode:?}"
        ));
    }
    let data_root: Option<PathBuf> = get(args, "--data-root").map(PathBuf::from);
    if exit_mode == "atr" && data_root.is_none() {
        return Err("--data-root is required with --exit atr (to read 1m bars for stop/target \
                     resolution)"
            .into());
    }

    let costs_path = get(args, "--costs").map(PathBuf::from);
    let binance_costs_path = get(args, "--binance-costs").map(PathBuf::from);
    match (&costs_path, &binance_costs_path) {
        (Some(_), Some(_)) => {
            return Err("--costs and --binance-costs are mutually exclusive".into())
        }
        (None, None) => return Err("one of --costs or --binance-costs is required".into()),
        _ => {}
    }

    // `--fee-maker-bps` is unused by today's taker-only simulation - see the
    // `Report::fee_maker_bps` doc comment - but is still required alongside
    // `--fee-taker-bps` under `--binance-costs` so the report always records
    // what the protocol's future maker-aware simulation would have used.
    let fee_taker_bps: Option<f64> = match get(args, "--fee-taker-bps") {
        Some(v) => Some(v.parse().map_err(|e| format!("bad --fee-taker-bps: {e}"))?),
        None => None,
    };
    let fee_maker_bps: Option<f64> = match get(args, "--fee-maker-bps") {
        Some(v) => Some(v.parse().map_err(|e| format!("bad --fee-maker-bps: {e}"))?),
        None => None,
    };
    if binance_costs_path.is_some() {
        if fee_taker_bps.is_none() {
            return Err("--fee-taker-bps is required with --binance-costs".into());
        }
        if fee_maker_bps.is_none() {
            return Err("--fee-maker-bps is required with --binance-costs".into());
        }
    }

    let matrix = load_matrix(&matrix_path)?;
    let folds_doc: FoldsDocument = {
        let text = std::fs::read_to_string(&folds_path)
            .map_err(|e| format!("{}: {e}", folds_path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", folds_path.display()))?
    };
    let folds_dir = folds_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let horizon_key = folds_doc.horizon_min.to_string();
    let assets: Vec<String> = matrix
        .assets
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    // Chronological order (not file order) is what makes the hold-state
    // carry below correct, and doubles as the real-overlap check.
    let ordered_folds = validate_no_ts_overlap(&folds_doc.folds)?;

    // `--exit atr` only: one asset's 1m bars + ATR(14) + ts index, read
    // once up front (not per fold) - same store key rule `training-matrix`
    // uses (`coin.to_uppercase()`), since the matrix's `asset` column is
    // the universe coin name, not the store key.
    let mut asset_paths: BTreeMap<String, AssetPath> = BTreeMap::new();
    if exit_mode == "atr" {
        let perp_root = data_root.as_ref().expect("checked above").join("perp");
        for asset in &assets {
            let store_key = asset.to_uppercase();
            let bars = crypto_portfolio::store::read_asset(&perp_root, 60, &store_key)?;
            if !bars.is_empty() {
                asset_paths.insert(asset.clone(), AssetPath::new(bars));
            }
        }
    }

    // Every calendar day any fold's test window could produce an entry on -
    // the full domain the per-(asset, day) round-trip cost lookup below
    // needs to cover, independent of which folds actually end up trading.
    let days_span: Vec<NaiveDate> = match (ordered_folds.first(), ordered_folds.last()) {
        (Some(first), Some(last)) => binance_micro::days(
            DateTime::<Utc>::from_timestamp(first.test_start_ts, 0).expect("fold ts in range"),
            DateTime::<Utc>::from_timestamp(last.test_end_ts, 0).expect("fold ts in range"),
        ),
        _ => Vec::new(),
    };

    // Both cost sources ultimately reduce to the same shape, `asset ->
    // (entry day -> round_trip_bps)`: the plan-2 `--costs` path's flat
    // per-asset number is just that same value repeated across every day in
    // `days_span` - see `simulate_with_state_by_day`'s doc comment for why
    // one simulation code path can serve both from here on.
    let mut round_trip_by_asset_day: BTreeMap<String, BTreeMap<NaiveDate, f64>> = BTreeMap::new();
    let mut excluded_thin_books: Vec<String> = Vec::new();
    let fee_bps_used: Option<f64>;
    let costs_path_str: Option<String>;
    let binance_costs_path_str: Option<String>;
    // Whether a per-asset day is untradeable due to cost data is only a
    // meaningful per-day diagnostic under `--binance-costs` (see
    // `Report::days_without_costs`'s doc comment) - `--costs`'s flat number
    // is either tradable every day or excluded outright
    // (`excluded_thin_books`), so this stays `false` and `days_without_costs`
    // stays empty for that path.
    let is_binance_mode = binance_costs_path.is_some();
    // `threshold_mult * round_trip_bps` per asset, computed from whichever
    // cost source is exact for that path - see `OverallReport
    // ::threshold_bps_by_asset`'s doc comment for why this can't just be
    // derived from `round_trip_by_asset_day` uniformly (a mean-of-N-copies
    // of the plan-2 flat cost would drift by ULPs off the direct value).
    let threshold_bps_by_asset: BTreeMap<String, f64>;

    if let Some(costs_path) = &costs_path {
        let costs: BTreeMap<String, CostSummary> = {
            let text = std::fs::read_to_string(costs_path)
                .map_err(|e| format!("{}: {e}", costs_path.display()))?;
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", costs_path.display()))?
        };
        let (flat_round_trip, excluded) = compute_round_trip_bps(&assets, &costs, &notional);
        excluded_thin_books = excluded;
        threshold_bps_by_asset = flat_round_trip
            .iter()
            .map(|(asset, rt)| (asset.clone(), threshold_mult * rt))
            .collect();
        for (asset, rt) in &flat_round_trip {
            let by_day: BTreeMap<NaiveDate, f64> = days_span.iter().map(|&d| (d, *rt)).collect();
            if !by_day.is_empty() {
                round_trip_by_asset_day.insert(asset.clone(), by_day);
            }
        }
        fee_bps_used = Some(TAKER_FEE_BPS);
        costs_path_str = Some(costs_path.display().to_string());
        binance_costs_path_str = None;
    } else {
        let binance_costs_path = binance_costs_path.as_ref().expect("checked above");
        let binance_costs: BTreeMap<String, BTreeMap<NaiveDate, DayCost>> = {
            let text = std::fs::read_to_string(binance_costs_path)
                .map_err(|e| format!("{}: {e}", binance_costs_path.display()))?;
            let raw: BTreeMap<String, BTreeMap<String, DayCost>> = serde_json::from_str(&text)
                .map_err(|e| format!("{}: {e}", binance_costs_path.display()))?;
            raw.into_iter()
                .map(|(asset, by_day)| {
                    let parsed: Result<BTreeMap<NaiveDate, DayCost>, String> = by_day
                        .into_iter()
                        .map(|(day, dc)| {
                            NaiveDate::parse_from_str(&day, "%Y-%m-%d")
                                .map(|d| (d, dc))
                                .map_err(|e| {
                                    format!(
                                        "{}: bad day {day:?}: {e}",
                                        binance_costs_path.display()
                                    )
                                })
                        })
                        .collect();
                    parsed.map(|by_day| (asset, by_day))
                })
                .collect::<Result<_, _>>()?
        };
        let fee_taker = fee_taker_bps.expect("checked above");
        for asset in &assets {
            let day_costs = binance_costs.get(asset);
            let mut by_day = BTreeMap::new();
            for &day in &days_span {
                if let Some(rt) = resolve_round_trip(day_costs, day, fee_taker) {
                    by_day.insert(day, rt);
                }
            }
            if !by_day.is_empty() {
                round_trip_by_asset_day.insert(asset.clone(), by_day);
            }
        }
        threshold_bps_by_asset = round_trip_by_asset_day
            .iter()
            .map(|(asset, by_day)| {
                let mean = by_day.values().sum::<f64>() / by_day.len() as f64;
                (asset.clone(), threshold_mult * mean)
            })
            .collect();
        fee_bps_used = Some(fee_taker);
        costs_path_str = None;
        binance_costs_path_str = Some(binance_costs_path.display().to_string());
    }

    let n_assets = round_trip_by_asset_day.len();
    if n_assets == 0 {
        return Err(
            "no tradable assets: every matrix asset is thin-book excluded or has no cost data \
             for --notional"
                .into(),
        );
    }

    let mut fold_reports = Vec::with_capacity(ordered_folds.len());
    let mut all_trades: Vec<Trade> = Vec::new();
    let mut all_preds: Vec<Pred> = Vec::new();
    let mut stitched_daily: BTreeMap<NaiveDate, f64> = BTreeMap::new();
    // Per-asset "flat again at this ts", carried across fold seams so a
    // position opened near the end of fold k can't be silently re-opened by
    // fold k+1's predictions inside the same hold window - the live bot has
    // one continuous position per asset, not one that forgets on every
    // fold's first row.
    let mut hold_state: BTreeMap<String, i64> = BTreeMap::new();
    // `--exit atr` only: summed across every fold into `Report::exit_stats`.
    let mut exit_stats_total = ExitStatsAccum::default();

    for fold in &ordered_folds {
        let model_path = folds_dir.join(&fold.model);
        let trees = load_model(&model_path, &matrix.features, &matrix.feature_set_version)?;

        let mut preds: Vec<Pred> = Vec::new();
        // `None` (no row in the whole matrix ever carried this horizon key)
        // means every row's lookup would have missed too - skip the fold's
        // test window entirely rather than indexing a column that isn't
        // there.
        let fwd_col = matrix.fwd.get(&horizon_key);
        if let Some(fwd_col) = fwd_col {
            for i in 0..matrix.n() {
                let ts = matrix.ts[i];
                if !(ts >= fold.test_start_ts && ts < fold.test_end_ts) {
                    continue;
                }
                let fwd_bps = fwd_col[i];
                if fwd_bps.is_nan() {
                    continue; // no label at this horizon for this row
                }
                let row_feats = matrix.row_features(i);
                let values: Vec<f64> = matrix
                    .features
                    .iter()
                    .zip(row_feats)
                    .map(|(name, &v)| {
                        if v.is_nan() {
                            Err(format!(
                                "{}: row ts={} asset={} missing feature {name:?}",
                                matrix_path.display(),
                                ts,
                                matrix.asset(i)
                            ))
                        } else {
                            Ok(v)
                        }
                    })
                    .collect::<Result<_, _>>()?;
                let pred_bps = lightgbm_json::predict(&trees, &values)?;
                preds.push(Pred {
                    ts,
                    asset: matrix.asset(i).to_string(),
                    pred_bps,
                    fwd_bps,
                });
            }
        }

        let (trades, next_state) = if exit_mode == "atr" {
            let (trades, next_state, stats) = simulate_with_state_atr(
                &preds,
                &round_trip_by_asset_day,
                &asset_paths,
                folds_doc.horizon_min,
                threshold_mult,
                &hold_state,
            );
            exit_stats_total.stops += stats.stops;
            exit_stats_total.targets += stats.targets;
            exit_stats_total.time_exits += stats.time_exits;
            exit_stats_total.bars_held_sum += stats.bars_held_sum;
            exit_stats_total.skipped_no_atr += stats.skipped_no_atr;
            (trades, next_state)
        } else {
            simulate_with_state_by_day(
                &preds,
                &round_trip_by_asset_day,
                folds_doc.horizon_min,
                threshold_mult,
                &hold_state,
            )
        };
        hold_state = next_state;
        let daily = daily_returns_bps(&trades, n_assets, (fold.test_start_ts, fold.test_end_ts));
        let sharpe = annualized_sharpe(&daily);

        let (pred_abs_p50, pred_abs_p90, pred_abs_p99) = match pred_magnitude_quantiles(&preds) {
            Some((p50, p90, p99)) => (Some(p50), Some(p90), Some(p99)),
            None => (None, None, None),
        };

        fold_reports.push(FoldReport {
            i: fold.i,
            train_span: [fold.train_start_ts, fold.train_end_ts],
            test_span: [fold.test_start_ts, fold.test_end_ts],
            n_trades: trades.len(),
            sharpe,
            ic: pearson_ic(&preds),
            rank_ic: rank_ic(&preds),
            pred_abs_p50,
            pred_abs_p90,
            pred_abs_p99,
            n_preds: preds.len(),
        });

        merge_daily(&mut stitched_daily, &daily);
        all_trades.extend(trades);
        // Pooled deliberately: `all_preds` feeds `overall.ic`/`overall.rank_ic`
        // below, a walk-forward-aggregate skill estimate across every fold's
        // DISTINCT model - not one model's skill. `FoldReport::ic`/`rank_ic`
        // above are where a single model's skill lives.
        all_preds.extend(preds);
    }

    // Per asset, how many DISTINCT calendar days among its actual
    // predictions had no resolvable round-trip cost (dropped by
    // `simulate_with_state_by_day`'s lookup, not merely a day the asset
    // never had a row on at all) - the useful diagnostic per
    // `Report::days_without_costs`'s doc comment. Computed from `all_preds`
    // rather than swept over `days_span`, and only under `--binance-costs`:
    // under `--costs` every tradable asset's flat cost covers every day in
    // `days_span` by construction, so this would otherwise stay empty
    // anyway, but skipping it avoids miscounting an entirely-excluded
    // thin-book asset's preds as "days without costs" when
    // `excluded_thin_books` already reports it.
    let mut days_without_costs: BTreeMap<String, usize> = BTreeMap::new();
    if is_binance_mode {
        let mut dropped_days: BTreeMap<String, BTreeSet<NaiveDate>> = BTreeMap::new();
        for p in &all_preds {
            let day = date_of(p.ts);
            let has_cost = round_trip_by_asset_day
                .get(p.asset.as_str())
                .is_some_and(|by_day| by_day.contains_key(&day));
            if !has_cost {
                dropped_days.entry(p.asset.clone()).or_default().insert(day);
            }
        }
        days_without_costs = dropped_days
            .into_iter()
            .map(|(asset, days)| (asset, days.len()))
            .collect();
    }

    let mut per_asset_acc: BTreeMap<String, (usize, f64, usize)> = BTreeMap::new();
    for trade in &all_trades {
        let entry = per_asset_acc.entry(trade.asset.clone()).or_default();
        entry.0 += 1;
        entry.1 += trade.net_bps;
        if trade.net_bps > 0.0 {
            entry.2 += 1;
        }
    }
    let per_asset: BTreeMap<String, AssetReport> = per_asset_acc
        .into_iter()
        .map(|(asset, (n_trades, total_net_bps, wins))| {
            (
                asset,
                AssetReport {
                    n_trades,
                    total_net_bps,
                    hit_rate: wins as f64 / n_trades as f64,
                },
            )
        })
        .collect();

    let overall_sharpe = annualized_sharpe(&stitched_daily);
    let n_trades_total = all_trades.len();
    let gate = if n_trades_total == 0 {
        "NO-TRADES"
    } else {
        match overall_sharpe {
            Some(s) if s > GATE_THRESHOLD => "PASS",
            _ => "FAIL",
        }
    };

    let test_span_days = stitched_daily.len();

    let exit_stats_report = if exit_mode == "atr" {
        let n = exit_stats_total.stops + exit_stats_total.targets + exit_stats_total.time_exits;
        Some(ExitStatsReport {
            stops: exit_stats_total.stops,
            targets: exit_stats_total.targets,
            time_exits: exit_stats_total.time_exits,
            mean_bars_held: if n > 0 {
                exit_stats_total.bars_held_sum as f64 / n as f64
            } else {
                0.0
            },
            skipped_no_atr: exit_stats_total.skipped_no_atr,
        })
    } else {
        None
    };
    let exit_mode_field = if exit_mode == "atr" {
        Some(exit_mode.clone())
    } else {
        None
    };

    let report = Report {
        generated_utc: Utc::now().to_rfc3339(),
        matrix: matrix_path.display().to_string(),
        costs: costs_path_str,
        binance_costs: binance_costs_path_str,
        horizon_min: folds_doc.horizon_min,
        threshold_mult,
        notional,
        fee_taker_bps,
        fee_maker_bps,
        folds: fold_reports,
        per_asset,
        excluded_thin_books,
        days_without_costs,
        daily_returns_bps: stitched_daily
            .into_iter()
            .map(|(day, bps)| (day.to_string(), bps))
            .collect(),
        exit_stats: exit_stats_report,
        overall: OverallReport {
            n_trades: n_trades_total,
            sharpe_annualized: overall_sharpe,
            gate: gate.to_string(),
            gate_threshold: GATE_THRESHOLD,
            ic: pearson_ic(&all_preds),
            rank_ic: rank_ic(&all_preds),
            threshold_bps_by_asset,
            projected_30d_volume_usd: projected_30d_volume_usd(
                n_trades_total,
                notional_usd,
                test_span_days,
            ),
            fee_bps_used,
            exit_mode: exit_mode_field,
        },
    };

    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::write(&out_path, json).map_err(|e| format!("{}: {e}", out_path.display()))?;
    println!(
        "{} fold(s), {n_trades_total} trade(s), gate={gate}, sharpe={:?} -> {}",
        report.folds.len(),
        report.overall.sharpe_annualized,
        out_path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pred(asset: &str, ts: i64, p: f64, f: f64) -> Pred {
        Pred {
            ts,
            asset: asset.to_string(),
            pred_bps: p,
            fwd_bps: f,
        }
    }

    #[test]
    fn parity_with_the_python_booster() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/parity");
        let art: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(format!("{dir}/model.json")).unwrap())
                .unwrap();
        let features: Vec<String> = art["features"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        let feature_set_version = art["feature_set_version"].as_str().unwrap();
        let trees = load_model(
            Path::new(&format!("{dir}/model.json")),
            &features,
            feature_set_version,
        )
        .unwrap();
        let expected: Vec<f64> =
            serde_json::from_str(&std::fs::read_to_string(format!("{dir}/expected.json")).unwrap())
                .unwrap();
        for (i, line) in std::fs::read_to_string(format!("{dir}/rows.jsonl"))
            .unwrap()
            .lines()
            .enumerate()
        {
            let row: serde_json::Value = serde_json::from_str(line).unwrap();
            let values: Vec<f64> = features
                .iter()
                .map(|n| row["features"][n].as_f64().unwrap())
                .collect();
            let got = lightgbm_json::predict(&trees, &values).unwrap();
            assert!(
                (got - expected[i]).abs() < 1e-9,
                "row {i}: {got} vs {}",
                expected[i]
            );
        }
    }

    #[test]
    fn costs_gate_trades_out() {
        // |pred| = 30 < 1.5 x 25 -> nothing trades.
        let preds: Vec<Pred> = (0..20)
            .map(|i| pred("A", i * 300, if i % 2 == 0 { 30.0 } else { -30.0 }, 5.0))
            .collect();
        let costs = BTreeMap::from([("A".to_string(), 25.0)]);
        assert!(simulate(&preds, &costs, 30, 1.5).is_empty());
    }

    #[test]
    fn the_hold_window_blocks_overlapping_trades() {
        let preds: Vec<Pred> = (0..24).map(|i| pred("A", i * 300, 100.0, 8.0)).collect();
        let costs = BTreeMap::from([("A".to_string(), 10.0)]);
        let trades = simulate(&preds, &costs, 30, 1.5);
        assert!(!trades.is_empty());
        let entries: Vec<i64> = trades.iter().map(|t| t.entry_ts).collect();
        assert!(entries.windows(2).all(|w| w[1] - w[0] >= 30 * 60));
    }

    #[test]
    fn shorts_earn_the_negated_move_and_both_sides_pay_costs() {
        let preds = vec![pred("A", 0, -50.0, -20.0)]; // predicts down 50, market fell 20
        let costs = BTreeMap::from([("A".to_string(), 10.0)]);
        let t = &simulate(&preds, &costs, 30, 1.5)[0];
        assert_eq!(t.side, -1);
        assert!((t.net_bps - (20.0 - 10.0)).abs() < 1e-9);
    }

    #[test]
    fn no_trade_days_count_as_zero_and_flat_series_has_no_sharpe() {
        let day = 86_400i64;
        let trades = vec![Trade {
            asset: "A".into(),
            entry_ts: 3 * day + 60,
            side: 1,
            net_bps: 12.0,
        }];
        let daily = daily_returns_bps(&trades, 2, (0, 10 * day));
        assert_eq!(daily.len(), 10);
        assert_eq!(daily.values().filter(|v| **v == 0.0).count(), 9);
        let booked: f64 = *daily.values().find(|v| **v != 0.0).unwrap();
        assert!(
            (booked - 6.0).abs() < 1e-9,
            "12bps over 2 equal-weight slots"
        );
        let flat: BTreeMap<_, _> = daily.keys().map(|d| (*d, 0.0)).collect();
        assert!(annualized_sharpe(&flat).is_none());
    }

    fn fold_spec(i: usize, test_start_ts: i64, test_end_ts: i64) -> FoldSpec {
        FoldSpec {
            i,
            train_start_ts: 0,
            train_end_ts: test_start_ts,
            test_start_ts,
            test_end_ts,
            model: format!("fold-{i}.json"),
        }
    }

    /// Real folds are never midnight-aligned (feature warmup eats the
    /// matrix's first ~65 minutes), so a contiguous fold pair's calendar
    /// days always share one boundary day via `daily_returns_bps`'s
    /// zero-fill. Stitching must sum that shared day, not treat it as an
    /// overlap error.
    #[test]
    fn stitching_sums_across_a_midday_fold_boundary() {
        let day = 86_400i64;
        let boundary = (5 * day) / 2; // 2.5 days - lands mid-day, not at midnight
        let folds = [fold_spec(0, 0, boundary), fold_spec(1, boundary, 5 * day)];

        let ordered = validate_no_ts_overlap(&folds)
            .expect("contiguous half-open windows must not be flagged as overlapping");
        assert_eq!(ordered.len(), 2);

        // Both trades land on day 2 (the shared boundary day): one from
        // each fold's window.
        let trade_a = Trade {
            asset: "A".into(),
            entry_ts: 200_000,
            side: 1,
            net_bps: 7.0,
        };
        let trade_b = Trade {
            asset: "A".into(),
            entry_ts: 220_000,
            side: 1,
            net_bps: 11.0,
        };
        assert_eq!(date_of(trade_a.entry_ts), date_of(trade_b.entry_ts));

        let daily_a = daily_returns_bps(std::slice::from_ref(&trade_a), 1, (0, boundary));
        let daily_b = daily_returns_bps(std::slice::from_ref(&trade_b), 1, (boundary, 5 * day));

        let mut stitched: BTreeMap<NaiveDate, f64> = BTreeMap::new();
        merge_daily(&mut stitched, &daily_a);
        merge_daily(&mut stitched, &daily_b);

        // Days 0..4 inclusive - one entry per calendar day, not one per
        // fold's zero-filled span (which would double-count the boundary).
        assert_eq!(stitched.len(), 5);
        let boundary_day = date_of(trade_a.entry_ts);
        assert!(
            (stitched[&boundary_day] - (trade_a.net_bps + trade_b.net_bps)).abs() < 1e-9,
            "boundary day must sum both folds' contributions, got {}",
            stitched[&boundary_day]
        );
    }

    #[test]
    fn genuine_ts_overlap_between_folds_still_errors() {
        let day = 86_400i64;
        // fold_b starts well before fold_a's test window ends - a real
        // overlap, not just a shared calendar day.
        let folds = [
            fold_spec(0, 0, (5 * day) / 2),
            fold_spec(1, 200_000, 5 * day),
        ];
        assert!(validate_no_ts_overlap(&folds).is_err());
    }

    /// The live bot holds one continuous position per asset; the
    /// walk-forward gate simulating fold-by-fold must not let a position
    /// opened near the end of fold k get silently re-opened by fold k+1's
    /// predictions that fall inside the same hold window.
    #[test]
    fn hold_state_carries_across_a_fold_seam() {
        let e = 100_000i64; // fold k's test_end_ts (arbitrary)
        let costs = BTreeMap::from([("A".to_string(), 10.0)]);

        let preds_k = vec![pred("A", e - 60, 100.0, 8.0)];
        let (trades_k, state) = simulate_with_state(&preds_k, &costs, 30, 1.5, &BTreeMap::new());
        assert_eq!(trades_k.len(), 1);
        assert_eq!(trades_k[0].entry_ts, e - 60);

        let hold_ends = (e - 60) + 30 * 60; // flat again at e + 1740
        let preds_k1 = vec![
            pred("A", e + 300, 1_000.0, 8.0), // still inside the carried hold window
            pred("A", hold_ends + 1, 1_000.0, 8.0), // first eligible entry
        ];
        let (trades_k1, _) = simulate_with_state(&preds_k1, &costs, 30, 1.5, &state);
        assert_eq!(
            trades_k1.len(),
            1,
            "the row inside the carried hold window must not re-enter"
        );
        assert_eq!(trades_k1[0].entry_ts, hold_ends + 1);
    }

    #[test]
    fn ic_matches_a_hand_computed_correlation() {
        // pred = 2·fwd exactly -> both ICs are 1; anti-correlated -> -1.
        let mk = |sign: f64| -> Vec<Pred> {
            (0..40)
                .map(|i| {
                    let f = (i as f64) - 20.0 + ((i % 3) as f64) * 0.1;
                    pred("A", i * 60, sign * 2.0 * f, f)
                })
                .collect()
        };
        assert!((pearson_ic(&mk(1.0)).unwrap() - 1.0).abs() < 1e-9);
        assert!((pearson_ic(&mk(-1.0)).unwrap() + 1.0).abs() < 1e-9);
        assert!((rank_ic(&mk(1.0)).unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn rank_ic_ignores_scale_but_pearson_does_not() {
        // Monotone but convex mapping: rank IC stays 1, Pearson dips below 1.
        let preds: Vec<Pred> = (0..40)
            .map(|i| {
                let f = i as f64;
                pred("A", i * 60, f * f, f)
            })
            .collect();
        assert!((rank_ic(&preds).unwrap() - 1.0).abs() < 1e-9);
        assert!(pearson_ic(&preds).unwrap() < 0.999);
    }

    #[test]
    fn too_few_or_degenerate_preds_yield_no_ic() {
        let few: Vec<Pred> = (0..10).map(|i| pred("A", i * 60, 1.0, 1.0)).collect();
        assert!(pearson_ic(&few).is_none(), "under 30 preds");
        let flat: Vec<Pred> = (0..40).map(|i| pred("A", i * 60, 5.0, i as f64)).collect();
        assert!(pearson_ic(&flat).is_none(), "zero pred variance");
    }

    #[test]
    fn magnitude_quantiles_are_ordered() {
        let preds: Vec<Pred> = (0..100)
            .map(|i| pred("A", i * 60, (i as f64) - 50.0, 0.0))
            .collect();
        let (p50, p90, p99) = pred_magnitude_quantiles(&preds).unwrap();
        assert!(p50 <= p90 && p90 <= p99);
        assert!(p50 > 0.0);
    }

    #[test]
    fn a_feature_set_version_mismatch_is_refused() {
        // Build a minimal artifact JSON on disk with feature_set_version
        // "fs-other" and call load_model against expected version
        // "fs-rust-scalper-1" (the matrix manifest's) - the error must name
        // both versions.
        let path = std::env::temp_dir().join(format!(
            "scalper-gate-version-mismatch-{}.json",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"{
                "format_version": "crypto-lightgbm-json-1",
                "feature_set_version": "fs-other",
                "features": ["f0"],
                "model": { "tree_info": [] }
            }"#,
        )
        .unwrap();

        let err = load_model(&path, &["f0".to_string()], "fs-rust-scalper-1").unwrap_err();

        std::fs::remove_file(&path).ok();

        assert!(
            err.contains("fs-other") && err.contains("fs-rust-scalper-1"),
            "expected the error to name both versions, got: {err}"
        );
    }

    /// The version cross-check proven end-to-end through `cmd_gate` itself
    /// (not just `load_model` in isolation): a REAL fs-1-versioned model
    /// artifact - fs-1's historical `feature_set_version` and its actual
    /// 26-feature order, exactly as they were before fs-2 (later fs-3)
    /// bumped `features_scalper::FEATURE_SET_VERSION` past
    /// `fs-rust-scalper-1` - scored against a matrix built from the current
    /// (fs-3) feature catalog must be refused, not silently scored against
    /// the wrong 26 columns of a 38-column row.
    #[test]
    fn a_stale_fs1_artifact_is_refused_end_to_end_against_an_fs3_matrix() {
        use features_scalper::{FEATURE_NAMES, FEATURE_SET_VERSION};

        let dir = std::env::temp_dir().join(format!(
            "scalper-gate-e2e-version-cross-check-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        // An fs-2 matrix: the manifest and one fully-populated row use the
        // real, current feature catalog (38 names), not a hand-picked
        // stand-in.
        let mut features = serde_json::Map::new();
        for name in FEATURE_NAMES {
            features.insert(name.to_string(), serde_json::json!(1.0));
        }
        let feature_names: Vec<&str> = FEATURE_NAMES.to_vec();
        let manifest = serde_json::json!({
            "kind": "manifest",
            "feature_set_version": FEATURE_SET_VERSION,
            "features": feature_names,
            "horizons_min": [15],
            "stride_min": 1,
            "assets": ["BTC"],
        });
        let row = serde_json::json!({
            "ts": 0,
            "asset": "BTC",
            "features": features,
            "fwd_bps": {"15": 5.0},
        });
        let matrix_path = dir.join("matrix.jsonl");
        std::fs::write(&matrix_path, format!("{manifest}\n{row}\n")).unwrap();

        let folds_path = dir.join("folds.json");
        std::fs::write(
            &folds_path,
            serde_json::to_string(&serde_json::json!({
                "horizon_min": 15,
                "folds": [{
                    "i": 0,
                    "train_start_ts": 0,
                    "train_end_ts": 0,
                    "test_start_ts": 0,
                    "test_end_ts": 3600,
                    "model": "fold-0.json",
                }],
            }))
            .unwrap(),
        )
        .unwrap();

        // Empty costs: the lone asset falls back to DEFAULT_ROUND_TRIP_BPS,
        // so the gate still has a tradable asset to reach the model-loading
        // step - this test is about the version check, not cost lookup.
        let costs_path = dir.join("costs.json");
        std::fs::write(&costs_path, "{}").unwrap();

        const FS1_FEATURES: [&str; 26] = [
            "ret_1",
            "ret_3",
            "ret_5",
            "ret_15",
            "ret_30",
            "ret_60",
            "mom_5",
            "mom_15",
            "mom_30",
            "mom_60",
            "vol_15",
            "vol_60",
            "vol_ratio_15_60",
            "vwap_dist_60",
            "volume_z_60",
            "volume_ratio_5_60",
            "trades_z_60",
            "hl_range",
            "body_frac",
            "upper_wick_frac",
            "lower_wick_frac",
            "tod_sin",
            "tod_cos",
            "dow",
            "btc_ret_5",
            "rel_ret_5",
        ];
        let fs1_artifact = serde_json::json!({
            "format_version": MODEL_FORMAT_VERSION,
            "feature_set_version": "fs-rust-scalper-1",
            "features": FS1_FEATURES,
            "model": { "tree_info": [] },
        });
        std::fs::write(
            dir.join("fold-0.json"),
            serde_json::to_string(&fs1_artifact).unwrap(),
        )
        .unwrap();

        let out_path = dir.join("report.json");
        let args = vec![
            "--matrix".to_string(),
            matrix_path.to_string_lossy().to_string(),
            "--folds".to_string(),
            folds_path.to_string_lossy().to_string(),
            "--costs".to_string(),
            costs_path.to_string_lossy().to_string(),
            "--out".to_string(),
            out_path.to_string_lossy().to_string(),
        ];
        let err = cmd_gate(&args).unwrap_err();

        std::fs::remove_dir_all(&dir).ok();

        assert!(
            err.contains("fs-rust-scalper-1") && err.contains(FEATURE_SET_VERSION),
            "expected the gate to refuse the stale fs-1 artifact against the fs-3 matrix, \
             got: {err}"
        );
    }

    // -- resolve_round_trip ------------------------------------------------

    fn day_cost(spread_p75: f64, impact: Option<f64>) -> DayCost {
        DayCost {
            spread_bps_p75: spread_p75,
            impact_bps: impact,
            samples: 100,
        }
    }

    fn d(offset_days: i64) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 1, 1).unwrap() + chrono::Duration::days(offset_days)
    }

    #[test]
    fn resolve_round_trip_uses_the_exact_day_and_the_pre_registered_formula() {
        let costs = BTreeMap::from([(d(0), day_cost(5.0, Some(2.0)))]);
        // 2*fee_taker + spread_p75 + 2*impact = 2*3.0 + 5.0 + 2*2.0 = 15.0.
        let got = resolve_round_trip(Some(&costs), d(0), 3.0).unwrap();
        assert!((got - 15.0).abs() < 1e-9, "got {got}");
    }

    #[test]
    fn resolve_round_trip_falls_back_to_the_nearest_prior_day_within_14() {
        // No entry for d(5); d(3) is the nearest usable prior day within 14.
        let costs = BTreeMap::from([
            (d(3), day_cost(4.0, Some(1.0))),
            (d(1), day_cost(999.0, Some(999.0))), // further back - must not win
        ]);
        let got = resolve_round_trip(Some(&costs), d(5), 3.0).unwrap();
        // 2*3.0 + 4.0 + 2*1.0 = 12.0, from d(3), not d(1).
        assert!((got - 12.0).abs() < 1e-9, "got {got}");
    }

    #[test]
    fn resolve_round_trip_treats_a_thin_entry_day_as_untradeable_with_no_fallback() {
        // d(5) is the entry day and HAS a DayCost entry, but it's thin
        // (impact None). The pre-registered rule reserves the prior-day
        // walk for a day with NO cost data at all - a measured-and-thin
        // entry day must report untradeable outright, even though a calmer
        // prior day (d(4)) exists right next to it.
        let costs = BTreeMap::from([
            (d(5), day_cost(5.0, None)),
            (d(4), day_cost(6.0, Some(1.0))),
        ]);
        assert!(
            resolve_round_trip(Some(&costs), d(5), 3.0).is_none(),
            "a thin entry day must not fall back to a calmer prior day"
        );
    }

    #[test]
    fn resolve_round_trip_entry_day_absent_still_falls_back_to_an_ok_prior_day() {
        // d(5) has no entry at all (as opposed to a present-but-thin one) -
        // this IS the case the prior-day fallback exists for.
        let costs = BTreeMap::from([(d(3), day_cost(4.0, Some(1.0)))]);
        let got = resolve_round_trip(Some(&costs), d(5), 3.0).unwrap();
        assert!((got - 12.0).abs() < 1e-9, "got {got}"); // 2*3 + 4 + 2*1
    }

    #[test]
    fn resolve_round_trip_falls_back_past_a_thin_prior_day_too() {
        // Entry day d(5) is absent; the nearest prior day d(4) IS present
        // but thin - the walk must not stop there (there's no impact number
        // to price from a thin day either), it must keep going to d(3).
        let costs = BTreeMap::from([
            (d(4), day_cost(5.0, None)),
            (d(3), day_cost(6.0, Some(1.0))),
        ]);
        let got = resolve_round_trip(Some(&costs), d(5), 3.0).unwrap();
        assert!((got - 14.0).abs() < 1e-9, "got {got}"); // 2*3 + 6 + 2*1
    }

    #[test]
    fn resolve_round_trip_gives_up_beyond_14_days_back() {
        let costs = BTreeMap::from([(d(0), day_cost(5.0, Some(2.0)))]);
        assert!(
            resolve_round_trip(Some(&costs), d(14), 3.0).is_some(),
            "exactly 14 days back is still within the window"
        );
        assert!(
            resolve_round_trip(Some(&costs), d(15), 3.0).is_none(),
            "15 days back is outside the window"
        );
    }

    #[test]
    fn resolve_round_trip_is_none_when_the_asset_has_no_cost_data_at_all() {
        assert!(resolve_round_trip(None, d(0), 3.0).is_none());
    }

    // -- projected_30d_volume_usd -------------------------------------------

    #[test]
    fn projected_30d_volume_usd_matches_hand_computed_arithmetic() {
        // 10 trades * 2 legs * $5,000 notional / 5 test-span days * 30 =
        // 100,000 / 5 * 30 = 600,000.
        let got = projected_30d_volume_usd(10, 5_000.0, 5);
        assert!((got - 600_000.0).abs() < 1e-9, "got {got}");
    }

    #[test]
    fn projected_30d_volume_usd_is_zero_not_nan_with_a_zero_span() {
        assert_eq!(projected_30d_volume_usd(10, 5_000.0, 0), 0.0);
    }

    // -- simulate_with_state_by_day ------------------------------------------

    #[test]
    fn simulate_by_day_charges_the_cost_resolved_for_each_preds_own_entry_day() {
        // Same asset, two entries a day apart: day 0's round-trip is cheap
        // (10bps), day 1's is expensive (100bps) - the trade booked on day 1
        // must be charged 100, not day 0's 10, proving the lookup is keyed
        // by entry day and not just by asset.
        let day0_ts = 0i64;
        let day1_ts = 86_400i64 + 1;
        // pred_bps = 200 clears both day 0's threshold (1.5*10=15) and day
        // 1's much higher threshold (1.5*100=150).
        let preds = vec![
            pred("A", day0_ts, 200.0, 20.0),
            pred("A", day1_ts, 200.0, 20.0),
        ];
        let mut by_day = BTreeMap::new();
        by_day.insert(date_of(day0_ts), 10.0);
        by_day.insert(date_of(day1_ts), 100.0);
        let round_trip_by_asset_day = BTreeMap::from([("A".to_string(), by_day)]);

        let trades = simulate_by_day(&preds, &round_trip_by_asset_day, 30, 1.5);
        assert_eq!(trades.len(), 2, "both days clear their own threshold");
        assert!((trades[0].net_bps - (20.0 - 10.0)).abs() < 1e-9);
        assert!((trades[1].net_bps - (20.0 - 100.0)).abs() < 1e-9);
    }

    #[test]
    fn simulate_by_day_drops_preds_on_a_day_with_no_resolved_cost() {
        // Day 0 has a cost entry; day 1 (a different asset day) has none at
        // all - that pred must be skipped, not defaulted to some cost.
        let day0_ts = 0i64;
        let day1_ts = 86_400i64 + 1;
        let preds = vec![
            pred("A", day0_ts, 50.0, 20.0),
            pred("A", day1_ts, 50.0, 20.0),
        ];
        let by_day = BTreeMap::from([(date_of(day0_ts), 10.0)]);
        let round_trip_by_asset_day = BTreeMap::from([("A".to_string(), by_day)]);

        let trades = simulate_by_day(&preds, &round_trip_by_asset_day, 30, 1.5);
        assert_eq!(trades.len(), 1, "only day 0's pred has a resolved cost");
        assert_eq!(trades[0].entry_ts, day0_ts);
    }

    // -- cmd_gate: --binance-costs wiring ------------------------------------

    #[test]
    fn costs_and_binance_costs_flags_are_mutually_exclusive() {
        let args = vec![
            "--matrix".to_string(),
            "does-not-exist.jsonl".to_string(),
            "--folds".to_string(),
            "does-not-exist.json".to_string(),
            "--out".to_string(),
            "does-not-exist-out.json".to_string(),
            "--costs".to_string(),
            "a.json".to_string(),
            "--binance-costs".to_string(),
            "b.json".to_string(),
        ];
        let err = cmd_gate(&args).unwrap_err();
        assert!(err.contains("mutually exclusive"), "got: {err}");
    }

    #[test]
    fn neither_costs_flag_given_is_an_error() {
        let args = vec![
            "--matrix".to_string(),
            "does-not-exist.jsonl".to_string(),
            "--folds".to_string(),
            "does-not-exist.json".to_string(),
            "--out".to_string(),
            "does-not-exist-out.json".to_string(),
        ];
        let err = cmd_gate(&args).unwrap_err();
        assert!(
            err.contains("--costs") && err.contains("--binance-costs"),
            "got: {err}"
        );
    }

    #[test]
    fn binance_costs_requires_both_fee_flags() {
        let args = vec![
            "--matrix".to_string(),
            "does-not-exist.jsonl".to_string(),
            "--folds".to_string(),
            "does-not-exist.json".to_string(),
            "--out".to_string(),
            "does-not-exist-out.json".to_string(),
            "--binance-costs".to_string(),
            "b.json".to_string(),
        ];
        let err = cmd_gate(&args).unwrap_err();
        assert!(err.contains("--fee-taker-bps"), "got: {err}");

        let args_with_taker = {
            let mut a = args.clone();
            a.push("--fee-taker-bps".to_string());
            a.push("3.0".to_string());
            a
        };
        let err2 = cmd_gate(&args_with_taker).unwrap_err();
        assert!(err2.contains("--fee-maker-bps"), "got: {err2}");
    }

    /// Regression for a mean-of-N-identical-copies drift: under `--costs`,
    /// `overall.threshold_bps_by_asset` must be built from the flat
    /// per-asset round-trip directly (bit-exact, the same number this field
    /// always reported before it grew a day dimension underneath it), not
    /// from averaging `round_trip_by_asset_day`'s day-keyed copies of that
    /// same value back down - summing ten copies of a non-round float and
    /// dividing by ten does not always reproduce the float exactly.
    #[test]
    fn threshold_bps_by_asset_is_bit_exact_under_the_costs_path() {
        use features_scalper::{FEATURE_NAMES, FEATURE_SET_VERSION};

        let dir = std::env::temp_dir().join(format!(
            "scalper-gate-threshold-exactness-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        let mut features = serde_json::Map::new();
        for name in FEATURE_NAMES {
            features.insert(name.to_string(), serde_json::json!(1.0));
        }
        let feature_names: Vec<&str> = FEATURE_NAMES.to_vec();
        let manifest = serde_json::json!({
            "kind": "manifest",
            "feature_set_version": FEATURE_SET_VERSION,
            "features": feature_names,
            "horizons_min": [15],
            "stride_min": 1,
            "assets": ["BTC"],
        });
        let row = serde_json::json!({
            "ts": 600, "asset": "BTC", "features": features, "fwd_bps": {"15": 1.0},
        });
        let matrix_path = dir.join("matrix.jsonl");
        std::fs::write(&matrix_path, format!("{manifest}\n{row}\n")).unwrap();

        // A ten-day test window - enough days_span entries that a naive
        // sum-then-divide mean of ten identical floats is likely to drift
        // from the value a single multiply produces.
        let folds_path = dir.join("folds.json");
        std::fs::write(
            &folds_path,
            serde_json::to_string(&serde_json::json!({
                "horizon_min": 15,
                "folds": [{
                    "i": 0, "train_start_ts": 0, "train_end_ts": 0,
                    "test_start_ts": 0, "test_end_ts": 10 * 86_400,
                    "model": "fold-0.json",
                }],
            }))
            .unwrap(),
        )
        .unwrap();

        let model = serde_json::json!({
            "format_version": MODEL_FORMAT_VERSION,
            "feature_set_version": FEATURE_SET_VERSION,
            "features": feature_names,
            "model": { "tree_info": [{
                "tree_index": 0, "num_leaves": 1, "num_cat": 0, "shrinkage": 1.0,
                "tree_structure": { "leaf_value": 0.0 },
            }] },
        });
        std::fs::write(
            dir.join("fold-0.json"),
            serde_json::to_string(&model).unwrap(),
        )
        .unwrap();

        // A deliberately non-round cost summary - the kind of value where
        // ULP drift from a sum-then-divide mean would actually show up.
        let spread_median = 3.333_333_333_333_f64;
        let cross = 1.234_567_891_234_f64;
        let costs_path = dir.join("costs.json");
        std::fs::write(
            &costs_path,
            serde_json::to_string(&serde_json::json!({
                "BTC": {
                    "samples": 10,
                    "spread_bps_median": spread_median,
                    "spread_bps_p75": 4.0,
                    "cross_bps": {"5000": cross},
                    "top_depth_usd_median": 1000.0,
                },
            }))
            .unwrap(),
        )
        .unwrap();

        let out_path = dir.join("report.json");
        let args = vec![
            "--matrix".to_string(),
            matrix_path.to_string_lossy().to_string(),
            "--folds".to_string(),
            folds_path.to_string_lossy().to_string(),
            "--costs".to_string(),
            costs_path.to_string_lossy().to_string(),
            "--out".to_string(),
            out_path.to_string_lossy().to_string(),
        ];
        cmd_gate(&args).unwrap();

        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&out_path).unwrap()).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        let rt = 2.0 * (TAKER_FEE_BPS + spread_median / 2.0 + cross);
        let expected = DEFAULT_THRESHOLD_MULT * rt;
        let got = report["overall"]["threshold_bps_by_asset"]["BTC"]
            .as_f64()
            .unwrap();
        assert_eq!(
            got, expected,
            "the --costs path's threshold must be bit-exact, not a drifted mean"
        );
    }

    /// Full wiring, end to end: a two-row matrix (one asset, two calendar
    /// days a day apart), a single fold spanning both days, and a
    /// `--binance-costs` file with a `DayCost` for day 0 only - day 1 must
    /// fall back to day 0's cost (the pre-registered 14-day rule), both
    /// trades should fire (a single-leaf model predicts the same constant
    /// for every row), and the report's fee/volume-projection fields must
    /// match the hand-computed arithmetic.
    #[test]
    fn cmd_gate_runs_end_to_end_against_binance_costs_with_the_prior_day_fallback() {
        use features_scalper::{FEATURE_NAMES, FEATURE_SET_VERSION};

        let dir = std::env::temp_dir().join(format!(
            "scalper-gate-e2e-binance-costs-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        let mut features = serde_json::Map::new();
        for name in FEATURE_NAMES {
            features.insert(name.to_string(), serde_json::json!(1.0));
        }
        let feature_names: Vec<&str> = FEATURE_NAMES.to_vec();
        let manifest = serde_json::json!({
            "kind": "manifest",
            "feature_set_version": FEATURE_SET_VERSION,
            "features": feature_names,
            "horizons_min": [15],
            "stride_min": 1,
            "assets": ["BTC"],
        });
        // Row A: day 0, 00:10 UTC. Row B: day 1, 00:10 UTC - a full day
        // later, well outside the 15-minute hold window.
        let ts_a = 600i64;
        let ts_b = 86_400i64 + 600;
        let row_a = serde_json::json!({
            "ts": ts_a, "asset": "BTC", "features": features, "fwd_bps": {"15": 20.0},
        });
        let row_b = serde_json::json!({
            "ts": ts_b, "asset": "BTC", "features": features, "fwd_bps": {"15": 20.0},
        });
        let matrix_path = dir.join("matrix.jsonl");
        std::fs::write(&matrix_path, format!("{manifest}\n{row_a}\n{row_b}\n")).unwrap();

        let folds_path = dir.join("folds.json");
        std::fs::write(
            &folds_path,
            serde_json::to_string(&serde_json::json!({
                "horizon_min": 15,
                "folds": [{
                    "i": 0,
                    "train_start_ts": 0,
                    "train_end_ts": 0,
                    "test_start_ts": 0,
                    "test_end_ts": 3 * 86_400,
                    "model": "fold-0.json",
                }],
            }))
            .unwrap(),
        )
        .unwrap();

        // A single-leaf tree: predicts the constant 100.0 bps for every row,
        // regardless of feature values - well past 1.5x either day's
        // round-trip cost, so both rows must trade long.
        let model = serde_json::json!({
            "format_version": MODEL_FORMAT_VERSION,
            "feature_set_version": FEATURE_SET_VERSION,
            "features": feature_names,
            "model": {
                "tree_info": [{
                    "tree_index": 0,
                    "num_leaves": 1,
                    "num_cat": 0,
                    "shrinkage": 1.0,
                    "tree_structure": { "leaf_value": 100.0 },
                }],
            },
        });
        std::fs::write(
            dir.join("fold-0.json"),
            serde_json::to_string(&model).unwrap(),
        )
        .unwrap();

        // Only day 0 (1970-01-01) has a DayCost - day 1's entry must fall
        // back to it. round_trip = 2*3.0 + 5.0 + 2*2.0 = 15.0;
        // threshold = 1.5*15 = 22.5 < 100.0 pred, so both rows trade.
        let binance_costs_path = dir.join("costs-daily.json");
        std::fs::write(
            &binance_costs_path,
            serde_json::to_string(&serde_json::json!({
                "BTC": { "1970-01-01": { "spread_bps_p75": 5.0, "impact_bps": 2.0, "samples": 50 } },
            }))
            .unwrap(),
        )
        .unwrap();

        let out_path = dir.join("report.json");
        let args = vec![
            "--matrix".to_string(),
            matrix_path.to_string_lossy().to_string(),
            "--folds".to_string(),
            folds_path.to_string_lossy().to_string(),
            "--binance-costs".to_string(),
            binance_costs_path.to_string_lossy().to_string(),
            "--fee-taker-bps".to_string(),
            "3.0".to_string(),
            "--fee-maker-bps".to_string(),
            "1.5".to_string(),
            "--out".to_string(),
            out_path.to_string_lossy().to_string(),
        ];
        cmd_gate(&args).unwrap();

        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&out_path).unwrap()).unwrap();

        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(report["overall"]["n_trades"], 2);
        assert_eq!(report["fee_taker_bps"], 3.0);
        assert_eq!(report["fee_maker_bps"], 1.5);
        assert_eq!(report["overall"]["fee_bps_used"], 3.0);
        assert_eq!(
            report["days_without_costs"].as_object().unwrap().len(),
            0,
            "both entry days resolved a cost (day 1 via the prior-day fallback)"
        );
        // 2 trades * 2 legs * $5,000 (default --notional) / 3 test-span days
        // (the fold spans 1970-01-01..1970-01-04 exclusive) * 30 = 200,000.
        let projected = report["overall"]["projected_30d_volume_usd"]
            .as_f64()
            .unwrap();
        assert!((projected - 200_000.0).abs() < 1e-6, "got {projected}");
        let net_bps_a = report["per_asset"]["BTC"]["total_net_bps"]
            .as_f64()
            .unwrap();
        // Both trades net fwd_bps(20) - round_trip(15) = 5 each -> 10 total.
        assert!((net_bps_a - 10.0).abs() < 1e-9, "got {net_bps_a}");
    }

    /// `days_without_costs` must count DISTINCT days the asset actually had
    /// a dropped prediction on, not every calendar day in the fold's whole
    /// test span regardless of whether the asset had a row there at all. A
    /// fold spanning 25 days with cost data anchored at day 0 (so BTC has
    /// SOME tradable days via the 14-day fallback, days 0-14, keeping the
    /// asset out of "no tradable assets") but a single BTC prediction on
    /// day 20 - 6 days past the fallback's reach - must report exactly 1,
    /// not the ~10 unresolvable days_span entries a days_span-driven sweep
    /// would have produced before this fix.
    #[test]
    fn days_without_costs_counts_dropped_prediction_days_not_the_whole_span() {
        use features_scalper::{FEATURE_NAMES, FEATURE_SET_VERSION};

        let dir = std::env::temp_dir().join(format!(
            "scalper-gate-days-without-costs-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        let mut features = serde_json::Map::new();
        for name in FEATURE_NAMES {
            features.insert(name.to_string(), serde_json::json!(1.0));
        }
        let feature_names: Vec<&str> = FEATURE_NAMES.to_vec();
        let manifest = serde_json::json!({
            "kind": "manifest",
            "feature_set_version": FEATURE_SET_VERSION,
            "features": feature_names,
            "horizons_min": [15],
            "stride_min": 1,
            "assets": ["BTC"],
        });
        // The lone BTC row: day 20, well past day 0's 14-day fallback reach.
        let ts = 20 * 86_400i64 + 600;
        let row = serde_json::json!({
            "ts": ts, "asset": "BTC", "features": features, "fwd_bps": {"15": 1.0},
        });
        let matrix_path = dir.join("matrix.jsonl");
        std::fs::write(&matrix_path, format!("{manifest}\n{row}\n")).unwrap();

        let folds_path = dir.join("folds.json");
        std::fs::write(
            &folds_path,
            serde_json::to_string(&serde_json::json!({
                "horizon_min": 15,
                "folds": [{
                    "i": 0, "train_start_ts": 0, "train_end_ts": 0,
                    "test_start_ts": 0, "test_end_ts": 25 * 86_400,
                    "model": "fold-0.json",
                }],
            }))
            .unwrap(),
        )
        .unwrap();

        let model = serde_json::json!({
            "format_version": MODEL_FORMAT_VERSION,
            "feature_set_version": FEATURE_SET_VERSION,
            "features": feature_names,
            "model": { "tree_info": [{
                "tree_index": 0, "num_leaves": 1, "num_cat": 0, "shrinkage": 1.0,
                "tree_structure": { "leaf_value": 100.0 },
            }] },
        });
        std::fs::write(
            dir.join("fold-0.json"),
            serde_json::to_string(&model).unwrap(),
        )
        .unwrap();

        // Only day 0 has cost data - BTC is tradable on days 0-14 via the
        // fallback (keeping it out of "no tradable assets"), but day 20 is
        // 6 days beyond that reach.
        let binance_costs_path = dir.join("costs-daily.json");
        std::fs::write(
            &binance_costs_path,
            serde_json::to_string(&serde_json::json!({
                "BTC": { "1970-01-01": { "spread_bps_p75": 5.0, "impact_bps": 2.0, "samples": 50 } },
            }))
            .unwrap(),
        )
        .unwrap();

        let out_path = dir.join("report.json");
        let args = vec![
            "--matrix".to_string(),
            matrix_path.to_string_lossy().to_string(),
            "--folds".to_string(),
            folds_path.to_string_lossy().to_string(),
            "--binance-costs".to_string(),
            binance_costs_path.to_string_lossy().to_string(),
            "--fee-taker-bps".to_string(),
            "3.0".to_string(),
            "--fee-maker-bps".to_string(),
            "1.5".to_string(),
            "--out".to_string(),
            out_path.to_string_lossy().to_string(),
        ];
        cmd_gate(&args).unwrap();

        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&out_path).unwrap()).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(
            report["overall"]["n_trades"], 0,
            "the only prediction had no cost"
        );
        let count = report["days_without_costs"]["BTC"].as_u64().unwrap();
        assert_eq!(
            count, 1,
            "must count the one day BTC actually had a dropped prediction on, not the span"
        );
    }

    /// `--exit time` (the default, unspecified here) must never emit
    /// `exit_stats` or `overall.exit_mode` at all - not `null`, ABSENT -
    /// since the byte-identity guard for Amendment 3 (`--exit time` must
    /// diff empty against every gate report written before it) depends on
    /// these keys not existing in that mode's JSON.
    #[test]
    fn exit_time_mode_omits_the_new_report_fields_entirely() {
        use features_scalper::{FEATURE_NAMES, FEATURE_SET_VERSION};

        let dir = std::env::temp_dir().join(format!(
            "scalper-gate-e2e-exit-time-omits-fields-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        let mut features = serde_json::Map::new();
        for name in FEATURE_NAMES {
            features.insert(name.to_string(), serde_json::json!(1.0));
        }
        let feature_names: Vec<&str> = FEATURE_NAMES.to_vec();
        let manifest = serde_json::json!({
            "kind": "manifest",
            "feature_set_version": FEATURE_SET_VERSION,
            "features": feature_names,
            "horizons_min": [15],
            "stride_min": 1,
            "assets": ["BTC"],
        });
        let row = serde_json::json!({
            "ts": 600, "asset": "BTC", "features": features, "fwd_bps": {"15": 20.0},
        });
        let matrix_path = dir.join("matrix.jsonl");
        std::fs::write(&matrix_path, format!("{manifest}\n{row}\n")).unwrap();

        let folds_path = dir.join("folds.json");
        std::fs::write(
            &folds_path,
            serde_json::to_string(&serde_json::json!({
                "horizon_min": 15,
                "folds": [{
                    "i": 0, "train_start_ts": 0, "train_end_ts": 0,
                    "test_start_ts": 0, "test_end_ts": 3_600,
                    "model": "fold-0.json",
                }],
            }))
            .unwrap(),
        )
        .unwrap();

        let model = serde_json::json!({
            "format_version": MODEL_FORMAT_VERSION,
            "feature_set_version": FEATURE_SET_VERSION,
            "features": feature_names,
            "model": { "tree_info": [{
                "tree_index": 0, "num_leaves": 1,
                "tree_structure": { "leaf_value": 100.0 },
            }] },
        });
        std::fs::write(dir.join("fold-0.json"), serde_json::to_string(&model).unwrap()).unwrap();

        let costs_path = dir.join("costs.json");
        std::fs::write(&costs_path, "{}").unwrap();

        let out_path = dir.join("report.json");
        let args = vec![
            "--matrix".to_string(),
            matrix_path.to_string_lossy().to_string(),
            "--folds".to_string(),
            folds_path.to_string_lossy().to_string(),
            "--costs".to_string(),
            costs_path.to_string_lossy().to_string(),
            "--out".to_string(),
            out_path.to_string_lossy().to_string(),
        ];
        cmd_gate(&args).unwrap(); // no --exit at all - the default

        let raw = std::fs::read_to_string(&out_path).unwrap();
        let report: serde_json::Value = serde_json::from_str(&raw).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert!(
            !raw.contains("exit_stats") && !raw.contains("exit_mode"),
            "the raw JSON text must not contain either key at all under --exit time"
        );
        assert!(report.get("exit_stats").is_none());
        assert!(report["overall"].get("exit_mode").is_none());
    }

    /// Full wiring, end to end, for `--exit atr` (Amendment 3): a flat 1m
    /// bar series (constant OHLC -> constant ATR = 0.1 at every warm index)
    /// written to a real perp store, one matrix row at bar index 20 (well
    /// past the 14-bar ATR warm-up), a single-leaf model predicting a
    /// constant 100.0 (well past the DEFAULT_ROUND_TRIP_BPS-derived
    /// threshold), and a 15-bar horizon. Neither the stop
    /// (entry - 4*0.1 = 99.6) nor the target (entry + 4.8*0.1 = 100.48) is
    /// ever touched by the flat path (low=99.95, high=100.05 throughout),
    /// so the trade must resolve as a Time exit at bar 35's close (100.0),
    /// giving net_bps = 0 gross minus the DEFAULT_ROUND_TRIP_BPS round trip
    /// (20.0), hand-computed and checked exactly.
    #[test]
    fn cmd_gate_runs_end_to_end_in_atr_mode_and_resolves_a_time_exit() {
        use chrono::{Duration, TimeZone};
        use features_scalper::{FEATURE_NAMES, FEATURE_SET_VERSION};

        let dir = std::env::temp_dir().join(format!(
            "scalper-gate-e2e-atr-exit-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        let base = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let bars: Vec<Bar> = (0..50)
            .map(|i| Bar {
                ts_utc: base + Duration::minutes(i),
                asset: "TEST".to_string(),
                interval_s: 60,
                open: 100.0,
                high: 100.05,
                low: 99.95,
                close: 100.0,
                volume: 1.0,
                quote_volume: Some(100.0),
                trades: Some(5),
            })
            .collect();
        let entry_idx = 20usize;
        let entry_ts = bars[entry_idx].ts_utc.timestamp();

        let perp_root = dir.join("perp");
        crypto_portfolio::store::write(&perp_root, &bars).unwrap();

        let mut features = serde_json::Map::new();
        for name in FEATURE_NAMES {
            features.insert(name.to_string(), serde_json::json!(1.0));
        }
        let feature_names: Vec<&str> = FEATURE_NAMES.to_vec();
        let manifest = serde_json::json!({
            "kind": "manifest",
            "feature_set_version": FEATURE_SET_VERSION,
            "features": feature_names,
            "horizons_min": [15],
            "stride_min": 1,
            "assets": ["TEST"],
        });
        let row = serde_json::json!({
            "ts": entry_ts, "asset": "TEST", "features": features, "fwd_bps": {"15": 999.0},
        });
        let matrix_path = dir.join("matrix.jsonl");
        std::fs::write(&matrix_path, format!("{manifest}\n{row}\n")).unwrap();

        let folds_path = dir.join("folds.json");
        std::fs::write(
            &folds_path,
            serde_json::to_string(&serde_json::json!({
                "horizon_min": 15,
                "folds": [{
                    "i": 0, "train_start_ts": 0, "train_end_ts": entry_ts,
                    "test_start_ts": entry_ts - 60, "test_end_ts": entry_ts + 60,
                    "model": "fold-0.json",
                }],
            }))
            .unwrap(),
        )
        .unwrap();

        // Single-leaf model: predicts a constant 100.0 bps regardless of
        // features - well past 1.5 * DEFAULT_ROUND_TRIP_BPS (30.0).
        let model = serde_json::json!({
            "format_version": MODEL_FORMAT_VERSION,
            "feature_set_version": FEATURE_SET_VERSION,
            "features": feature_names,
            "model": { "tree_info": [{
                "tree_index": 0, "num_leaves": 1,
                "tree_structure": { "leaf_value": 100.0 },
            }] },
        });
        std::fs::write(dir.join("fold-0.json"), serde_json::to_string(&model).unwrap()).unwrap();

        // Empty costs -> "TEST" falls back to DEFAULT_ROUND_TRIP_BPS (20.0).
        let costs_path = dir.join("costs.json");
        std::fs::write(&costs_path, "{}").unwrap();

        let out_path = dir.join("report.json");
        let args = vec![
            "--matrix".to_string(),
            matrix_path.to_string_lossy().to_string(),
            "--folds".to_string(),
            folds_path.to_string_lossy().to_string(),
            "--costs".to_string(),
            costs_path.to_string_lossy().to_string(),
            "--exit".to_string(),
            "atr".to_string(),
            "--data-root".to_string(),
            dir.to_string_lossy().to_string(),
            "--out".to_string(),
            out_path.to_string_lossy().to_string(),
        ];
        cmd_gate(&args).unwrap();

        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&out_path).unwrap()).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(report["overall"]["exit_mode"], "atr");
        assert_eq!(report["overall"]["n_trades"], 1);
        assert_eq!(report["exit_stats"]["stops"], 0);
        assert_eq!(report["exit_stats"]["targets"], 0);
        assert_eq!(report["exit_stats"]["time_exits"], 1);
        assert_eq!(report["exit_stats"]["mean_bars_held"], 15.0);
        assert_eq!(report["exit_stats"]["skipped_no_atr"], 0);

        // Hand-computed: gross = 1 * (100.0/100.0 - 1) * 1e4 = 0.0;
        // net = 0.0 - 20.0 = -20.0.
        let net_bps = report["per_asset"]["TEST"]["total_net_bps"]
            .as_f64()
            .unwrap();
        assert!(
            (net_bps - (-20.0)).abs() < 1e-9,
            "flat path -> 0 gross, net = -DEFAULT_ROUND_TRIP_BPS, got {net_bps}"
        );
    }
}
