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
        let raw: Vec<String> = model
            .features
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

    pub fn predict(&self, values: &[f64], as_of: NaiveDate) -> Result<f64, String> {
        if as_of <= self.trained_through {
            return Err(format!(
                "model trained through {} cannot score {as_of}: training contains the answer",
                self.trained_through
            ));
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
