//! Auditable LightGBM runtime format and inference.
//!
//! Python may fit the model, but it receives an already computed and
//! rank-normalised matrix from `features-crypto`. The only runtime artifact is
//! JSON containing metadata, ordered feature names, and LightGBM's tree dump.

use std::path::Path;

use chrono::NaiveDate;
use sha2::{Digest, Sha256};

pub const FORMAT_VERSION: &str = "crypto-lightgbm-json-1";
pub const MODEL_VERSION: &str = "ml-ranker-rust-features-1";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub format_version: String,
    pub model_version: String,
    pub feature_set_version: String,
    pub trained_through: NaiveDate,
    pub trained_at: String,
    pub n_rows: usize,
    pub n_dates: usize,
    pub features: Vec<String>,
    /// What the trees were fit to predict. "return" is the documented reward,
    /// demean(ret); "per_risk" is demean(ret/vol), whose output is already in
    /// return-per-unit-risk units. Inference has to know which, because a
    /// per-risk score divided by volatility again would double-count risk and
    /// quietly rebuild the concentration the sizing study removed.
    /// "per_risk_abs" is ret/vol NOT demeaned within date (the sign carries
    /// market direction; docs/research/absolute-label-unbalanced.md) and is
    /// converted exactly like "per_risk". Absent on artefacts fit before the
    /// field existed, all of which were "return".
    #[serde(default = "default_reward")]
    pub reward: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_horizon_hours: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_lag_hours: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_contract: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_context: Option<String>,
    pub tree_info: Vec<Tree>,
    #[serde(skip)]
    pub model_id: String,
}

fn default_reward() -> String {
    "return".into()
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tree {
    pub tree_index: usize,
    pub num_leaves: usize,
    #[serde(default)]
    pub num_cat: Option<usize>,
    #[serde(default)]
    pub shrinkage: Option<f64>,
    pub tree_structure: Node,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Node {
    #[serde(default)]
    pub split_feature: Option<usize>,
    #[serde(default)]
    pub threshold: Option<f64>,
    #[serde(default)]
    pub decision_type: Option<String>,
    #[serde(default)]
    pub default_left: Option<bool>,
    #[serde(default)]
    pub missing_type: Option<String>,
    #[serde(default)]
    pub left_child: Option<Box<Node>>,
    #[serde(default)]
    pub right_child: Option<Box<Node>>,
    #[serde(default)]
    pub leaf_value: Option<f64>,
    #[serde(flatten)]
    pub diagnostics: std::collections::BTreeMap<String, serde_json::Value>,
}

impl Node {
    pub(crate) fn predict(&self, values: &[f64]) -> Result<f64, String> {
        if let Some(value) = self.leaf_value {
            return Ok(value);
        }
        let feature = self
            .split_feature
            .ok_or("tree node has neither split nor leaf")?;
        let value = *values
            .get(feature)
            .ok_or_else(|| format!("tree references absent feature {feature}"))?;
        let go_left = if value.is_nan() {
            self.default_left.unwrap_or(true)
        } else {
            match self.decision_type.as_deref().unwrap_or("<=") {
                "<=" => value <= self.threshold.ok_or("numeric split has no threshold")?,
                other => return Err(format!("unsupported LightGBM decision_type {other:?}")),
            }
        };
        let child = if go_left {
            &self.left_child
        } else {
            &self.right_child
        };
        child
            .as_deref()
            .ok_or("split node is missing a child")?
            .predict(values)
    }
}

impl Model {
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path)
            .map_err(|e| format!("cannot read model {}: {e}", path.display()))?;
        let mut model: Self =
            serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        if model.label_horizon_hours.is_some_and(|hours| hours <= 0) {
            return Err("model label_horizon_hours must be positive".into());
        }
        if model.label_lag_hours.is_some_and(|hours| hours < 0) {
            return Err("model label_lag_hours must be nonnegative".into());
        }
        if model
            .target_contract
            .as_deref()
            .is_some_and(|contract| contract != crate::training::TARGET_CONTRACT)
        {
            return Err("unsupported model target_contract".into());
        }
        if model.format_version != FORMAT_VERSION {
            return Err(format!(
                "model format {:?}, expected {FORMAT_VERSION:?}",
                model.format_version
            ));
        }
        if model.model_version != MODEL_VERSION {
            return Err(format!(
                "model version {:?}, expected {MODEL_VERSION:?}",
                model.model_version
            ));
        }
        if model.feature_set_version != features_crypto::FEATURE_SET_VERSION {
            return Err(format!(
                "model feature set {:?}, runtime is {:?}; retrain from Rust-emitted features",
                model.feature_set_version,
                features_crypto::FEATURE_SET_VERSION
            ));
        }
        let ranked_count =
            crate::market_context::ranked_count(&model.features, model.market_context.as_deref())?;
        let raw: Vec<String> = model.features[..ranked_count]
            .iter()
            .map(|name| {
                name.strip_prefix("x_")
                    .ok_or_else(|| {
                        format!("model input {name:?} is not rank-normalised (x_ prefix missing)")
                    })
                    .map(str::to_owned)
            })
            .collect::<Result<_, _>>()?;
        features_crypto::validate_selection(&raw)?;
        if model.tree_info.is_empty() {
            return Err("model contains no trees".into());
        }
        model.model_id = format!("{:x}", Sha256::digest(&bytes))[..16].to_owned();
        Ok(model)
    }

    pub fn ranked_features(&self) -> &[String] {
        let count = self.features.len()
            - if self.market_context.is_some() {
                crate::market_context::FEATURES.len()
            } else {
                0
            };
        &self.features[..count]
    }

    pub fn predict(&self, values: &[f64], as_of: NaiveDate) -> Result<f64, String> {
        self.ensure_available(as_of)?;
        if values.len() != self.features.len() {
            return Err(format!(
                "model expects {} inputs, received {}",
                self.features.len(),
                values.len()
            ));
        }
        self.tree_info.iter().try_fold(0.0, |sum, tree| {
            tree.tree_structure.predict(values).map(|v| sum + v)
        })
    }

    pub fn ensure_available(&self, as_of: NaiveDate) -> Result<(), String> {
        let days = match self.label_horizon_hours {
            Some(horizon) => {
                horizon
                    .checked_add(self.label_lag_hours.unwrap_or(0))
                    // trained_through is a date; a training row may use any hour
                    // on it. Allow 23h for that slot, then ceil to a decision day.
                    .and_then(|hours| hours.checked_add(46))
                    .ok_or("model label window overflows")?
                    / 24
            }
            None => 1,
        };
        let first_score_date = self
            .trained_through
            .checked_add_days(chrono::Days::new(days.max(1) as u64))
            .ok_or("model label window exceeds calendar")?;
        if as_of < first_score_date {
            return Err(format!(
                "model trained through {} cannot score {as_of}: training contains the answer until {first_score_date}",
                self.trained_through
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn market_context_contract_requires_fixed_raw_suffix_and_valid_ranked_prefix() {
        let path =
            std::env::temp_dir().join(format!("crypto-market-schema-{}.json", std::process::id()));
        let mut doc = serde_json::json!({
            "format_version":FORMAT_VERSION,"model_version":MODEL_VERSION,
            "feature_set_version":features_crypto::FEATURE_SET_VERSION,
            "trained_through":"2025-01-01","trained_at":"fixture","n_rows":12,"n_dates":1,
            "features":["x_ret_7"],"reward":"return",
            "tree_info":[{"tree_index":0,"num_leaves":1,"tree_structure":{"leaf_value":0.01}}]
        });
        let save = |doc: &serde_json::Value| {
            std::fs::write(&path, serde_json::to_vec(doc).unwrap()).unwrap()
        };
        save(&doc);
        let legacy = Model::load(&path).unwrap();
        assert_eq!(legacy.market_context, None);
        assert!(serde_json::to_value(&legacy)
            .unwrap()
            .get("market_context")
            .is_none());
        let features: Vec<_> = std::iter::once("x_ret_7")
            .chain(crate::market_context::FEATURES)
            .collect();
        doc["features"] = serde_json::json!(features);
        save(&doc);
        assert!(Model::load(&path).is_err());
        doc["market_context"] = crate::market_context::CONTRACT.into();
        save(&doc);
        let context = Model::load(&path).unwrap();
        assert_eq!(context.ranked_features(), ["x_ret_7"]);
        for (key, value) in [
            ("market_context", serde_json::json!("unknown")),
            ("features", serde_json::json!(["x_ret_7", "mkt_btc_ret_7d"])),
            (
                "features",
                serde_json::json!([
                    "x_unknown",
                    "mkt_btc_ret_7d",
                    "mkt_btc_ret_30d",
                    "mkt_btc_ret_90d",
                    "mkt_btc_vol_30d"
                ]),
            ),
            (
                "features",
                serde_json::json!([
                    "mkt_btc_ret_7d",
                    "mkt_btc_ret_30d",
                    "mkt_btc_ret_90d",
                    "mkt_btc_vol_30d",
                    "x_ret_7"
                ]),
            ),
        ] {
            let mut bad = doc.clone();
            bad[key] = value;
            save(&bad);
            assert!(Model::load(&path).is_err());
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn label_metadata_accepts_legacy_and_rejects_invalid_explicit_values() {
        let path =
            std::env::temp_dir().join(format!("crypto-label-metadata-{}.json", std::process::id()));
        let baseline = serde_json::json!({
            "format_version": FORMAT_VERSION, "model_version": MODEL_VERSION,
            "feature_set_version": features_crypto::FEATURE_SET_VERSION,
            "trained_through": "2025-01-01", "trained_at": "fixture", "n_rows": 12,
            "n_dates": 1, "features": ["x_ret_7"], "reward": "return",
            "tree_info": [{"tree_index": 0, "num_leaves": 1, "tree_structure": {"leaf_value": 0.01}}]
        });
        std::fs::write(&path, serde_json::to_vec(&baseline).unwrap()).unwrap();
        let legacy = Model::load(&path).unwrap();
        assert_eq!(legacy.label_horizon_hours, None);
        assert_eq!(legacy.label_lag_hours, None);
        assert_eq!(legacy.target_contract, None);
        for horizon in [24, 72] {
            let mut metadata = baseline.clone();
            metadata["label_horizon_hours"] = horizon.into();
            metadata["label_lag_hours"] = 1.into();
            metadata["target_contract"] = crate::training::TARGET_CONTRACT.into();
            std::fs::write(&path, serde_json::to_vec(&metadata).unwrap()).unwrap();
            let model = Model::load(&path).unwrap();
            assert_eq!(model.label_horizon_hours, Some(horizon));
            assert_eq!(model.label_lag_hours, Some(1));
            assert_eq!(
                model.target_contract.as_deref(),
                Some(crate::training::TARGET_CONTRACT)
            );
        }
        for (key, value) in [
            ("label_horizon_hours", serde_json::json!(0)),
            ("label_horizon_hours", serde_json::json!(-1)),
            ("label_horizon_hours", serde_json::json!(24.5)),
            ("label_lag_hours", serde_json::json!(-1)),
            ("target_contract", serde_json::json!("unknown")),
        ] {
            let mut metadata = baseline.clone();
            metadata[key] = value;
            std::fs::write(&path, serde_json::to_vec(&metadata).unwrap()).unwrap();
            assert!(Model::load(&path).is_err());
        }
        std::fs::remove_file(path).unwrap();
    }

    fn leaf(value: f64) -> Node {
        Node {
            split_feature: None,
            threshold: None,
            decision_type: None,
            default_left: None,
            missing_type: None,
            left_child: None,
            right_child: None,
            leaf_value: Some(value),
            diagnostics: Default::default(),
        }
    }

    #[test]
    fn evaluates_numeric_trees_and_enforces_cutoff() {
        let root = Node {
            split_feature: Some(0),
            threshold: Some(0.0),
            decision_type: Some("<=".into()),
            default_left: Some(true),
            missing_type: Some("None".into()),
            left_child: Some(Box::new(leaf(-0.2))),
            right_child: Some(Box::new(leaf(0.3))),
            leaf_value: None,
            diagnostics: Default::default(),
        };
        let model = Model {
            format_version: FORMAT_VERSION.into(),
            model_version: MODEL_VERSION.into(),
            feature_set_version: features_crypto::FEATURE_SET_VERSION.into(),
            trained_through: "2025-01-01".parse().unwrap(),
            trained_at: "fixture".into(),
            reward: default_reward(),
            label_horizon_hours: None,
            label_lag_hours: None,
            target_contract: None,
            market_context: None,
            n_rows: 1,
            n_dates: 1,
            features: vec!["x_ret_7".into()],
            tree_info: vec![Tree {
                tree_index: 0,
                num_leaves: 2,
                num_cat: Some(0),
                shrinkage: Some(1.0),
                tree_structure: root,
            }],
            model_id: "fixture".into(),
        };
        assert_eq!(
            model
                .predict(&[-1.0], "2025-01-02".parse().unwrap())
                .unwrap(),
            -0.2
        );
        assert_eq!(
            model
                .predict(&[1.0], "2025-01-02".parse().unwrap())
                .unwrap(),
            0.3
        );
        assert!(model
            .predict(&[1.0], "2025-01-01".parse().unwrap())
            .is_err());
        for (horizon, last_blocked, first_allowed) in [
            (24, "2025-01-02", "2025-01-03"),
            (72, "2025-01-04", "2025-01-05"),
        ] {
            let mut labeled = model.clone();
            labeled.label_horizon_hours = Some(horizon);
            labeled.label_lag_hours = Some(1);
            assert!(labeled
                .predict(&[1.0], last_blocked.parse().unwrap())
                .is_err());
            assert_eq!(
                labeled
                    .predict(&[1.0], first_allowed.parse().unwrap())
                    .unwrap(),
                0.3
            );
        }
    }
}
