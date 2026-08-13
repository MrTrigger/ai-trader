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

use crate::costs::{percentile, CostSummary};
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
pub fn simulate_with_state(
    preds: &[Pred],
    round_trip_bps: &BTreeMap<String, f64>,
    horizon_min: i64,
    threshold_mult: f64,
    carry: &BTreeMap<String, i64>,
) -> (Vec<Trade>, BTreeMap<String, i64>) {
    let mut sorted: Vec<&Pred> = preds.iter().collect();
    sorted.sort_by(|a, b| (a.asset.as_str(), a.ts).cmp(&(b.asset.as_str(), b.ts)));

    let hold_secs = horizon_min * 60;
    let mut flat_after: BTreeMap<String, i64> = carry.clone();
    let mut trades = Vec::new();

    for p in sorted {
        let Some(&round_trip) = round_trip_bps.get(p.asset.as_str()) else {
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

/// UTC calendar date a unix timestamp falls on.
fn date_of(ts: i64) -> NaiveDate {
    DateTime::<Utc>::from_timestamp(ts, 0)
        .expect("timestamp in range")
        .date_naive()
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
/// was built against, plus every row. Reuses `matrix::MatrixRow` (Task 2's
/// row type) rather than redefining it.
struct Matrix {
    features: Vec<String>,
    feature_set_version: String,
    rows: Vec<MatrixRow>,
}

fn load_matrix(path: &Path) -> Result<Matrix, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut lines = text.lines();
    let manifest_line = lines
        .next()
        .ok_or_else(|| format!("{}: empty matrix", path.display()))?;
    let manifest: serde_json::Value = serde_json::from_str(manifest_line)
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

    let rows: Vec<MatrixRow> = lines
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(|e| format!("{}: row: {e}", path.display())))
        .collect::<Result<_, _>>()?;

    Ok(Matrix {
        features,
        feature_set_version,
        rows,
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
    threshold_bps_by_asset: BTreeMap<String, f64>,
}

#[derive(Serialize)]
struct Report {
    generated_utc: String,
    matrix: String,
    costs: String,
    horizon_min: i64,
    threshold_mult: f64,
    notional: String,
    folds: Vec<FoldReport>,
    per_asset: BTreeMap<String, AssetReport>,
    excluded_thin_books: Vec<String>,
    daily_returns_bps: BTreeMap<String, f64>,
    overall: OverallReport,
}

pub fn cmd_gate(args: &[String]) -> Result<(), String> {
    let matrix_path = PathBuf::from(need(args, "--matrix")?);
    let folds_path = PathBuf::from(need(args, "--folds")?);
    let costs_path = PathBuf::from(need(args, "--costs")?);
    let out_path = PathBuf::from(need(args, "--out")?);
    let threshold_mult: f64 = match get(args, "--threshold-mult") {
        Some(v) => v
            .parse()
            .map_err(|e| format!("bad --threshold-mult: {e}"))?,
        None => DEFAULT_THRESHOLD_MULT,
    };
    let notional = get(args, "--notional").unwrap_or_else(|| DEFAULT_NOTIONAL.to_string());

    let matrix = load_matrix(&matrix_path)?;
    let folds_doc: FoldsDocument = {
        let text = std::fs::read_to_string(&folds_path)
            .map_err(|e| format!("{}: {e}", folds_path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", folds_path.display()))?
    };
    let costs: BTreeMap<String, CostSummary> = {
        let text = std::fs::read_to_string(&costs_path)
            .map_err(|e| format!("{}: {e}", costs_path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", costs_path.display()))?
    };
    let folds_dir = folds_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let horizon_key = folds_doc.horizon_min.to_string();
    let assets: Vec<String> = matrix
        .rows
        .iter()
        .map(|r| r.asset.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let (round_trip, excluded_thin_books) = compute_round_trip_bps(&assets, &costs, &notional);
    let n_assets = round_trip.len();
    if n_assets == 0 {
        return Err(
            "no tradable assets: every matrix asset is thin-book excluded or has no cost data \
             for --notional"
                .into(),
        );
    }

    // Chronological order (not file order) is what makes the hold-state
    // carry below correct, and doubles as the real-overlap check.
    let ordered_folds = validate_no_ts_overlap(&folds_doc.folds)?;

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

    for fold in &ordered_folds {
        let model_path = folds_dir.join(&fold.model);
        let trees = load_model(&model_path, &matrix.features, &matrix.feature_set_version)?;

        let mut preds: Vec<Pred> = Vec::new();
        for row in matrix
            .rows
            .iter()
            .filter(|r| r.ts >= fold.test_start_ts && r.ts < fold.test_end_ts)
        {
            let Some(&fwd_bps) = row.fwd_bps.get(&horizon_key) else {
                continue; // no label at this horizon for this row
            };
            let values: Vec<f64> = matrix
                .features
                .iter()
                .map(|name| {
                    row.features.get(name).copied().ok_or_else(|| {
                        format!(
                            "{}: row ts={} asset={} missing feature {name:?}",
                            matrix_path.display(),
                            row.ts,
                            row.asset
                        )
                    })
                })
                .collect::<Result<_, _>>()?;
            let pred_bps = lightgbm_json::predict(&trees, &values)?;
            preds.push(Pred {
                ts: row.ts,
                asset: row.asset.clone(),
                pred_bps,
                fwd_bps,
            });
        }

        let (trades, next_state) = simulate_with_state(
            &preds,
            &round_trip,
            folds_doc.horizon_min,
            threshold_mult,
            &hold_state,
        );
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

    let report = Report {
        generated_utc: Utc::now().to_rfc3339(),
        matrix: matrix_path.display().to_string(),
        costs: costs_path.display().to_string(),
        horizon_min: folds_doc.horizon_min,
        threshold_mult,
        notional,
        folds: fold_reports,
        per_asset,
        excluded_thin_books,
        daily_returns_bps: stitched_daily
            .into_iter()
            .map(|(day, bps)| (day.to_string(), bps))
            .collect(),
        overall: OverallReport {
            n_trades: n_trades_total,
            sharpe_annualized: overall_sharpe,
            gate: gate.to_string(),
            gate_threshold: GATE_THRESHOLD,
            ic: pearson_ic(&all_preds),
            rank_ic: rank_ic(&all_preds),
            threshold_bps_by_asset: round_trip
                .iter()
                .map(|(asset, rt)| (asset.clone(), threshold_mult * rt))
                .collect(),
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
    /// 26-feature order, exactly as they were before this task bumped
    /// `features_scalper::FEATURE_SET_VERSION` to `fs-rust-scalper-2` -
    /// scored against a matrix built from the current (fs-2) feature
    /// catalog must be refused, not silently scored against the wrong 26
    /// columns of a 38-column row.
    #[test]
    fn a_stale_fs1_artifact_is_refused_end_to_end_against_an_fs2_matrix() {
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
            "expected the gate to refuse the stale fs-1 artifact against the fs-2 matrix, \
             got: {err}"
        );
    }
}
