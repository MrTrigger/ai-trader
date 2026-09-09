use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Timelike, Utc};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::model::Model;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub effective_at: DateTime<Utc>,
    pub model_path: String,
    pub sha256: String,
    pub trained_through: NaiveDate,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn schedule_selects_only_active_models_and_rejects_invalid_evidence() {
        let dir = std::env::temp_dir().join(format!(
            "model-schedule-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let model_path = dir.join("model.json");
        let model = json!({
            "format_version":crate::model::FORMAT_VERSION,"model_version":crate::model::MODEL_VERSION,
            "feature_set_version":features_crypto::FEATURE_SET_VERSION,
            "trained_through":"2025-01-01","trained_at":"fixture","n_rows":12,"n_dates":1,
            "features":["x_ret_7"],"reward":"per_risk","label_horizon_hours":24,"label_lag_hours":1,
            "target_contract":crate::training::TARGET_CONTRACT,
            "tree_info":[{"tree_index":0,"num_leaves":1,"tree_structure":{"leaf_value":0.01}}]
        });
        let bytes = serde_json::to_vec(&model).unwrap();
        let sha = format!("{:x}", Sha256::digest(&bytes));
        std::fs::write(&model_path, bytes).unwrap();
        let entry = json!({"effective_at":"2025-01-04T00:00:00Z","model_path":model_path,"sha256":sha,"trained_through":"2025-01-01"});
        let path = dir.join("schedule.json");
        let document = json!({"schema":"lighter-model-schedule-1","entries":[entry]});
        let save = |value: &serde_json::Value| {
            std::fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap()
        };
        save(&document);
        let schedule = Schedule::load(&path).unwrap();
        assert!(schedule
            .at("2025-01-03T23:59:59Z".parse().unwrap())
            .is_err());
        assert_eq!(
            schedule
                .at("2025-01-04T00:00:00Z".parse().unwrap())
                .unwrap()
                .sha256,
            sha
        );
        for (key, value) in [
            ("effective_at", json!("2025-01-02T00:00:00Z")),
            ("effective_at", json!("2025-01-04T01:00:00Z")),
            ("trained_through", json!("2025-01-02")),
            ("sha256", json!("bad")),
        ] {
            let mut bad = document.clone();
            bad["entries"][0][key] = value;
            save(&bad);
            assert!(Schedule::load(&path).is_err(), "accepted invalid {key}");
        }
        let mut duplicate = document.clone();
        duplicate["entries"]
            .as_array_mut()
            .unwrap()
            .push(entry.clone());
        save(&duplicate);
        assert!(Schedule::load(&path).is_err());
        let mut next = entry;
        next["effective_at"] = json!("2025-02-01T00:00:00Z");
        duplicate["entries"][1] = next;
        save(&duplicate);
        let schedule = Schedule::load(&path).unwrap();
        assert_eq!(
            schedule
                .at("2025-01-31T23:59:59Z".parse().unwrap())
                .unwrap()
                .effective_at,
            "2025-01-04T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(
            schedule
                .at("2025-02-01T00:00:00Z".parse().unwrap())
                .unwrap()
                .effective_at,
            "2025-02-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[derive(Debug, Clone)]
pub struct Schedule {
    pub source: PathBuf,
    pub entries: Vec<Entry>,
}

impl Schedule {
    pub fn load(path: &Path) -> Result<Self, String> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Document {
            schema: String,
            entries: Vec<Entry>,
        }
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let document: Document = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if document.schema != "lighter-model-schedule-1" || document.entries.is_empty() {
            return Err("expected a nonempty lighter-model-schedule-1".into());
        }
        let mut previous = None;
        for entry in &document.entries {
            if entry.effective_at.time().num_seconds_from_midnight() != 0
                || entry.effective_at.timestamp_subsec_nanos() != 0
                || previous.is_some_and(|at| entry.effective_at <= at)
            {
                return Err("model activations must be strictly ordered UTC midnights".into());
            }
            let bytes = std::fs::read(&entry.model_path).map_err(|e| e.to_string())?;
            if format!("{:x}", Sha256::digest(bytes)) != entry.sha256 {
                return Err(format!(
                    "{}: scheduled model checksum mismatch",
                    entry.model_path
                ));
            }
            let model = Model::load(Path::new(&entry.model_path))?;
            if model.trained_through != entry.trained_through
                || model.label_horizon_hours.is_none()
                || model.label_lag_hours.is_none()
                || model.target_contract.as_deref() != Some(crate::training::TARGET_CONTRACT)
            {
                return Err("scheduled model requires matching training cutoff and explicit Rust label contract".into());
            }
            model.ensure_available(entry.effective_at.date_naive())?;
            previous = Some(entry.effective_at);
        }
        Ok(Self {
            source: path.to_owned(),
            entries: document.entries,
        })
    }

    pub fn at(&self, at: DateTime<Utc>) -> Result<&Entry, String> {
        self.entries
            .iter()
            .rev()
            .find(|entry| entry.effective_at <= at)
            .ok_or_else(|| format!("no scheduled model active at {at}"))
    }
}
