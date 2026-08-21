//! Market-level matrix and timing model for the directional bot's round-2
//! V4 (`docs/crypto-directional-design.md`). One row per UTC day: market
//! features computed in Rust from the stores under `--data-root` (bars,
//! funding, and the `data/directional/` raw pulls), plus vol-scaled forward
//! labels at 7/14/30 days. Python fits LightGBM on the matrix (house rule:
//! Python fits, nothing else); inference reads the SAME matrix rows through
//! `model::Tree`, so train and inference cannot diverge on a feature.
//!
//! Point-in-time discipline: every feature at date D uses data through D
//! (bars) or D−1 (external series - FRED values, DVOL, stablecoins, OI are
//! lagged one day, which also covers publication lag). Labels look forward
//! from D and exist only where the index does.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::NaiveDate;
use features_crypto::DailyRow;
use serde::{Deserialize, Serialize};

/// Feature catalog, fixed by the round-2 pre-registration. Order is the
/// model input order; missing values are NaN (LightGBM handles them via
/// default_left, same as the ranker's imputation-free columns).
pub const MARKET_FEATURES: [&str; 20] = [
    "mkt_ret_7",
    "mkt_ret_30",
    "mkt_ret_90",
    "mkt_z_7",
    "mkt_z_30",
    "mkt_vol_7",
    "mkt_vol_30",
    "breadth_7",
    "breadth_30",
    "breadth_90",
    "breadth_180",
    "fund_mean_7d",
    "fund_disp",
    "fund_extreme_share",
    "oi_chg_7",
    "oi_chg_30",
    "dvol",
    "dvol_prem",
    "stab_g7",
    "stab_g30",
];

pub const LABELS: [&str; 3] = ["fwd_z_7", "fwd_z_14", "fwd_z_30"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketRow {
    pub date: NaiveDate,
    /// Present features only; absent = NaN to the model.
    pub features: BTreeMap<String, f64>,
    /// Present labels only; the tail of the series has none.
    pub labels: BTreeMap<String, f64>,
}

fn is_finite(v: f64) -> Option<f64> {
    v.is_finite().then_some(v)
}

fn std_dev(xs: &[f64]) -> Option<f64> {
    if xs.len() < 2 {
        return None;
    }
    let mean = xs.iter().sum::<f64>() / xs.len() as f64;
    let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (xs.len() - 1) as f64;
    is_finite(var.sqrt())
}

/// Most recent value with date STRICTLY BEFORE `d` (the one-day external
/// lag), within `max_age` days.
fn lagged(series: &BTreeMap<NaiveDate, f64>, d: NaiveDate, max_age: i64) -> Option<f64> {
    series
        .range(..d)
        .next_back()
        .filter(|(k, _)| (d - **k).num_days() <= max_age)
        .map(|(_, v)| *v)
}

/// Build the market matrix from prepared daily rows plus the raw pulls in
/// `{root}/directional/`. Deterministic; safe to rebuild any time.
pub fn build_market_matrix(daily: &[DailyRow], root: &Path) -> Result<Vec<MarketRow>, String> {
    // ---- group by date ----
    let mut by_date: BTreeMap<NaiveDate, Vec<&DailyRow>> = BTreeMap::new();
    for row in daily {
        by_date
            .entry(row.ts_utc.date_naive())
            .or_default()
            .push(row);
    }

    // ---- external series (all lagged at use) ----
    let dir = root.join("directional");
    let oi = load_oi(&dir)?;
    let dvol = load_dvol(&dir)?;
    let stab = load_stablecoins(&dir)?;

    // ---- per-date market aggregates, then rolling stats on the index ----
    let dates: Vec<NaiveDate> = by_date.keys().copied().collect();
    let mut level = 0.0f64;
    let mut levels: Vec<f64> = Vec::with_capacity(dates.len());
    let mut daily_ret: Vec<Option<f64>> = Vec::with_capacity(dates.len());
    let mut per_date_feats: Vec<BTreeMap<String, f64>> = Vec::with_capacity(dates.len());

    for d in &dates {
        let rows = &by_date[d];
        let eligible: Vec<&&DailyRow> = rows
            .iter()
            .filter(|r| r.bars_available >= 90 && r.adv_quote.is_some())
            .collect();
        let mut top: Vec<&&DailyRow> = eligible.clone();
        top.sort_by(|a, b| {
            b.adv_quote
                .unwrap_or(0.0)
                .total_cmp(&a.adv_quote.unwrap_or(0.0))
                .then_with(|| a.asset.cmp(&b.asset))
        });
        top.truncate(20);

        let rets: Vec<f64> = top.iter().filter_map(|r| r.ret_1).collect();
        let r = if rets.is_empty() {
            None
        } else {
            is_finite(rets.iter().sum::<f64>() / rets.len() as f64)
        };
        if let Some(r) = r {
            level += r;
        }
        levels.push(level);
        daily_ret.push(r);

        let mut f = BTreeMap::new();
        let breadth = |get: &dyn Fn(&DailyRow) -> Option<f64>| -> Option<f64> {
            let v: Vec<f64> = eligible.iter().filter_map(|r| get(r)).collect();
            (!v.is_empty())
                .then(|| 2.0 * v.iter().filter(|x| **x > 0.0).count() as f64 / v.len() as f64 - 1.0)
        };
        if let Some(b) = breadth(&|r| r.ret_7) {
            f.insert("breadth_7".into(), b);
        }
        if let Some(b) = breadth(&|r| r.ret_30) {
            f.insert("breadth_30".into(), b);
        }
        if let Some(b) = breadth(&|r| r.ret_90) {
            f.insert("breadth_90".into(), b);
        }
        if let Some(b) = breadth(&|r| r.ret_180) {
            f.insert("breadth_180".into(), b);
        }
        let fundings: Vec<f64> = top.iter().filter_map(|r| r.funding_7d).collect();
        if !fundings.is_empty() {
            f.insert(
                "fund_mean_7d".into(),
                fundings.iter().sum::<f64>() / fundings.len() as f64,
            );
            if let Some(sd) = std_dev(&fundings) {
                f.insert("fund_disp".into(), sd);
            }
        }
        let fzs: Vec<f64> = top.iter().filter_map(|r| r.funding_z).collect();
        if !fzs.is_empty() {
            f.insert(
                "fund_extreme_share".into(),
                fzs.iter().filter(|z| z.abs() > 2.0).count() as f64 / fzs.len() as f64,
            );
        }
        per_date_feats.push(f);
    }

    // ---- rolling index stats + externals + labels ----
    let mut out = Vec::with_capacity(dates.len());
    for (i, d) in dates.iter().enumerate() {
        let mut f = per_date_feats[i].clone();
        let ret_h = |h: usize| -> Option<f64> {
            (i >= h)
                .then(|| levels[i] - levels[i - h])
                .and_then(is_finite)
        };
        let vol_h = |h: usize| -> Option<f64> {
            if i + 1 < h {
                return None;
            }
            let w: Vec<f64> = daily_ret[i + 1 - h..=i].iter().flatten().copied().collect();
            if w.len() < h * 3 / 4 {
                return None;
            }
            std_dev(&w).map(|s| s * 365.0f64.sqrt())
        };
        let vol30 = vol_h(30);
        for (name, h) in [
            ("mkt_ret_7", 7usize),
            ("mkt_ret_30", 30),
            ("mkt_ret_90", 90),
        ] {
            if let Some(v) = ret_h(h) {
                f.insert(name.into(), v);
            }
        }
        if let Some(v) = vol_h(7) {
            f.insert("mkt_vol_7".into(), v);
        }
        if let Some(v) = vol30 {
            f.insert("mkt_vol_30".into(), v);
        }
        for (name, h) in [("mkt_z_7", 7usize), ("mkt_z_30", 30)] {
            if let (Some(r), Some(v)) = (ret_h(h), vol30) {
                if v > 0.0 {
                    if let Some(z) = is_finite(r / (v * (h as f64 / 365.0).sqrt())) {
                        f.insert(name.into(), z);
                    }
                }
            }
        }
        // Externals, one-day lagged.
        if let (Some(now), Some(then)) = (
            lagged(&oi, *d, 5),
            lagged(&oi, *d - chrono::Duration::days(7), 5),
        ) {
            if now > 0.0 && then > 0.0 {
                if let Some(v) = is_finite((now / then).ln()) {
                    f.insert("oi_chg_7".into(), v);
                }
            }
        }
        if let (Some(now), Some(then)) = (
            lagged(&oi, *d, 5),
            lagged(&oi, *d - chrono::Duration::days(30), 5),
        ) {
            if now > 0.0 && then > 0.0 {
                if let Some(v) = is_finite((now / then).ln()) {
                    f.insert("oi_chg_30".into(), v);
                }
            }
        }
        if let Some(v) = lagged(&dvol, *d, 5) {
            f.insert("dvol".into(), v / 100.0);
            if let Some(rv) = vol30 {
                f.insert("dvol_prem".into(), v / 100.0 - rv);
            }
        }
        if let (Some(now), Some(then)) = (
            lagged(&stab, *d, 5),
            lagged(&stab, *d - chrono::Duration::days(7), 5),
        ) {
            if now > 0.0 && then > 0.0 {
                if let Some(v) = is_finite((now / then).ln()) {
                    f.insert("stab_g7".into(), v);
                }
            }
        }
        if let (Some(now), Some(then)) = (
            lagged(&stab, *d, 5),
            lagged(&stab, *d - chrono::Duration::days(30), 5),
        ) {
            if now > 0.0 && then > 0.0 {
                if let Some(v) = is_finite((now / then).ln()) {
                    f.insert("stab_g30".into(), v);
                }
            }
        }
        // Labels: vol-scaled forward index return. Only where the future
        // exists and vol is defined.
        let mut labels = BTreeMap::new();
        if let Some(v) = vol30.filter(|v| *v > 0.0) {
            for (name, h) in [("fwd_z_7", 7usize), ("fwd_z_14", 14), ("fwd_z_30", 30)] {
                if i + h < levels.len() {
                    if let Some(z) =
                        is_finite((levels[i + h] - levels[i]) / (v * (h as f64 / 365.0).sqrt()))
                    {
                        labels.insert(name.into(), z);
                    }
                }
            }
        }
        // A row with no index history at all is not a row.
        if f.contains_key("mkt_ret_30") {
            out.push(MarketRow {
                date: *d,
                features: f,
                labels,
            });
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------
// raw pull parsers ({root}/directional/, fetched by documented commands)
// ---------------------------------------------------------------------

/// BTC+ETH summed open-interest value per day, from the metrics jsonl the
/// scalper puller writes (last 5-minute row of each day file).
fn load_oi(dir: &Path) -> Result<BTreeMap<NaiveDate, f64>, String> {
    #[derive(Deserialize)]
    struct M {
        ts_s: i64,
        sum_open_interest_value: Option<f64>,
    }
    let mut by_day: BTreeMap<NaiveDate, f64> = BTreeMap::new();
    for symbol in ["BTCUSDT", "ETHUSDT"] {
        let sub = dir.join("binance-micro").join("metrics").join(symbol);
        let Ok(entries) = std::fs::read_dir(&sub) else {
            continue; // absent source = absent features, not an error
        };
        for entry in entries {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().is_none_or(|e| e != "jsonl") {
                continue;
            }
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let mut last: Option<(i64, f64)> = None;
            for line in text.lines() {
                let m: M =
                    serde_json::from_str(line).map_err(|e| format!("{}: {e}", path.display()))?;
                if let Some(v) = m.sum_open_interest_value {
                    if last.is_none_or(|(ts, _)| m.ts_s > ts) {
                        last = Some((m.ts_s, v));
                    }
                }
            }
            if let Some((ts, v)) = last {
                let day = chrono::DateTime::from_timestamp(ts, 0)
                    .ok_or("bad metrics ts")?
                    .date_naive();
                *by_day.entry(day).or_default() += v;
            }
        }
    }
    Ok(by_day)
}

/// Deribit DVOL (BTC), last value per day from the 12h series.
fn load_dvol(dir: &Path) -> Result<BTreeMap<NaiveDate, f64>, String> {
    let path = dir.join("dvol-btc.json");
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let rows: Vec<(i64, f64, f64, f64, f64)> =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = BTreeMap::new();
    for (ts_ms, _o, _h, _l, close) in rows {
        let day = chrono::DateTime::from_timestamp_millis(ts_ms)
            .ok_or("bad dvol ts")?
            .date_naive();
        out.insert(day, close); // ascending input: last of day wins
    }
    Ok(out)
}

/// DeFiLlama total stablecoin circulating (peggedUSD), per day.
fn load_stablecoins(dir: &Path) -> Result<BTreeMap<NaiveDate, f64>, String> {
    let path = dir.join("stablecoins.json");
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    #[derive(Deserialize)]
    struct Row {
        date: serde_json::Value,
        #[serde(rename = "totalCirculating")]
        total: BTreeMap<String, f64>,
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let rows: Vec<Row> =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = BTreeMap::new();
    for r in rows {
        let epoch = match &r.date {
            serde_json::Value::String(s) => s.parse::<i64>().map_err(|e| e.to_string())?,
            serde_json::Value::Number(n) => n.as_i64().ok_or("bad stablecoin date")?,
            other => return Err(format!("bad stablecoin date {other:?}")),
        };
        if let Some(v) = r.total.get("peggedUSD") {
            let day = chrono::DateTime::from_timestamp(epoch, 0)
                .ok_or("bad stablecoin ts")?
                .date_naive();
            out.insert(day, *v);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------
// matrix io + timing-model artifact
// ---------------------------------------------------------------------

#[derive(Serialize)]
struct Manifest<'a> {
    kind: &'static str,
    features: &'a [&'static str],
    labels: &'a [&'static str],
    n_rows: usize,
}

pub fn write_matrix(rows: &[MarketRow], out: &Path) -> Result<(), String> {
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut text = serde_json::to_string(&Manifest {
        kind: "market-matrix",
        features: &MARKET_FEATURES,
        labels: &LABELS,
        n_rows: rows.len(),
    })
    .map_err(|e| e.to_string())?;
    text.push('\n');
    for row in rows {
        text.push_str(&serde_json::to_string(row).map_err(|e| e.to_string())?);
        text.push('\n');
    }
    std::fs::write(out, text).map_err(|e| format!("{}: {e}", out.display()))
}

pub fn read_matrix(path: &Path) -> Result<Vec<MarketRow>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut lines = text.lines();
    let manifest: serde_json::Value = serde_json::from_str(
        lines
            .next()
            .ok_or_else(|| format!("{}: empty", path.display()))?,
    )
    .map_err(|e| e.to_string())?;
    if manifest["kind"] != "market-matrix" {
        return Err(format!("{}: not a market matrix", path.display()));
    }
    lines
        .map(|l| serde_json::from_str(l).map_err(|e| format!("{}: {e}", path.display())))
        .collect()
}

/// The timing-model artifact `training/train_directional.py` writes:
/// the market feature list, the label it was fit to, the label's training
/// standard deviation (for the clipped-z response), and the trees.
#[derive(Debug, Deserialize)]
pub struct TimingModel {
    pub format_version: String,
    pub model_version: String,
    pub features: Vec<String>,
    pub label: String,
    pub label_std: f64,
    pub trained_through: NaiveDate,
    pub tree_info: Vec<crate::model::Tree>,
}

impl TimingModel {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let model: TimingModel =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if model.format_version != crate::model::FORMAT_VERSION {
            return Err(format!(
                "{}: format {:?}, expected {:?}",
                path.display(),
                model.format_version,
                crate::model::FORMAT_VERSION
            ));
        }
        if model.model_version != "market-timing-1" {
            return Err(format!(
                "{}: model_version {:?}, expected \"market-timing-1\"",
                path.display(),
                model.model_version
            ));
        }
        if model.label_std <= 0.0 || !model.label_std.is_finite() {
            return Err(format!("{}: non-positive label_std", path.display()));
        }
        if model.tree_info.is_empty() {
            return Err(format!("{}: no trees", path.display()));
        }
        Ok(model)
    }

    /// Clipped-z response in [-1, 1] for the market row at `date`, refusing
    /// dates the model trained on - the same leakage guard the ranker has.
    pub fn response(&self, row: &MarketRow) -> Result<f64, String> {
        if row.date <= self.trained_through {
            return Err(format!(
                "timing model trained through {} cannot score {}: training contains the answer",
                self.trained_through, row.date
            ));
        }
        let values: Vec<f64> = self
            .features
            .iter()
            .map(|name| row.features.get(name).copied().unwrap_or(f64::NAN))
            .collect();
        let pred = self.tree_info.iter().try_fold(0.0, |sum, tree| {
            tree.tree_structure.predict(&values).map(|v| sum + v)
        })?;
        Ok((pred / self.label_std).clamp(-2.0, 2.0) / 2.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lagged_lookup_is_strictly_before_and_age_bounded() {
        let d = |s: &str| s.parse::<NaiveDate>().unwrap();
        let series = BTreeMap::from([(d("2024-01-01"), 1.0), (d("2024-01-05"), 2.0)]);
        assert_eq!(
            lagged(&series, d("2024-01-05"), 5),
            Some(1.0),
            "same-day excluded"
        );
        assert_eq!(lagged(&series, d("2024-01-06"), 5), Some(2.0));
        assert_eq!(lagged(&series, d("2024-01-12"), 5), None, "too stale");
    }
}
