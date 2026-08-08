//! Point-in-time cross-sectional scores for inspection, never feature creation.

use std::collections::{BTreeMap, BTreeSet};

use features_crypto::DailyRow;

pub const SCORING_VERSION: &str = "sc-phase1-1";
pub const NEUTRAL: f64 = 50.0;
pub const DEFAULT_MIN_GROUP_SIZE: usize = 5;
pub const UNGROUPED: &str = "all";

#[derive(Debug, Clone, serde::Serialize)]
pub struct ScoreRow {
    pub asset: String,
    pub group_key: String,
    pub momentum: f64,
    pub low_vol: f64,
    pub liquidity: f64,
    pub composite: f64,
    pub degenerate_flags: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ScoreResult {
    pub scoring_version: &'static str,
    pub rows: Vec<ScoreRow>,
    pub disclosures: Vec<String>,
}

#[derive(Clone, Copy)]
struct SubFactor {
    name: &'static str,
    higher_is_better: bool,
}

fn percentiles(values: &[(usize, f64)], higher_is_better: bool) -> Vec<(usize, f64)> {
    let mut ordered = values.to_vec();
    ordered.sort_by(|left, right| left.1.total_cmp(&right.1));
    let n = ordered.len() as f64;
    let mut out = Vec::with_capacity(ordered.len());
    let mut start = 0;
    while start < ordered.len() {
        let mut end = start + 1;
        while end < ordered.len() && ordered[end].1 == ordered[start].1 {
            end += 1;
        }
        let rank = ((start + 1) as f64 + end as f64) / 2.0;
        let value = 100.0 * (rank - 0.5) / n;
        let value = if higher_is_better {
            value
        } else {
            100.0 - value
        };
        for (index, _) in &ordered[start..end] {
            out.push((*index, value));
        }
        start = end;
    }
    out
}

pub fn baseline(
    cross: &[&DailyRow],
    groups: Option<&BTreeMap<String, String>>,
    min_group_size: usize,
) -> ScoreResult {
    let mut source = cross.to_vec();
    source.sort_by(|left, right| left.asset.cmp(&right.asset));
    let group_keys: Vec<_> = source
        .iter()
        .map(|row| {
            groups
                .and_then(|values| values.get(&row.asset))
                .cloned()
                .unwrap_or_else(|| UNGROUPED.into())
        })
        .collect();
    let mut members: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, group) in group_keys.iter().enumerate() {
        members.entry(group).or_default().push(index);
    }
    let small: BTreeSet<_> = members
        .iter()
        .filter(|(_, values)| values.len() < min_group_size)
        .map(|(group, _)| *group)
        .collect();
    let mut disclosures = Vec::new();
    for group in &small {
        let assets = members[group]
            .iter()
            .map(|index| source[*index].asset.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        disclosures.push(format!(
            "group {group:?} has {} asset(s), fewer than the {min_group_size} a percentile rank needs: {assets} scored neutral",
            members[group].len()
        ));
    }
    let factors = [
        SubFactor {
            name: "ret_30",
            higher_is_better: true,
        },
        SubFactor {
            name: "ret_90",
            higher_is_better: true,
        },
        SubFactor {
            name: "vol_30",
            higher_is_better: false,
        },
        SubFactor {
            name: "adv_quote",
            higher_is_better: true,
        },
    ];
    let mut scored = vec![vec![NEUTRAL; factors.len()]; source.len()];
    let mut flags = vec![Vec::new(); source.len()];
    for (factor_index, factor) in factors.iter().enumerate() {
        for (group, indexes) in &members {
            if small.contains(group) {
                for index in indexes {
                    flags[*index].push(format!("{}:small_group", factor.name));
                }
                continue;
            }
            let measured: Vec<_> = indexes
                .iter()
                .filter_map(|index| {
                    source[*index]
                        .value(factor.name)
                        .map(|value| (*index, value))
                })
                .collect();
            for index in indexes {
                if source[*index].value(factor.name).is_none() {
                    flags[*index].push(format!("{}:no_measurement", factor.name));
                    disclosures.push(format!(
                        "{} is missing for {}: scored neutral",
                        factor.name, source[*index].asset
                    ));
                }
            }
            for (index, value) in percentiles(&measured, factor.higher_is_better) {
                scored[index][factor_index] = value;
            }
        }
    }
    let mut rows = source
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let momentum = (scored[index][0] + scored[index][1]) / 2.0;
            let low_vol = scored[index][2];
            let liquidity = scored[index][3];
            ScoreRow {
                asset: row.asset.clone(),
                group_key: group_keys[index].clone(),
                momentum,
                low_vol,
                liquidity,
                composite: (2.0 * momentum + low_vol + liquidity) / 4.0,
                degenerate_flags: flags[index].clone(),
            }
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .composite
            .total_cmp(&left.composite)
            .then_with(|| left.asset.cmp(&right.asset))
    });
    disclosures.sort();
    disclosures.dedup();
    ScoreResult {
        scoring_version: SCORING_VERSION,
        rows,
        disclosures,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    fn row(asset: &str, value: Option<f64>) -> DailyRow {
        DailyRow {
            ts_utc: "2025-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap(),
            asset: asset.into(),
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            mark_open: 1.0,
            mark_close: 1.0,
            had_discontinuity: false,
            bars_available: 100,
            perp_listed: true,
            ret_1: value,
            ret_7: value,
            ret_30: value,
            ret_90: value,
            ret_30_skip_7: value,
            vol_30: value,
            adv_quote: value,
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
            beta_bench: value,
            gc_filter: None,
            gc_upper: None,
            gc_lower: None,
            gc_breakout_age: None,
            gc_regime_filter: None,
            gc_regime_upper: None,
            gc_regime_slope: None,
        }
    }

    #[test]
    fn ties_are_order_independent_and_missing_is_neutral() {
        let rows = [
            row("A", Some(1.0)),
            row("B", Some(1.0)),
            row("C", Some(3.0)),
            row("D", Some(4.0)),
            row("E", None),
        ];
        let refs = rows.iter().collect::<Vec<_>>();
        let result = baseline(&refs, None, 5);
        assert_eq!(
            result
                .rows
                .iter()
                .find(|row| row.asset == "A")
                .unwrap()
                .momentum,
            25.0
        );
        assert_eq!(
            result
                .rows
                .iter()
                .find(|row| row.asset == "B")
                .unwrap()
                .momentum,
            25.0
        );
        let missing = result.rows.iter().find(|row| row.asset == "E").unwrap();
        assert_eq!(missing.composite, 50.0);
        assert!(!missing.degenerate_flags.is_empty());
    }
}
