use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use features_crypto::Bar;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Member {
    pub asset: String,
    pub rank: usize,
    pub eligible: bool,
    pub reason: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Snapshot {
    pub as_of: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
    pub source: String,
    pub members: Vec<Member>,
}

fn canonical(asset: &str) -> bool {
    !asset.is_empty()
        && asset.len() <= 20
        && asset
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    values.retain(|v| v.is_finite());
    if values.is_empty() {
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

pub fn by_liquidity(
    bars: &[Bar],
    as_of: DateTime<Utc>,
    top_n: usize,
    lookback_days: i64,
    min_history_bars: usize,
    min_turnover: f64,
    tradeable: Option<&BTreeSet<String>>,
) -> Result<Vec<Member>, String> {
    if top_n == 0 {
        return Err("top_n must be positive".into());
    }
    let visible: Vec<_> = bars.iter().filter(|b| b.ts_utc < as_of).collect();
    let Some(newest) = visible.iter().map(|b| b.ts_utc).max() else {
        return Ok(Vec::new());
    };
    let window_start = as_of - Duration::days(lookback_days);
    let stale_before = newest - Duration::days(lookback_days);
    let mut grouped: BTreeMap<&str, Vec<&Bar>> = BTreeMap::new();
    for bar in visible {
        grouped.entry(&bar.asset).or_default().push(bar);
    }

    let mut scored = Vec::new();
    for (asset, rows) in grouped {
        let last = rows.iter().map(|b| b.ts_utc).max().unwrap();
        let turnover = median(
            rows.iter()
                .filter(|b| b.ts_utc >= window_start)
                .filter_map(|b| b.quote_volume)
                .collect(),
        );
        let (eligible, reason) = if tradeable.is_some_and(|set| !set.contains(asset)) {
            (false, "not listed on the execution venue".into())
        } else if !canonical(asset) {
            (
                false,
                "asset id is not canonical; needs a venue alias (spec 5.2)".into(),
            )
        } else if last < stale_before {
            (
                false,
                format!("no bars since {}: delisted or halted", last.date_naive()),
            )
        } else if rows.len() < min_history_bars {
            (
                false,
                format!("{} bars, needs {min_history_bars}", rows.len()),
            )
        } else {
            match turnover {
                None => (false, "no turnover estimate in the lookback window".into()),
                Some(value) if value < min_turnover => (
                    false,
                    format!("median turnover {value:.0} below {min_turnover:.0}"),
                ),
                Some(value) => (true, format!("median {lookback_days}d turnover {value:.0}")),
            }
        };
        scored.push((
            asset.to_owned(),
            turnover.unwrap_or_default(),
            eligible,
            reason,
        ));
    }
    scored.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then_with(|| b.1.total_cmp(&a.1))
            .then_with(|| a.0.cmp(&b.0))
    });
    Ok(scored
        .into_iter()
        .enumerate()
        .map(|(i, (asset, _, mut eligible, mut reason))| {
            let rank = i + 1;
            if eligible && rank > top_n {
                eligible = false;
                reason = format!("rank {rank}, outside the top {top_n}");
            }
            Member {
                asset,
                rank,
                eligible,
                reason,
            }
        })
        .collect())
}

pub fn path(root: &Path, as_of: DateTime<Utc>) -> PathBuf {
    root.join("universe")
        .join(format!("{}.json", as_of.date_naive()))
}

pub fn write(
    root: &Path,
    as_of: DateTime<Utc>,
    source: &str,
    members: Vec<Member>,
    overwrite: bool,
) -> Result<PathBuf, String> {
    let path = path(root, as_of);
    if path.exists() && !overwrite {
        return Err(format!(
            "{} already exists; snapshots are append-only",
            path.display()
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let snap = Snapshot {
        as_of,
        recorded_at: Utc::now(),
        source: source.into(),
        members,
    };
    let value = serde_json::to_value(snap).map_err(|e| e.to_string())?;
    let mut bytes = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(path)
}

pub fn load(root: &Path, as_of: DateTime<Utc>) -> Result<Vec<Member>, String> {
    let path = path(root, as_of);
    let bytes = std::fs::read(&path)
        .map_err(|e| format!("cannot read universe {}: {e}", path.display()))?;
    let snap: Snapshot =
        serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(snap.members)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;

    fn bar(asset: &str, day: i64, qv: f64) -> Bar {
        let ts = "2026-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap() + TimeDelta::days(day);
        Bar {
            ts_utc: ts,
            asset: asset.into(),
            interval_s: 86_400,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1.0,
            quote_volume: Some(qv),
            trades: Some(1),
        }
    }

    #[test]
    fn ranks_by_trailing_median_and_keeps_rejections_visible() {
        let as_of = "2026-05-01T00:00:00Z".parse().unwrap();
        let mut bars = Vec::new();
        for day in 0..120 {
            bars.push(bar("AAA", day, 100.0));
            bars.push(bar("BBB", day, 200.0));
        }
        let out = by_liquidity(&bars, as_of, 1, 30, 90, 0.0, None).unwrap();
        assert_eq!(out[0].asset, "BBB");
        assert!(out[0].eligible);
        assert_eq!(out[1].asset, "AAA");
        assert!(!out[1].eligible);
        assert!(out[1].reason.contains("outside the top"));
    }
}
