//! The declared seven-day timer changes allocation, never ranker features.
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use chrono::{Duration, NaiveDate};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::directional::{self, MarketRow, TimingModel};

const FEATURES: [&str; 7] = [
    "mkt_ret_7",
    "mkt_ret_30",
    "mkt_ret_90",
    "mkt_vol_30",
    "breadth_30",
    "breadth_90",
    "fund_mean_7d",
];

#[derive(Deserialize)]
struct Provenance {
    training_profile: String,
    training_last_date: NaiveDate,
    training_label_end: NaiveDate,
    purge_days: u32,
    matrix_sha256: String,
}

struct Inputs {
    model: TimingModel,
    rows: BTreeMap<NaiveDate, MarketRow>,
}

#[derive(PartialEq, Eq)]
struct Stamp {
    modified: SystemTime,
    len: u64,
}
fn stamp(path: &Path) -> Result<Stamp, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Stamp {
        modified: meta.modified().map_err(|e| e.to_string())?,
        len: meta.len(),
    })
}
struct Cached {
    model_stamp: Stamp,
    matrix_stamp: Stamp,
    inputs: Arc<Inputs>,
}
type Cache = HashMap<(PathBuf, PathBuf), Cached>;
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

fn load(model_path: &Path, matrix_path: &Path) -> Result<Inputs, String> {
    let model = TimingModel::load(model_path)?;
    let meta: Provenance =
        serde_json::from_slice(&std::fs::read(model_path).map_err(|e| e.to_string())?)
            .map_err(|e| format!("timer provenance: {e}"))?;
    if meta.training_profile != "selection-timer-7d-v1"
        || model.label != "fwd_z_7"
        || model.features.iter().map(String::as_str).ne(FEATURES)
        || model.tree_info.len() > 50
        || model.tree_info.iter().any(|t| t.num_leaves > 2)
    {
        return Err("timer is not the declared seven-feature selection-timer-7d-v1 profile".into());
    }
    if meta.purge_days != 9
        || meta.training_label_end != meta.training_last_date + Duration::days(7)
        || meta.training_label_end > model.trained_through - Duration::days(2)
    {
        return Err("timer training labels cross the purged cutoff".into());
    }
    let matrix_bytes = std::fs::read(matrix_path).map_err(|e| e.to_string())?;
    if format!("{:x}", Sha256::digest(&matrix_bytes)) != meta.matrix_sha256 {
        return Err("timer matrix differs from its recorded training input".into());
    }
    let matrix = directional::read_matrix(matrix_path)?;
    if matrix.is_empty() || matrix.windows(2).any(|w| w[1].date <= w[0].date) {
        return Err("timer matrix must have strictly increasing dates".into());
    }
    Ok(Inputs {
        model,
        rows: matrix.into_iter().map(|r| (r.date, r)).collect(),
    })
}

fn response(inputs: &Inputs, score_date: NaiveDate) -> Result<f64, String> {
    let (_, row) = inputs
        .rows
        .range(..=score_date)
        .next_back()
        .ok_or("timer has no market row at the score date")?;
    if (score_date - row.date).num_days() > 3 {
        return Err("timer market row is more than three days stale".into());
    }
    // Labels in a full market matrix are never model inputs.
    let result = inputs.model.response(row)?;
    if !result.is_finite() {
        return Err("timer response is non-finite".into());
    }
    Ok(result)
}

pub fn predict(model_path: &str, matrix_path: &str, score_date: NaiveDate) -> Result<f64, String> {
    let model_path = Path::new(model_path)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let matrix_path = Path::new(matrix_path)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let model_stamp = stamp(&model_path)?;
    let matrix_stamp = stamp(&matrix_path)?;
    let key = (model_path, matrix_path);
    let inputs = {
        let mut cache = CACHE
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| "timer cache poisoned")?;
        if cache
            .get(&key)
            .is_none_or(|v| v.model_stamp != model_stamp || v.matrix_stamp != matrix_stamp)
        {
            let inputs = Arc::new(load(&key.0, &key.1)?);
            if stamp(&key.0)? != model_stamp || stamp(&key.1)? != matrix_stamp {
                return Err("timer input changed while loading".into());
            }
            cache.insert(
                key.clone(),
                Cached {
                    model_stamp,
                    matrix_stamp,
                    inputs,
                },
            );
        }
        Arc::clone(
            &cache
                .get(&key)
                .ok_or("timer cache entry is missing")?
                .inputs,
        )
    };
    response(&inputs, score_date)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fn fixtures() -> (PathBuf, PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "selection-timer-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let matrix = dir.join("matrix.jsonl");
        let model = dir.join("model.json");
        let rows = vec![MarketRow {
            date: "2024-01-02".parse().unwrap(),
            features: BTreeMap::new(),
            labels: BTreeMap::from([("fwd_z_7".into(), 100000.0)]),
        }];
        directional::write_matrix(&rows, &matrix).unwrap();
        std::fs::write(&model, json!({
            "format_version":"crypto-lightgbm-json-1", "model_version":"market-timing-1",
            "features": FEATURES, "label":"fwd_z_7", "label_std":1.0, "trained_through":"2024-01-01",
            "training_profile":"selection-timer-7d-v1", "training_last_date":"2023-12-23", "training_label_end":"2023-12-30", "purge_days":9,
            "matrix_sha256":format!("{:x}",Sha256::digest(std::fs::read(&matrix).unwrap())),
            "tree_info":[{"tree_index":0,"num_leaves":1,"tree_structure":{"leaf_value":0.5}}]
        }).to_string()).unwrap();
        (dir, model, matrix)
    }
    #[test]
    fn cached_response_refuses_missing_stale_and_training_dates() {
        let (dir, model, matrix) = fixtures();
        let input = load(&model, &matrix).unwrap();
        assert!(response(&input, "2024-01-01".parse().unwrap()).is_err());
        assert!(response(&input, "2024-01-06".parse().unwrap()).is_err());
        for _ in 0..2 {
            assert_eq!(
                predict(
                    model.to_str().unwrap(),
                    matrix.to_str().unwrap(),
                    "2024-01-02".parse().unwrap()
                )
                .unwrap(),
                0.25
            );
        }
        let mut leaked = input;
        leaked.model.trained_through = "2024-01-02".parse().unwrap();
        assert!(response(&leaked, "2024-01-02".parse().unwrap()).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn matrix_mutation_and_future_training_labels_are_rejected() {
        let (dir, model, matrix) = fixtures();
        let mut doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&model).unwrap()).unwrap();
        doc["training_label_end"] = json!("2024-01-05");
        std::fs::write(&model, doc.to_string()).unwrap();
        assert!(load(&model, &matrix)
            .err()
            .unwrap()
            .contains("purged cutoff"));
        doc["training_label_end"] = json!("2023-12-30");
        std::fs::write(&model, doc.to_string()).unwrap();
        std::fs::write(&matrix, "changed").unwrap();
        assert!(load(&model, &matrix)
            .err()
            .unwrap()
            .contains("training input"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
