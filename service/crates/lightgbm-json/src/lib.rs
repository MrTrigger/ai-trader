//! Broker- and strategy-neutral evaluator for LightGBM's JSON tree dump.
//!
//! Artifact identity, feature catalogues, leakage cutoffs, and reward semantics
//! remain with the consuming strategy. This crate owns only deterministic tree
//! evaluation so portfolio bots do not copy an inference engine.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    fn predict(&self, values: &[f64]) -> Result<f64, String> {
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

pub fn predict(trees: &[Tree], values: &[f64]) -> Result<f64, String> {
    if trees.is_empty() {
        return Err("model contains no trees".into());
    }
    trees.iter().try_fold(0.0, |sum, tree| {
        tree.tree_structure.predict(values).map(|value| sum + value)
    })
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
    fn evaluates_numeric_tree() {
        let tree = Tree {
            tree_index: 0,
            num_leaves: 2,
            num_cat: Some(0),
            shrinkage: Some(1.0),
            tree_structure: Node {
                split_feature: Some(0),
                threshold: Some(0.0),
                decision_type: Some("<=".into()),
                default_left: Some(true),
                missing_type: None,
                left_child: Some(Box::new(leaf(-0.1))),
                right_child: Some(Box::new(leaf(0.2))),
                leaf_value: None,
                diagnostics: Default::default(),
            },
        };
        assert_eq!(predict(std::slice::from_ref(&tree), &[-1.0]).unwrap(), -0.1);
        assert_eq!(predict(&[tree], &[1.0]).unwrap(), 0.2);
    }
}
