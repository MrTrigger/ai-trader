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

use crate::costs::CostSummary;
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
/// label, feature_set_version, model_version, horizon_min, and everything
/// LightGBM stuffs into `model` besides `tree_info`) are intentionally
/// ignored here - this loader's only job is trees plus the feature order
/// they were fit against, mirroring `crypto_portfolio::model::Model::load`'s
/// parsing idiom without inheriting its crypto-portfolio-specific checks
/// (model_version, feature_set_version, x_-prefix convention) that don't
/// apply to the scalper's artifact.
#[derive(Debug, Deserialize)]
struct ModelArtifact {
    format_version: String,
    features: Vec<String>,
    model: ModelDump,
}

#[derive(Debug, Deserialize)]
struct ModelDump {
    tree_info: Vec<lightgbm_json::Tree>,
}

/// Load a fold artifact and return its trees, having verified the artifact
/// is the expected format and that its feature order matches
/// `expected_features` exactly - order-exact, because `lightgbm_json::Tree`
/// addresses features by index, not name. A mismatch is a hard error: it
/// means the model would silently score the wrong inputs.
pub fn load_model(
    path: &Path,
    expected_features: &[String],
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

/// Per-asset threshold-gated long/short simulation.
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
pub fn simulate(
    preds: &[Pred],
    round_trip_bps: &BTreeMap<String, f64>,
    horizon_min: i64,
    threshold_mult: f64,
) -> Vec<Trade> {
    let mut sorted: Vec<&Pred> = preds.iter().collect();
    sorted.sort_by(|a, b| (a.asset.as_str(), a.ts).cmp(&(b.asset.as_str(), b.ts)));

    let hold_secs = horizon_min * 60;
    let mut flat_after: BTreeMap<&str, i64> = BTreeMap::new();
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
        flat_after.insert(p.asset.as_str(), p.ts + hold_secs);
    }
    trades
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

/// The training matrix: manifest feature order plus every row. Reuses
/// `matrix::MatrixRow` (Task 2's row type) rather than redefining it.
struct Matrix {
    features: Vec<String>,
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

    let rows: Vec<MatrixRow> = lines
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(|e| format!("{}: row: {e}", path.display())))
        .collect::<Result<_, _>>()?;

    Ok(Matrix { features, rows })
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

    let mut fold_reports = Vec::with_capacity(folds_doc.folds.len());
    let mut all_trades: Vec<Trade> = Vec::new();
    let mut stitched_daily: BTreeMap<NaiveDate, f64> = BTreeMap::new();

    for fold in &folds_doc.folds {
        let model_path = folds_dir.join(&fold.model);
        let trees = load_model(&model_path, &matrix.features)?;

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

        let trades = simulate(&preds, &round_trip, folds_doc.horizon_min, threshold_mult);
        let daily = daily_returns_bps(&trades, n_assets, (fold.test_start_ts, fold.test_end_ts));
        let sharpe = annualized_sharpe(&daily);

        fold_reports.push(FoldReport {
            i: fold.i,
            train_span: [fold.train_start_ts, fold.train_end_ts],
            test_span: [fold.test_start_ts, fold.test_end_ts],
            n_trades: trades.len(),
            sharpe,
        });

        // Fold test windows come from a single walk-forward split and never
        // overlap, so merging their daily maps can't silently clobber a day
        // two folds both claim.
        for (day, bps) in &daily {
            if stitched_daily.insert(*day, *bps).is_some() {
                return Err(format!(
                    "fold {} test window overlaps a previous fold's day {day} - folds.json is \
                     not a valid walk-forward split",
                    fold.i
                ));
            }
        }
        all_trades.extend(trades);
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
            Some(s) if s >= GATE_THRESHOLD => "PASS",
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
        let trees = load_model(Path::new(&format!("{dir}/model.json")), &features).unwrap();
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
}
