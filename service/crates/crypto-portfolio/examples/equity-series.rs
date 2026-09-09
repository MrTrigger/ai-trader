//! Chain walk-forward fold NAVs into one continuous equity curve with running
//! drawdown, for plotting. Financial arithmetic stays here; the plotter draws.
//!
//! usage: equity-series OUT_JSON LABEL=FOLD_DIR [LABEL=FOLD_DIR ...]
//!
//! Every fold restarts at its own initial cash. Chaining multiplies the
//! previous fold's ending value by each step's NAV / initial cash, which is
//! exactly the compounding the walk-forward summary reports. Drawdown is
//! NAV / running peak - 1 on the chained series.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

fn number(value: &Value) -> Result<f64, String> {
    match value {
        Value::Number(n) => n.as_f64().ok_or_else(|| "non-finite nav".into()),
        Value::String(s) => s.parse::<f64>().map_err(|e| format!("nav {s:?}: {e}")),
        other => Err(format!("unexpected nav value {other}")),
    }
}

fn series(dir: &Path) -> Result<(Vec<String>, Vec<f64>, Vec<f64>, Vec<Value>), String> {
    let mut paths: Vec<_> = fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("fold-") && name.ends_with(".json"))
        })
        .collect();
    paths.sort_by_key(|path| {
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.trim_start_matches("fold-").parse::<usize>().ok())
            .unwrap_or(usize::MAX)
    });
    if paths.is_empty() {
        return Err(format!("no fold files in {}", dir.display()));
    }
    let mut dates = Vec::new();
    let mut nav = Vec::new();
    let mut sources = Vec::new();
    let mut chained = 1.0;
    for path in paths {
        let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let initial = number(&value["initial_cash"])?;
        let steps = value["steps"].as_array().ok_or("missing steps")?;
        let mut last = chained;
        for step in steps {
            let as_of = step["as_of"].as_str().ok_or("missing as_of")?;
            if let Some(previous) = dates.last() {
                if previous >= &as_of.to_string() {
                    return Err(format!("non-increasing date {as_of} in {}", path.display()));
                }
            }
            last = chained * number(&step["nav"])? / initial;
            dates.push(as_of.to_string());
            nav.push(last);
        }
        chained = last;
        sources.push(json!({"file": path, "sha256": format!("{:x}", Sha256::digest(&bytes))}));
    }
    let mut peak = f64::MIN;
    let drawdown = nav
        .iter()
        .map(|value| {
            peak = peak.max(*value);
            value / peak - 1.0
        })
        .collect();
    Ok((dates, nav, drawdown, sources))
}

fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        return Err("usage: equity-series OUT_JSON LABEL=FOLD_DIR [LABEL=FOLD_DIR ...]".into());
    }
    let out = Path::new(&args[0]);
    let mut grid: Option<Vec<String>> = None;
    let mut curves = serde_json::Map::new();
    for spec in &args[1..] {
        let (label, dir) = spec
            .split_once('=')
            .ok_or_else(|| format!("expected LABEL=DIR, got {spec:?}"))?;
        let (dates, nav, drawdown, sources) = series(Path::new(dir))?;
        match &grid {
            None => grid = Some(dates.clone()),
            Some(existing) if existing != &dates => {
                return Err(format!("{label}: date grid differs from the first series"));
            }
            _ => {}
        }
        let max_drawdown = drawdown.iter().cloned().fold(0.0, f64::min);
        let observations = nav.len();
        curves.insert(
            label.to_string(),
            json!({
                "sources": sources,
                "total_return": nav.last().copied().unwrap_or(1.0) - 1.0,
                "max_drawdown": max_drawdown,
                "observations": observations,
                "growth_of_one": nav,
                "drawdown": drawdown,
            }),
        );
    }
    let dates = grid.ok_or("no series")?;
    let document = json!({
        "schema": "equity-series-1",
        "method": "Fold NAVs chained multiplicatively at their own initial cash; drawdown is value over running peak minus one. No resampling, no annualisation.",
        "first_date": dates.first(),
        "last_date": dates.last(),
        "dates": dates,
        "curves": curves,
    });
    fs::write(out, serde_json::to_vec_pretty(&document).map_err(|e| e.to_string())?)
        .map_err(|e| format!("{}: {e}", out.display()))?;
    for (label, curve) in &curves {
        println!(
            "{label}\treturn {:.4}\tmax drawdown {:.4}\tobservations {}",
            curve["total_return"].as_f64().unwrap_or(f64::NAN),
            curve["max_drawdown"].as_f64().unwrap_or(f64::NAN),
            curve["observations"]
        );
    }
    Ok(())
}
