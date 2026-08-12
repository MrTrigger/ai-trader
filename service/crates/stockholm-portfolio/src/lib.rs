//! Stockholm-specific model contract and no-direction-quota replay policy.
//!
//! Provider access lives in `equity-data`, feature/matrix semantics in
//! `features-stockholm`, and generic LightGBM evaluation in `lightgbm-json`.

use std::collections::BTreeMap;
use std::path::Path;

use equity_data::BenchmarkHistory;
use features_stockholm::{DirectionTrainingRow, TrainingRow, UniverseBucket};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::Date;

pub const FORMAT_VERSION: &str = "stockholm-model-json-2";
pub const LEGACY_FORMAT_VERSION: &str = "stockholm-lightgbm-json-1";
pub const MODEL_VERSION: &str = "stockholm-ranker-1";
pub const DIRECTION_FORMAT_VERSION: &str = "stockholm-direction-model-json-1";
pub const DIRECTION_MODEL_VERSION: &str = "stockholm-direction-model-1";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredictionComposition {
    /// Consume the stock model in the return space declared by its reward.
    #[default]
    Direct,
    /// Preserve an absolute model's cross-sectional ordering, remove its
    /// same-date equal-weight market component, and add a separately fitted
    /// causal OMXSGI forecast before choosing a side.
    CrossSectionalResidualPlusMarket,
}

fn default_model_family() -> String {
    "lightgbm".into()
}

fn default_reward() -> String {
    "absolute_return".into()
}

fn default_objective() -> String {
    "l1".into()
}

fn default_direction_objective() -> String {
    "l2".into()
}

fn default_ensemble_seeds() -> usize {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub format_version: String,
    pub model_version: String,
    pub feature_set_version: String,
    pub label_version: String,
    #[serde(with = "date_serde")]
    pub trained_through: Date,
    pub trained_at: String,
    pub n_rows: usize,
    pub n_dates: usize,
    pub features: Vec<String>,
    pub survivorship_status: String,
    #[serde(default = "default_model_family")]
    pub model_family: String,
    #[serde(default = "default_reward")]
    pub reward: String,
    #[serde(default = "default_objective")]
    pub objective: String,
    #[serde(default = "default_ensemble_seeds")]
    pub ensemble_seeds: usize,
    #[serde(default)]
    pub target_clip: Option<[f64; 2]>,
    /// Training-prefix conversion from a dimensionless rank score to expected
    /// relative return. Present only for the relative-rank reward.
    #[serde(default)]
    pub reward_scale: Option<f64>,
    #[serde(default)]
    pub ridge_lambda: Option<f64>,
    #[serde(default)]
    pub linear_intercept: Option<f64>,
    #[serde(default)]
    pub linear_weights: Option<Vec<f64>>,
    #[serde(default)]
    pub tree_blend_weight: Option<f64>,
    #[serde(default)]
    pub tree_info: Vec<lightgbm_json::Tree>,
    /// Purged time-prefix calibration from raw model return units to a
    /// conservative expected return consumed by portfolio construction.
    #[serde(default)]
    pub calibration: Option<PredictionCalibration>,
    /// Frozen source artefacts whose already-fitted predictions were averaged
    /// into this tree ensemble. Empty for an ordinary single fit.
    #[serde(default)]
    pub blend_components: Vec<ModelBlendComponent>,
    #[serde(skip)]
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelBlendComponent {
    pub sha256: String,
    pub feature_set_version: String,
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionCalibration {
    pub method: String,
    #[serde(with = "date_serde")]
    pub start: Date,
    #[serde(with = "date_serde")]
    pub end: Date,
    pub observations: usize,
    pub intercept: f64,
    pub slope: f64,
    pub residual_standard_deviation: f64,
}

impl Model {
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path)
            .map_err(|error| format!("cannot read model {}: {error}", path.display()))?;
        let mut model: Self = serde_json::from_slice(&bytes)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if !matches!(
            model.format_version.as_str(),
            FORMAT_VERSION | LEGACY_FORMAT_VERSION
        ) {
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
        let expected_features = expected_features(&model.feature_set_version)?;
        if features_stockholm::label_horizon(&model.label_version).is_none() {
            return Err(format!(
                "unsupported Stockholm label version {:?}",
                model.label_version
            ));
        }
        features_stockholm::validate_selection(&model.features)?;
        if model.features != expected_features {
            return Err("model feature order does not match its declared version".into());
        }
        let validate_linear = |model: &Model| -> Result<(), String> {
            let intercept = model
                .linear_intercept
                .ok_or_else(|| "linear model lacks its intercept".to_string())?;
            let weights = model
                .linear_weights
                .as_ref()
                .ok_or_else(|| "linear model lacks its weights".to_string())?;
            if weights.len() != model.features.len()
                || !intercept.is_finite()
                || !weights.iter().all(|weight| weight.is_finite())
            {
                return Err("linear model parameters are invalid".into());
            }
            match model.ridge_lambda {
                Some(value) if value.is_finite() && value > 0.0 => Ok(()),
                _ => Err("ridge model penalty is invalid".into()),
            }
        };
        match model.model_family.as_str() {
            "lightgbm" if model.tree_info.is_empty() => {
                return Err("LightGBM model contains no trees".into());
            }
            "lightgbm" => {}
            "ridge" => {
                if model.format_version != FORMAT_VERSION {
                    return Err("ridge model requires the current model format".into());
                }
                if !model.tree_info.is_empty() {
                    return Err("ridge model unexpectedly contains trees".into());
                }
                validate_linear(&model)?;
            }
            "hybrid" => {
                if model.format_version != FORMAT_VERSION || model.tree_info.is_empty() {
                    return Err("hybrid model requires current-format trees".into());
                }
                validate_linear(&model)?;
                match model.tree_blend_weight {
                    Some(weight) if weight.is_finite() && (0.0..=1.0).contains(&weight) => {}
                    _ => return Err("hybrid tree blend weight is invalid".into()),
                }
            }
            other => return Err(format!("unsupported Stockholm model family {other:?}")),
        }
        if !matches!(
            model.reward.as_str(),
            "absolute_return"
                | "return_per_risk"
                | "relative_return"
                | "relative_return_per_risk"
                | "relative_rank"
        ) {
            return Err(format!("unsupported Stockholm reward {:?}", model.reward));
        }
        if !matches!(model.objective.as_str(), "l2" | "l1" | "huber") {
            return Err(format!(
                "unsupported Stockholm objective {:?}",
                model.objective
            ));
        }
        if model.ensemble_seeds == 0 {
            return Err("model ensemble seed count must be positive".into());
        }
        if let Some([lower, upper]) = model.target_clip {
            if !lower.is_finite() || !upper.is_finite() || lower > upper {
                return Err("model target clip is invalid".into());
            }
        }
        match (model.reward.as_str(), model.reward_scale) {
            ("relative_rank", Some(scale)) if scale.is_finite() && scale > 0.0 => {}
            ("relative_rank", _) => {
                return Err("relative-rank model lacks a positive reward scale".into());
            }
            (_, Some(_)) => return Err("non-rank model unexpectedly has a reward scale".into()),
            (_, None) => {}
        }
        if let Some(calibration) = &model.calibration {
            if calibration.method != "purged_oos_affine_shrinkage_v1"
                || calibration.start > calibration.end
                || calibration.end > model.trained_through
                || calibration.observations == 0
                || !calibration.intercept.is_finite()
                || !calibration.slope.is_finite()
                || !(0.0..=1.0).contains(&calibration.slope)
                || !calibration.residual_standard_deviation.is_finite()
                || calibration.residual_standard_deviation < 0.0
            {
                return Err("model prediction calibration is invalid".into());
            }
        }
        if !model.blend_components.is_empty() {
            let total_weight = model
                .blend_components
                .iter()
                .map(|component| component.weight)
                .sum::<f64>();
            if model.blend_components.len() < 2
                || (total_weight - 1.0).abs() > 1e-12
                || model.blend_components.iter().any(|component| {
                    component.sha256.len() != 64
                        || !component
                            .sha256
                            .bytes()
                            .all(|byte| byte.is_ascii_hexdigit())
                        || component.feature_set_version.is_empty()
                        || !component.weight.is_finite()
                        || component.weight <= 0.0
                })
            {
                return Err("model blend provenance is invalid".into());
            }
        }
        model.model_id = format!("{:x}", Sha256::digest(&bytes))[..16].to_owned();
        Ok(model)
    }

    pub fn predict(&self, row: &TrainingRow) -> Result<f64, String> {
        if row.date <= self.trained_through {
            return Err(format!(
                "model trained through {} cannot score {}",
                self.trained_through, row.date
            ));
        }
        let values = self
            .features
            .iter()
            .map(|name| {
                row.features
                    .get(name)
                    .copied()
                    .ok_or_else(|| format!("matrix row lacks model feature {name:?}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let linear_score = || -> Result<f64, String> {
            let intercept = self
                .linear_intercept
                .ok_or_else(|| "linear model lacks its intercept".to_string())?;
            let weights = self
                .linear_weights
                .as_ref()
                .ok_or_else(|| "linear model lacks its weights".to_string())?;
            Ok(intercept
                + values
                    .iter()
                    .zip(weights)
                    .map(|(value, weight)| value * weight)
                    .sum::<f64>())
        };
        let score = match self.model_family.as_str() {
            "lightgbm" => lightgbm_json::predict(&self.tree_info, &values)?,
            "ridge" => linear_score()?,
            "hybrid" => {
                let tree_weight = self
                    .tree_blend_weight
                    .ok_or_else(|| "hybrid model lacks its blend weight".to_string())?;
                tree_weight * lightgbm_json::predict(&self.tree_info, &values)?
                    + (1.0 - tree_weight) * linear_score()?
            }
            other => return Err(format!("unsupported Stockholm model family {other:?}")),
        };
        let return_prediction = match self.reward.as_str() {
            "absolute_return" => Ok(score),
            "return_per_risk" | "relative_return_per_risk" => Ok(score * row.vol60),
            "relative_return" => Ok(score),
            "relative_rank" => Ok(score
                * self
                    .reward_scale
                    .expect("validated relative-rank reward scale")),
            _ => Err(format!("unsupported Stockholm reward {:?}", self.reward)),
        }?;
        Ok(self
            .calibration
            .as_ref()
            .map_or(return_prediction, |calibration| {
                calibration.intercept + calibration.slope * return_prediction
            }))
    }

    fn diagnostic_target(&self, row: &TrainingRow) -> Result<f64, String> {
        match self.reward.as_str() {
            "absolute_return" | "return_per_risk" => Ok(row.target),
            "relative_return" | "relative_return_per_risk" | "relative_rank" => {
                row.relative_target.ok_or_else(|| {
                    format!(
                        "matrix row {} on {} lacks its Rust relative target",
                        row.instrument_id, row.date
                    )
                })
            }
            _ => Err(format!("unsupported Stockholm reward {:?}", self.reward)),
        }
    }
}

/// Separate absolute-market-return model. It cannot be loaded as a stock
/// ranker, which prevents accidental interchange of the two prediction tasks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectionModel {
    pub format_version: String,
    pub model_version: String,
    pub feature_set_version: String,
    pub label_version: String,
    #[serde(with = "date_serde")]
    pub trained_through: Date,
    pub trained_at: String,
    pub n_rows: usize,
    pub n_dates: usize,
    pub features: Vec<String>,
    pub model_family: String,
    #[serde(default = "default_reward")]
    pub reward: String,
    pub objective: String,
    pub target_clip: Option<[f64; 2]>,
    /// Fixed conversion from predicted horizon return to the policy's bounded
    /// score: `clip(predicted_return / score_scale, -1, 1)`.
    pub score_scale: f64,
    pub tree_info: Vec<lightgbm_json::Tree>,
    #[serde(skip)]
    pub model_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DirectionPrediction {
    pub predicted_return: f64,
    pub score: f64,
}

impl DirectionModel {
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path)
            .map_err(|error| format!("cannot read direction model {}: {error}", path.display()))?;
        let mut model: Self = serde_json::from_slice(&bytes)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if model.format_version != DIRECTION_FORMAT_VERSION
            || model.model_version != DIRECTION_MODEL_VERSION
        {
            return Err("unsupported Stockholm direction model format or version".into());
        }
        let expected = match model.feature_set_version.as_str() {
            features_stockholm::DIRECTION_FEATURE_SET_VERSION => {
                features_stockholm::direction_model_feature_names()
            }
            features_stockholm::DIRECTION_GLOBAL_RISK_FEATURE_SET_VERSION => {
                features_stockholm::direction_global_risk_model_feature_names()
            }
            _ => return Err("unsupported direction model feature-set version".into()),
        };
        if model.features != expected {
            return Err("direction model feature contract differs from Rust".into());
        }
        if features_stockholm::direction_label_horizon(&model.label_version).is_none() {
            return Err(format!(
                "unsupported direction label version {:?}",
                model.label_version
            ));
        }
        if model.model_family != "lightgbm"
            || !matches!(model.objective.as_str(), "l2" | "l1" | "huber")
        {
            return Err("direction model must use a supported LightGBM objective".into());
        }
        if !matches!(model.reward.as_str(), "absolute_return" | "direction_sign") {
            return Err("direction model has an unsupported reward".into());
        }
        if model.tree_info.is_empty() {
            return Err("direction model contains no trees".into());
        }
        if !model.score_scale.is_finite() || model.score_scale <= 0.0 {
            return Err("direction model score scale must be finite and positive".into());
        }
        if let Some([lower, upper]) = model.target_clip {
            if !lower.is_finite() || !upper.is_finite() || lower > upper {
                return Err("direction model target clip is invalid".into());
            }
        }
        model.model_id = format!("{:x}", Sha256::digest(&bytes))[..16].to_owned();
        Ok(model)
    }

    pub fn predict(&self, row: &DirectionTrainingRow) -> Result<DirectionPrediction, String> {
        if row.date <= self.trained_through {
            return Err(format!(
                "direction model trained through {} cannot score {}",
                self.trained_through, row.date
            ));
        }
        let values = self
            .features
            .iter()
            .map(|name| {
                row.features
                    .get(name)
                    .copied()
                    .ok_or_else(|| format!("direction row lacks model feature {name:?}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let raw_prediction = lightgbm_json::predict(&self.tree_info, &values)?;
        let (predicted_return, score) = match self.reward.as_str() {
            "absolute_return" => (
                raw_prediction,
                (raw_prediction / self.score_scale).clamp(-1.0, 1.0),
            ),
            "direction_sign" => (
                raw_prediction.clamp(-1.0, 1.0) * self.score_scale,
                raw_prediction.clamp(-1.0, 1.0),
            ),
            _ => unreachable!("reward validated on load"),
        };
        if !predicted_return.is_finite() {
            return Err("direction model produced a non-finite forecast".into());
        }
        Ok(DirectionPrediction {
            predicted_return,
            score,
        })
    }
}

fn expected_features(version: &str) -> Result<Vec<String>, String> {
    match version {
        features_stockholm::BASELINE_FEATURE_SET_VERSION => {
            Ok(features_stockholm::baseline_model_feature_names())
        }
        features_stockholm::BASELINE_GLOBAL_RISK_FEATURE_SET_VERSION => {
            Ok(features_stockholm::baseline_global_risk_model_feature_names())
        }
        features_stockholm::FEATURE_SET_VERSION => Ok(features_stockholm::model_feature_names()),
        features_stockholm::RESIDUAL_FEATURE_SET_VERSION => {
            Ok(features_stockholm::residual_model_feature_names())
        }
        features_stockholm::PUBLIC_SHORT_FEATURE_SET_VERSION => {
            Ok(features_stockholm::public_short_model_feature_names())
        }
        features_stockholm::PDMR_FEATURE_SET_VERSION => {
            Ok(features_stockholm::pdmr_model_feature_names())
        }
        features_stockholm::REPORT_EVENT_FEATURE_SET_VERSION => {
            Ok(features_stockholm::pdmr_report_model_feature_names())
        }
        features_stockholm::FUNDAMENTAL_FEATURE_SET_VERSION => {
            Ok(features_stockholm::fundamental_model_feature_names())
        }
        features_stockholm::QUARTERLY_FUNDAMENTAL_FEATURE_SET_VERSION => {
            Ok(features_stockholm::quarterly_fundamental_model_feature_names())
        }
        features_stockholm::PDMR_MACRO_FEATURE_SET_VERSION => {
            Ok(features_stockholm::pdmr_macro_model_feature_names())
        }
        features_stockholm::PDMR_MICROSTRUCTURE_FEATURE_SET_VERSION => {
            Ok(features_stockholm::pdmr_microstructure_model_feature_names())
        }
        features_stockholm::PDMR_MICROSTRUCTURE_BORROW_FEATURE_SET_VERSION => {
            Ok(features_stockholm::pdmr_microstructure_borrow_model_feature_names())
        }
        features_stockholm::PDMR_MICROSTRUCTURE_BORROW_NEWS_FEATURE_SET_VERSION => {
            Ok(features_stockholm::pdmr_microstructure_borrow_news_model_feature_names())
        }
        features_stockholm::PDMR_MICROSTRUCTURE_BORROW_NEWS_REPORT_TEXT_FEATURE_SET_VERSION => Ok(
            features_stockholm::pdmr_microstructure_borrow_news_report_text_model_feature_names(),
        ),
        features_stockholm::PDMR_MICROSTRUCTURE_BORROW_NEWS_REPORT_ATTACHMENTS_FEATURE_SET_VERSION => Ok(
            features_stockholm::pdmr_microstructure_borrow_news_report_text_model_feature_names(),
        ),
        other => Err(format!("unsupported Stockholm feature version {other:?}")),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CostConfig {
    /// All-in round-trip fallback reference at the configured friction
    /// multiplier. Retained so pre-component research reports remain readable.
    pub round_trip_bps: f64,
    /// IBKR percentage commission for a round trip. Order minima are handled
    /// by the live minimum-trade-value gate, not by this NAV-free backtest.
    #[serde(default = "default_round_trip_commission_bps")]
    pub round_trip_commission_bps: f64,
    /// Conservative market-impact floor before stress multiplication.
    #[serde(default = "default_round_trip_impact_bps")]
    pub round_trip_impact_bps: f64,
    /// Used only when a row lacks a causal measured Nasdaq spread.
    #[serde(default = "default_fallback_spread_bps")]
    pub fallback_spread_bps: f64,
    /// Multiplies observed/fallback spread and impact, but never commission.
    #[serde(default = "default_market_friction_multiple")]
    pub market_friction_multiple: f64,
    /// Extra First North round-trip spread/impact in basis points.
    pub first_north_extra_bps: f64,
    /// Borrow fee charged over one holding period in basis points.
    pub short_borrow_bps: f64,
    /// Conservative penalty for unobserved historical availability.
    pub short_availability_bps: f64,
    pub safety_margin_bps: f64,
}

impl Default for CostConfig {
    fn default() -> Self {
        Self {
            // 10 bps IB tiered round-trip commission + 5 bps impact + a
            // 20 bps spread fallback. Real order minima can be worse.
            round_trip_bps: 35.0,
            round_trip_commission_bps: default_round_trip_commission_bps(),
            round_trip_impact_bps: default_round_trip_impact_bps(),
            fallback_spread_bps: default_fallback_spread_bps(),
            market_friction_multiple: default_market_friction_multiple(),
            first_north_extra_bps: 35.0,
            short_borrow_bps: 10.0,
            short_availability_bps: 25.0,
            safety_margin_bps: 10.0,
        }
    }
}

fn default_round_trip_commission_bps() -> f64 {
    10.0
}

fn default_round_trip_impact_bps() -> f64 {
    5.0
}

fn default_fallback_spread_bps() -> f64 {
    20.0
}

fn default_market_friction_multiple() -> f64 {
    1.0
}

#[derive(Debug, Clone, Copy)]
struct ExecutionCost {
    commission_bps: f64,
    impact_bps: f64,
    spread_bps: f64,
    observed_spread: bool,
}

impl ExecutionCost {
    fn total_bps(self) -> f64 {
        self.commission_bps + self.impact_bps + self.spread_bps
    }
}

fn execution_cost(row: &TrainingRow, costs: &CostConfig) -> Result<ExecutionCost, String> {
    let values = [
        costs.round_trip_bps,
        costs.round_trip_commission_bps,
        costs.round_trip_impact_bps,
        costs.fallback_spread_bps,
        costs.market_friction_multiple,
        costs.first_north_extra_bps,
        costs.short_borrow_bps,
        costs.short_availability_bps,
        costs.safety_margin_bps,
    ];
    if values
        .iter()
        .any(|value| !value.is_finite() || *value < 0.0)
        || costs.market_friction_multiple == 0.0
    {
        return Err("execution-cost components must be finite and non-negative, with a positive market-friction multiple".into());
    }
    let measured = row.median_closing_spread_bps_20;
    if measured.is_some_and(|value| !value.is_finite() || value < 0.0) {
        return Err(format!(
            "matrix row {} on {} has invalid median closing spread {:?}",
            row.instrument_id, row.date, measured
        ));
    }
    let first_north_impact = if matches!(
        row.bucket,
        UniverseBucket::FirstNorth | UniverseBucket::FirstNorthPremier
    ) {
        costs.first_north_extra_bps
    } else {
        0.0
    };
    Ok(ExecutionCost {
        commission_bps: costs.round_trip_commission_bps,
        impact_bps: costs.market_friction_multiple
            * (costs.round_trip_impact_bps + first_north_impact),
        spread_bps: costs.market_friction_multiple * measured.unwrap_or(costs.fallback_spread_bps),
        observed_spread: measured.is_some(),
    })
}

fn holding_borrow_cost(
    row: &TrainingRow,
    cadence_sessions: usize,
    costs: &CostConfig,
) -> Result<f64, String> {
    let Some(annual_rate) = row.borrow_fee_annualized else {
        return Ok(costs.short_borrow_bps / 10_000.0);
    };
    if !annual_rate.is_finite() || annual_rate < 0.0 {
        return Err(format!(
            "matrix row {} on {} has invalid annual borrow fee {annual_rate}",
            row.instrument_id, row.date
        ));
    }
    Ok(annual_rate * cadence_sessions as f64 / 252.0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Long,
    Short,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionResult {
    pub instrument_id: String,
    pub symbol: String,
    pub bucket: UniverseBucket,
    #[serde(default)]
    pub sector: String,
    pub direction: Direction,
    /// Expected absolute stock return used for the economic hurdle and side.
    pub predicted_return: f64,
    /// Stock-selection model output before adding the market forecast. For an
    /// absolute-return model this is absent because `predicted_return` already
    /// has the required meaning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_prediction: Option<f64>,
    /// Causal OMXSGI horizon-return forecast added to a relative prediction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_return_prediction: Option<f64>,
    pub net_edge: f64,
    pub realised_return: f64,
    pub weight: f64,
    pub cost: f64,
    pub pnl: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    #[serde(with = "date_serde")]
    pub date: Date,
    pub nav: f64,
    pub period_return: f64,
    pub gross: f64,
    pub net: f64,
    /// Full maximum-budget versus realised-allocation attribution. Optional so
    /// frozen reports produced before the allocator contract changed remain
    /// readable by `add-benchmark`.
    #[serde(default)]
    pub allocation: Option<portfolio_construction::AllocationDiagnostics>,
    #[serde(default)]
    pub direction: Option<DirectionAttribution>,
    /// Causal absolute OMXSGI forecast used to translate a relative stock
    /// forecast into an absolute expected return on this decision date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_return_prediction: Option<f64>,
    /// Standalone gross return of the direction layer's smoothed target net
    /// applied to OMXSGI over this holding period. Execution costs are absent
    /// and disclosed in `DirectionLayerMetrics`.
    #[serde(default)]
    pub direction_market_return: Option<f64>,
    pub turnover: f64,
    pub long_pnl: f64,
    pub short_pnl: f64,
    pub cost_drag: f64,
    #[serde(default)]
    pub benchmark_period_return: Option<f64>,
    #[serde(default)]
    pub benchmark_nav: Option<f64>,
    #[serde(default)]
    pub active_return: Option<f64>,
    pub positions: Vec<PositionResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectionAttribution {
    pub observation: features_stockholm::MarketTrendObservation,
    pub decision: portfolio_construction::DirectionDecision,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectionLayerMetrics {
    pub periods: usize,
    pub total_return: f64,
    pub annualised_return: f64,
    pub annualised_volatility: f64,
    pub sharpe: f64,
    pub max_drawdown: f64,
    pub mean_budget_gross: f64,
    pub mean_budget_net: f64,
    pub strong_up_periods: usize,
    pub up_periods: usize,
    pub neutral_periods: usize,
    pub down_periods: usize,
    pub strong_down_periods: usize,
    pub cost_status: String,
}

/// Research-only isolation of the cross-sectional stock-selection signal.
/// This deliberately holds equal long and short sleeves so market direction
/// cannot hide whether the ordering itself adds value. It is never used as a
/// portfolio budget or live target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionLayerMetrics {
    pub periods: usize,
    pub total_return: f64,
    pub annualised_return: f64,
    pub annualised_volatility: f64,
    pub sharpe: f64,
    pub max_drawdown: f64,
    pub positive_periods: usize,
    pub gross_return_before_costs: f64,
    pub cost_drag: f64,
    pub positions_per_side: usize,
    pub construction: String,
    pub steps: Vec<SelectionLayerStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionLayerStep {
    #[serde(with = "date_serde")]
    pub date: Date,
    pub gross_return_before_costs: f64,
    pub cost_drag: f64,
    pub net_return: f64,
    pub turnover: f64,
    pub long_positions: usize,
    pub short_positions: usize,
}

/// Costed acceptance control for the canonical adjusted-price 12-1 momentum
/// signal. This is deliberately separate from the fitted model contract: it
/// answers whether the research stack can beat a simple, predeclared rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixedMomentumBacktestResult {
    pub kind: String,
    pub signal: String,
    pub survivorship_status: String,
    #[serde(with = "date_serde")]
    pub start: Date,
    #[serde(with = "date_serde")]
    pub end: Date,
    pub cadence_sessions: usize,
    pub max_positions: usize,
    pub position_weight: f64,
    pub costs: CostConfig,
    pub directional: FixedMomentumPerformance,
    pub long_only: FixedMomentumPerformance,
    pub long_short_diagnostic: FixedMomentumPerformance,
    pub disclosures: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixedMomentumPerformance {
    pub construction: String,
    pub metrics: Metrics,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub benchmark: Option<BenchmarkComparison>,
    pub steps: Vec<FixedMomentumStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixedMomentumStep {
    #[serde(with = "date_serde")]
    pub date: Date,
    pub nav: f64,
    pub gross_return_before_costs: f64,
    pub cost_drag: f64,
    pub net_return: f64,
    pub turnover: f64,
    pub gross: f64,
    pub net: f64,
    pub long_pnl: f64,
    pub short_pnl: f64,
    pub long_positions: usize,
    pub short_positions: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub benchmark_period_return: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct FixedMomentumConfig {
    pub start: Date,
    pub end: Date,
    pub cadence_sessions: usize,
    pub max_positions: usize,
    /// Fixed maximum per-name weight. Missing signals remain cash; surviving
    /// names are never scaled up to fill a sleeve or gross-exposure quota.
    pub position_weight: f64,
    pub costs: CostConfig,
    pub benchmark: Option<BenchmarkHistory>,
    pub survivorship_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectionPerformance {
    pub periods: usize,
    pub total_return: f64,
    pub annualised_return: f64,
    pub annualised_volatility: f64,
    pub sharpe: f64,
    pub max_drawdown: f64,
    pub positive_periods: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectionStandaloneStep {
    #[serde(with = "date_serde")]
    pub date: Date,
    pub target_market_return: f64,
    pub predicted_market_return: Option<f64>,
    pub score: f64,
    pub strategy_return: f64,
    pub decision: portfolio_construction::DirectionDecision,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectionStrategyEvaluation {
    pub name: String,
    pub performance: DirectionPerformance,
    pub mean_budget_gross: f64,
    pub mean_budget_net: f64,
    pub directional_accuracy: f64,
    pub forecast_correlation: Option<f64>,
    pub mean_absolute_forecast_error: Option<f64>,
    pub strong_up_periods: usize,
    pub up_periods: usize,
    pub neutral_periods: usize,
    pub down_periods: usize,
    pub strong_down_periods: usize,
    pub steps: Vec<DirectionStandaloneStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectionBacktestResult {
    pub kind: String,
    pub model_id: String,
    pub feature_set_version: String,
    pub label_version: String,
    #[serde(default = "default_reward")]
    pub reward: String,
    #[serde(default = "default_direction_objective")]
    pub objective: String,
    #[serde(with = "date_serde")]
    pub trained_through: Date,
    #[serde(with = "date_serde")]
    pub start: Date,
    #[serde(with = "date_serde")]
    pub end: Date,
    pub cadence_sessions: usize,
    pub config: portfolio_construction::DirectionConfig,
    pub trained_model: DirectionStrategyEvaluation,
    pub fixed_trend_control: DirectionStrategyEvaluation,
    pub omxsgi_long_only: DirectionPerformance,
    pub disclosures: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectionFoldSummary {
    #[serde(with = "date_serde")]
    pub start: Date,
    #[serde(with = "date_serde")]
    pub end: Date,
    pub trained_model: DirectionPerformance,
    pub fixed_trend_control: DirectionPerformance,
    pub omxsgi_long_only: DirectionPerformance,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectionWalkForwardSummary {
    pub kind: String,
    pub folds: usize,
    pub model_ids: Vec<String>,
    pub feature_set_version: String,
    pub label_version: String,
    pub reward: String,
    pub objective: String,
    #[serde(with = "date_serde")]
    pub start: Date,
    #[serde(with = "date_serde")]
    pub end: Date,
    pub cadence_sessions: usize,
    pub config: portfolio_construction::DirectionConfig,
    pub trained_model: DirectionStrategyEvaluation,
    pub fixed_trend_control: DirectionStrategyEvaluation,
    pub omxsgi_long_only: DirectionPerformance,
    pub fold_results: Vec<DirectionFoldSummary>,
    pub disclosures: Vec<String>,
}

pub fn direction_backtest(
    model: &DirectionModel,
    rows: &[DirectionTrainingRow],
    start: Date,
    end: Date,
    config: portfolio_construction::DirectionConfig,
) -> Result<DirectionBacktestResult, String> {
    if end < start || start <= model.trained_through {
        return Err("direction test must be strictly after its training cutoff".into());
    }
    config.validate()?;
    if rows.is_empty() {
        return Err("direction matrix contains no rows".into());
    }
    if rows.windows(2).any(|pair| pair[0].date >= pair[1].date) {
        return Err("direction rows must be strictly increasing and unique".into());
    }
    let cadence = features_stockholm::direction_label_horizon(&model.label_version)
        .ok_or("direction model has an invalid label horizon")?;
    let trained_model = evaluate_direction_strategy(
        &format!("trained_{}_model", model.reward),
        rows,
        start,
        end,
        cadence,
        Some(model.trained_through),
        config,
        |row| {
            let prediction = model.predict(row)?;
            Ok((prediction.score, Some(prediction.predicted_return)))
        },
    )?;
    let fixed_trend_control = evaluate_direction_strategy(
        "fixed_five_vote_trend_control",
        rows,
        start,
        end,
        cadence,
        None,
        config,
        |row| Ok((fixed_direction_score(row)?, None)),
    )?;
    let omxsgi_returns = trained_model
        .steps
        .iter()
        .map(|step| step.target_market_return)
        .collect::<Vec<_>>();
    Ok(DirectionBacktestResult {
        kind: "stockholm_direction_walk_forward_fold".into(),
        model_id: model.model_id.clone(),
        feature_set_version: model.feature_set_version.clone(),
        label_version: model.label_version.clone(),
        reward: model.reward.clone(),
        objective: model.objective.clone(),
        trained_through: model.trained_through,
        start,
        end,
        cadence_sessions: cadence,
        config,
        trained_model,
        fixed_trend_control,
        omxsgi_long_only: direction_performance(&omxsgi_returns, cadence),
        disclosures: vec![
            "The direction layer applies target net exposure to official OMXSGI returns before execution, financing, tax, and short-borrow costs; it is a timing diagnostic, not a directly tradable result.".into(),
            "All model inputs are official Nasdaq index EOD values known on the decision date; the label begins at the following official SOD value.".into(),
            "The trained model is compared on identical non-overlapping sessions with the predeclared fixed five-vote trend control and long-only OMXSGI.".into(),
        ],
    })
}

pub fn summarize_direction_folds(
    reports: &[DirectionBacktestResult],
) -> Result<DirectionWalkForwardSummary, String> {
    if reports.is_empty() {
        return Err("no direction folds supplied".into());
    }
    let mut reports = reports.iter().collect::<Vec<_>>();
    reports.sort_by_key(|report| report.start);
    let first = reports[0];
    for report in &reports {
        if report.feature_set_version != first.feature_set_version
            || report.label_version != first.label_version
            || report.reward != first.reward
            || report.objective != first.objective
            || report.cadence_sessions != first.cadence_sessions
            || report.config != first.config
        {
            return Err("direction fold contracts differ".into());
        }
    }
    if reports.windows(2).any(|pair| pair[0].end >= pair[1].start) {
        return Err("direction folds overlap or are out of order".into());
    }
    let trained_steps = reports
        .iter()
        .flat_map(|report| report.trained_model.steps.iter().cloned())
        .collect::<Vec<_>>();
    let fixed_steps = reports
        .iter()
        .flat_map(|report| report.fixed_trend_control.steps.iter().cloned())
        .collect::<Vec<_>>();
    if trained_steps
        .iter()
        .map(|step| step.date)
        .ne(fixed_steps.iter().map(|step| step.date))
    {
        return Err("trained and fixed direction folds use different test dates".into());
    }
    let benchmark_returns = trained_steps
        .iter()
        .map(|step| step.target_market_return)
        .collect::<Vec<_>>();
    let fold_results = reports
        .iter()
        .map(|report| DirectionFoldSummary {
            start: report.start,
            end: report.end,
            trained_model: report.trained_model.performance.clone(),
            fixed_trend_control: report.fixed_trend_control.performance.clone(),
            omxsgi_long_only: report.omxsgi_long_only.clone(),
        })
        .collect();
    Ok(DirectionWalkForwardSummary {
        kind: "stockholm_direction_purged_expanding_walk_forward_summary".into(),
        folds: reports.len(),
        model_ids: reports
            .iter()
            .map(|report| report.model_id.clone())
            .collect(),
        feature_set_version: first.feature_set_version.clone(),
        label_version: first.label_version.clone(),
        reward: first.reward.clone(),
        objective: first.objective.clone(),
        start: first.start,
        end: reports.last().expect("non-empty reports").end,
        cadence_sessions: first.cadence_sessions,
        config: first.config,
        trained_model: direction_evaluation_from_steps(
            &format!("trained_{}_model", first.reward),
            trained_steps,
            first.cadence_sessions,
        ),
        fixed_trend_control: direction_evaluation_from_steps(
            "fixed_five_vote_trend_control",
            fixed_steps,
            first.cadence_sessions,
        ),
        omxsgi_long_only: direction_performance(&benchmark_returns, first.cadence_sessions),
        fold_results,
        disclosures: vec![
            "Metrics are recomputed in Rust from concatenated, non-overlapping strictly-forward fold steps; they are not an average of fold Sharpes.".into(),
            "The direction timing series remains gross of execution, financing, tax, and short-borrow costs.".into(),
        ],
    })
}

#[allow(clippy::too_many_arguments)]
fn evaluate_direction_strategy<F>(
    name: &str,
    rows: &[DirectionTrainingRow],
    start: Date,
    end: Date,
    cadence: usize,
    warm_after: Option<Date>,
    config: portfolio_construction::DirectionConfig,
    mut score: F,
) -> Result<DirectionStrategyEvaluation, String>
where
    F: FnMut(&DirectionTrainingRow) -> Result<(f64, Option<f64>), String>,
{
    let mut state = portfolio_construction::DirectionState::default();
    let mut test_offset = 0_usize;
    let mut steps = Vec::new();
    for row in rows.iter().filter(|row| row.date <= end) {
        if warm_after.is_some_and(|cutoff| row.date <= cutoff) {
            continue;
        }
        let (score_value, predicted_market_return) = score(row)?;
        let decision = state.update(score_value, row.annualised_volatility_20, &config)?;
        if row.date < start {
            continue;
        }
        if test_offset % cadence == 0 {
            let target_net = decision.budget.target_net.unwrap_or(0.0);
            steps.push(DirectionStandaloneStep {
                date: row.date,
                target_market_return: row.target,
                predicted_market_return,
                score: score_value,
                strategy_return: target_net * row.target,
                decision,
            });
        }
        test_offset += 1;
    }
    if steps.is_empty() {
        return Err(format!("direction strategy {name} has no test periods"));
    }
    Ok(direction_evaluation_from_steps(name, steps, cadence))
}

fn direction_evaluation_from_steps(
    name: &str,
    steps: Vec<DirectionStandaloneStep>,
    cadence: usize,
) -> DirectionStrategyEvaluation {
    let returns = steps
        .iter()
        .map(|step| step.strategy_return)
        .collect::<Vec<_>>();
    let forecast_pairs = steps
        .iter()
        .filter_map(|step| Some((step.predicted_market_return?, step.target_market_return)))
        .collect::<Vec<_>>();
    let regime_count = |regime| {
        steps
            .iter()
            .filter(|step| step.decision.regime == regime)
            .count()
    };
    DirectionStrategyEvaluation {
        name: name.into(),
        performance: direction_performance(&returns, cadence),
        mean_budget_gross: steps
            .iter()
            .map(|step| step.decision.budget.max_gross)
            .sum::<f64>()
            / steps.len() as f64,
        mean_budget_net: steps
            .iter()
            .map(|step| step.decision.budget.target_net.unwrap_or(0.0))
            .sum::<f64>()
            / steps.len() as f64,
        directional_accuracy: steps
            .iter()
            .filter(|step| step.score.signum() == step.target_market_return.signum())
            .count() as f64
            / steps.len() as f64,
        forecast_correlation: (!forecast_pairs.is_empty()).then(|| {
            let predicted = forecast_pairs
                .iter()
                .map(|(predicted, _)| *predicted)
                .collect::<Vec<_>>();
            let realised = forecast_pairs
                .iter()
                .map(|(_, realised)| *realised)
                .collect::<Vec<_>>();
            let denominator = population_std(&predicted) * population_std(&realised);
            if denominator > 0.0 {
                population_covariance(&predicted, &realised) / denominator
            } else {
                0.0
            }
        }),
        mean_absolute_forecast_error: (!forecast_pairs.is_empty()).then(|| {
            forecast_pairs
                .iter()
                .map(|(predicted, realised)| (predicted - realised).abs())
                .sum::<f64>()
                / forecast_pairs.len() as f64
        }),
        strong_up_periods: regime_count(portfolio_construction::MarketRegime::StrongUp),
        up_periods: regime_count(portfolio_construction::MarketRegime::Up),
        neutral_periods: regime_count(portfolio_construction::MarketRegime::Neutral),
        down_periods: regime_count(portfolio_construction::MarketRegime::Down),
        strong_down_periods: regime_count(portfolio_construction::MarketRegime::StrongDown),
        steps,
    }
}

fn fixed_direction_score(row: &DirectionTrainingRow) -> Result<f64, String> {
    let names = [
        "x_omxsgi_price_vs_ma50",
        "x_omxsgi_ma50_vs_ma200",
        "x_omxsgi_ret_63",
        "x_omxsgi_ret_126",
        "x_omxsgi_ret_252",
    ];
    let sum = names.iter().try_fold(0.0, |sum, name| {
        let value = row
            .features
            .get(*name)
            .copied()
            .ok_or_else(|| format!("direction row lacks fixed feature {name:?}"))?;
        Ok::<_, String>(sum + value.signum())
    })?;
    Ok(sum / names.len() as f64)
}

fn direction_performance(returns: &[f64], cadence: usize) -> DirectionPerformance {
    let metrics = return_metrics(returns, cadence);
    DirectionPerformance {
        periods: returns.len(),
        total_return: metrics.total_return,
        annualised_return: metrics.annualised_return,
        annualised_volatility: metrics.annualised_volatility,
        sharpe: metrics.sharpe,
        max_drawdown: metrics.max_drawdown,
        positive_periods: returns.iter().filter(|value| **value > 0.0).count(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metrics {
    pub periods: usize,
    pub total_return: f64,
    pub annualised_return: f64,
    pub annualised_volatility: f64,
    pub sharpe: f64,
    pub max_drawdown: f64,
    pub positive_periods: usize,
    pub long_pnl: f64,
    pub short_pnl: f64,
    pub cost_drag: f64,
    pub mean_gross: f64,
    pub mean_net: f64,
    pub long_positions: usize,
    pub short_positions: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PredictionBucket {
    pub bucket: usize,
    pub observations: usize,
    pub mean_prediction: f64,
    pub mean_realised_return: f64,
    pub directional_accuracy: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PredictionDiagnostics {
    pub observations: usize,
    pub decision_dates: usize,
    pub mean_rank_ic: f64,
    pub positive_rank_ic_dates: usize,
    pub directional_accuracy: f64,
    pub mean_absolute_error: f64,
    pub buckets: Vec<PredictionBucket>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkComparison {
    pub symbol: String,
    pub name: String,
    pub return_type: String,
    pub currency: String,
    pub source: String,
    pub periods: usize,
    pub total_return: f64,
    pub annualised_return: f64,
    pub annualised_volatility: f64,
    pub sharpe: f64,
    pub max_drawdown: f64,
    pub portfolio_minus_benchmark_total_return: f64,
    pub portfolio_minus_benchmark_annualised_return: f64,
    pub tracking_error: f64,
    pub information_ratio: f64,
    pub correlation: f64,
    pub beta: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BorrowDiagnostics {
    pub matrix_rows: usize,
    pub matrix_rows_with_fee: usize,
    pub short_position_periods: usize,
    pub short_position_periods_with_fee: usize,
    pub observed_holding_cost_drag: f64,
    pub fallback_holding_cost_drag: f64,
    pub availability_penalty_drag: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionCostDiagnostics {
    pub matrix_rows: usize,
    pub matrix_rows_with_observed_spread: usize,
    pub selected_position_periods: usize,
    pub selected_position_periods_with_observed_spread: usize,
    pub commission_drag: f64,
    pub impact_drag: f64,
    pub observed_spread_drag: f64,
    pub fallback_spread_drag: f64,
}

impl ExecutionCostDiagnostics {
    fn charge_one_way(&mut self, absolute_turnover: f64, cost: ExecutionCost) -> f64 {
        let commission = absolute_turnover * cost.commission_bps / 20_000.0;
        let impact = absolute_turnover * cost.impact_bps / 20_000.0;
        let spread = absolute_turnover * cost.spread_bps / 20_000.0;
        self.commission_drag += commission;
        self.impact_drag += impact;
        if cost.observed_spread {
            self.observed_spread_drag += spread;
        } else {
            self.fallback_spread_drag += spread;
        }
        commission + impact + spread
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestResult {
    pub kind: String,
    pub model_id: String,
    #[serde(default = "default_model_family")]
    pub model_family: String,
    #[serde(default)]
    pub feature_set_version: String,
    #[serde(default = "default_reward")]
    pub reward: String,
    #[serde(default = "default_objective")]
    pub objective: String,
    #[serde(default = "default_ensemble_seeds")]
    pub ensemble_seeds: usize,
    #[serde(default)]
    pub target_clip: Option<[f64; 2]>,
    #[serde(default)]
    pub reward_scale: Option<f64>,
    #[serde(default)]
    pub calibration: Option<PredictionCalibration>,
    #[serde(default)]
    pub prediction_composition: PredictionComposition,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blend_components: Vec<ModelBlendComponent>,
    pub survivorship_status: String,
    #[serde(with = "date_serde")]
    pub start: Date,
    #[serde(with = "date_serde")]
    pub end: Date,
    pub cadence_sessions: usize,
    /// Horizon encoded by the fitted model's label contract. This normally
    /// equals `cadence_sessions`; a shorter value is allowed only by an
    /// explicit multi-session forecast aggregation experiment.
    #[serde(default)]
    pub model_horizon_sessions: usize,
    /// Fixed conversion from the model-horizon return forecast to the holding
    /// horizon. One preserves the ordinary same-horizon contract.
    #[serde(default = "default_prediction_horizon_scale")]
    pub prediction_horizon_scale: f64,
    #[serde(default)]
    pub ranking: portfolio_construction::RankingMethod,
    #[serde(default)]
    pub sizing: portfolio_construction::SizingMethod,
    #[serde(default = "default_budget")]
    pub allocation_budget: portfolio_construction::Budget,
    #[serde(default = "default_reference_edge")]
    pub reference_edge: f64,
    #[serde(default = "default_reference_volatility")]
    pub reference_volatility: f64,
    #[serde(default)]
    pub min_position_weight: f64,
    #[serde(default)]
    pub direction_config: Option<portfolio_construction::DirectionConfig>,
    /// Separate absolute-market model used to compose relative stock forecasts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_forecast_model_id: Option<String>,
    /// Maximum absolute single-name weight. Retained under its original field
    /// name so frozen v1 reports remain readable.
    pub position_weight: f64,
    pub max_positions: usize,
    /// Incumbents may remain while still inside this overall rank. Equal to
    /// `max_positions` disables the hold buffer.
    #[serde(default)]
    pub retention_rank: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_sector_gross: Option<f64>,
    pub costs: CostConfig,
    pub metrics: Metrics,
    #[serde(default)]
    pub diagnostics: Option<PredictionDiagnostics>,
    #[serde(default)]
    pub direction_metrics: Option<DirectionLayerMetrics>,
    #[serde(default)]
    pub selection_layer: Option<SelectionLayerMetrics>,
    #[serde(default)]
    pub benchmark: Option<BenchmarkComparison>,
    #[serde(default)]
    pub borrow_diagnostics: BorrowDiagnostics,
    #[serde(default)]
    pub execution_cost_diagnostics: ExecutionCostDiagnostics,
    pub steps: Vec<Step>,
    pub disclosures: Vec<String>,
}

fn default_budget() -> portfolio_construction::Budget {
    portfolio_construction::Budget::gross_only(1.0).expect("one is a valid gross budget")
}

fn default_reference_edge() -> f64 {
    0.01
}

fn default_reference_volatility() -> f64 {
    0.02
}

fn default_prediction_horizon_scale() -> f64 {
    1.0
}

#[derive(Debug, Clone)]
pub struct BacktestConfig {
    pub start: Date,
    pub end: Date,
    pub cadence_sessions: usize,
    /// Horizon represented by `Model::predict`. Portfolio P&L always comes
    /// from matrix labels spanning `cadence_sessions`.
    pub model_horizon_sessions: usize,
    /// Predeclared conversion from model-horizon expected return to expected
    /// return over `cadence_sessions`. This affects economic hurdles and
    /// reported predictions, never realised labels.
    pub prediction_horizon_scale: f64,
    pub max_positions: usize,
    pub retention_rank: usize,
    pub max_sector_gross: Option<f64>,
    pub ranking: portfolio_construction::RankingMethod,
    pub sizing: portfolio_construction::SizingMethod,
    pub allocation_budget: portfolio_construction::Budget,
    /// Maximum absolute single-name weight.
    pub position_weight: f64,
    /// Positions below this absolute weight are uneconomic and left as cash.
    pub min_position_weight: f64,
    /// Fixed sizing anchors; unlike cross-sectional normalization these do not
    /// turn an exposure maximum into a quota.
    pub reference_edge: f64,
    pub reference_volatility: f64,
    pub direction_config: Option<portfolio_construction::DirectionConfig>,
    pub prediction_composition: PredictionComposition,
    /// Causal expected OMXSGI horizon return by decision date. Relative-return
    /// stock models and explicit residual composition require this.
    pub market_return_forecasts: Option<BTreeMap<Date, f64>>,
    pub market_forecast_model_id: Option<String>,
    pub costs: CostConfig,
    pub benchmark: Option<BenchmarkHistory>,
}

#[derive(Debug)]
struct Candidate<'a> {
    row: &'a TrainingRow,
    direction: Direction,
    absolute_prediction: f64,
    relative_prediction: Option<f64>,
    market_return_prediction: Option<f64>,
    edge: f64,
    execution_cost: ExecutionCost,
}

pub fn backtest(
    model: &Model,
    rows: &[TrainingRow],
    config: &BacktestConfig,
) -> Result<BacktestResult, String> {
    let start = config.start;
    let end = config.end;
    let cadence_sessions = config.cadence_sessions;
    let max_positions = config.max_positions;
    let retention_rank = config.retention_rank;
    let max_sector_gross = config.max_sector_gross;
    let ranking = config.ranking;
    let sizing = config.sizing;
    let fallback_budget = config.allocation_budget;
    let position_weight = config.position_weight;
    let costs = config.costs.clone();
    let relative_reward = matches!(
        model.reward.as_str(),
        "relative_return" | "relative_return_per_risk" | "relative_rank"
    );
    let residual_composition =
        config.prediction_composition == PredictionComposition::CrossSectionalResidualPlusMarket;
    let uses_market_forecast = relative_reward || residual_composition;
    if cadence_sessions == 0 || max_positions == 0 {
        return Err("cadence and max positions must be positive".into());
    }
    if config.model_horizon_sessions == 0
        || !config.prediction_horizon_scale.is_finite()
        || config.prediction_horizon_scale <= 0.0
    {
        return Err("model horizon and prediction horizon scale must be positive".into());
    }
    if retention_rank < max_positions {
        return Err("retention rank must be at least max positions".into());
    }
    if max_sector_gross.is_some_and(|value| !value.is_finite() || value <= 0.0) {
        return Err("maximum sector gross must be finite and positive".into());
    }
    if !position_weight.is_finite() || position_weight <= 0.0 || position_weight > 1.0 {
        return Err("maximum position weight must be finite and in (0, 1]".into());
    }
    fallback_budget.validate()?;
    if let Some(direction) = config.direction_config {
        direction.validate()?;
        if config.benchmark.is_none() {
            return Err("direction overlay requires benchmark history".into());
        }
    }
    if residual_composition && relative_reward {
        return Err(
            "cross-sectional residual composition requires an absolute-return stock model".into(),
        );
    }
    if uses_market_forecast {
        if config.direction_config.is_none() {
            return Err("market-forecast composition requires an explicit direction layer".into());
        }
        if config.market_return_forecasts.is_none()
            || config
                .market_forecast_model_id
                .as_deref()
                .is_none_or(str::is_empty)
        {
            return Err(
                "market-forecast composition requires a separate causal market-return forecast"
                    .into(),
            );
        }
    } else if config.market_return_forecasts.is_some() || config.market_forecast_model_id.is_some()
    {
        return Err("absolute-return selection must not add a market-return forecast".into());
    }
    if config
        .market_return_forecasts
        .as_ref()
        .is_some_and(|forecasts| forecasts.values().any(|value| !value.is_finite()))
    {
        return Err("market-return forecasts must be finite".into());
    }
    let mut by_date: BTreeMap<Date, Vec<&TrainingRow>> = BTreeMap::new();
    for row in rows {
        if row.date >= start && row.date <= end {
            by_date.entry(row.date).or_default().push(row);
        }
    }
    let dates = by_date.keys().copied().collect::<Vec<_>>();
    if dates.is_empty() {
        return Err("backtest window has no matrix rows".into());
    }
    let mut nav = 1.0;
    let mut benchmark_nav = config.benchmark.as_ref().map(|_| 1.0);
    let mut steps = Vec::new();
    let selected_dates = dates
        .into_iter()
        .step_by(cadence_sessions)
        .collect::<Vec<_>>();
    let direction_schedule = match (config.direction_config, config.benchmark.as_ref()) {
        (Some(direction), Some(history)) => {
            Some(build_direction_schedule(history, &direction, end)?)
        }
        (Some(_), None) => return Err("direction overlay requires benchmark history".into()),
        (None, _) => None,
    };
    let mut previous: BTreeMap<String, (f64, UniverseBucket, ExecutionCost)> = BTreeMap::new();
    let mut diagnostic_blocks = Vec::with_capacity(selected_dates.len());
    let mut borrow_diagnostics = BorrowDiagnostics {
        matrix_rows: by_date.values().map(Vec::len).sum(),
        matrix_rows_with_fee: by_date
            .values()
            .flatten()
            .filter(|row| row.borrow_fee_annualized.is_some())
            .count(),
        ..Default::default()
    };
    let mut execution_cost_diagnostics = ExecutionCostDiagnostics {
        matrix_rows: by_date.values().map(Vec::len).sum(),
        matrix_rows_with_observed_spread: by_date
            .values()
            .flatten()
            .filter(|row| row.median_closing_spread_bps_20.is_some())
            .count(),
        ..Default::default()
    };
    for (date_index, date) in selected_dates.iter().copied().enumerate() {
        let direction = direction_schedule
            .as_ref()
            .map(|schedule| {
                schedule.get(&date).cloned().ok_or_else(|| {
                    format!("direction overlay has no mature OMX observation on {date}")
                })
            })
            .transpose()?;
        let allocation_budget = direction
            .as_ref()
            .map(|attribution| attribution.decision.budget)
            .unwrap_or(fallback_budget);
        let market_return_prediction = if uses_market_forecast {
            Some(
                config
                    .market_return_forecasts
                    .as_ref()
                    .expect("relative forecast map validated")
                    .get(&date)
                    .copied()
                    .ok_or_else(|| format!("market-return forecast is missing on {date}"))?,
            )
        } else {
            None
        };
        let mut candidates = Vec::new();
        let mut diagnostic_block = Vec::with_capacity(by_date[&date].len());
        let raw_predictions = by_date[&date]
            .iter()
            .map(|row| Ok((*row, model.predict(row)? * config.prediction_horizon_scale)))
            .collect::<Result<Vec<_>, String>>()?;
        let prediction_center = if residual_composition {
            raw_predictions
                .iter()
                .map(|(_, prediction)| *prediction)
                .sum::<f64>()
                / raw_predictions.len() as f64
        } else {
            0.0
        };
        for (row, raw_prediction) in raw_predictions {
            let selection_prediction = raw_prediction - prediction_center;
            let diagnostic_target = if residual_composition {
                row.relative_target.ok_or_else(|| {
                    format!(
                        "matrix row {} on {} lacks its Rust relative target",
                        row.instrument_id, row.date
                    )
                })?
            } else {
                model.diagnostic_target(row)?
            };
            diagnostic_block.push((selection_prediction, diagnostic_target));
            // Relative cross-sectional scores only describe performance versus
            // OMXSGI. Their sign cannot determine an absolute BUY/SELL side.
            // Compose the separately trained market forecast first, then apply
            // execution and borrow hurdles to the resulting stock forecast.
            let absolute_prediction =
                selection_prediction + market_return_prediction.unwrap_or(0.0);
            let execution_cost = execution_cost(row, &costs)?;
            let long_cost = execution_cost.total_bps() / 10_000.0;
            let borrow_cost = holding_borrow_cost(row, cadence_sessions, &costs)?;
            let short_cost = (execution_cost.total_bps() + costs.short_availability_bps) / 10_000.0
                + borrow_cost;
            let margin = costs.safety_margin_bps / 10_000.0;
            let long_edge = absolute_prediction - long_cost - margin;
            let short_edge = -absolute_prediction - short_cost - margin;
            let (direction, edge) = if long_edge >= short_edge {
                (Direction::Long, long_edge)
            } else {
                (Direction::Short, short_edge)
            };
            if edge > 0.0 {
                candidates.push(Candidate {
                    row,
                    direction,
                    absolute_prediction,
                    relative_prediction: uses_market_forecast.then_some(selection_prediction),
                    market_return_prediction,
                    edge,
                    execution_cost,
                });
            }
        }
        diagnostic_blocks.push(diagnostic_block);
        let holding_borrow_costs = candidates
            .iter()
            .map(|candidate| {
                Ok((
                    candidate.row.instrument_id.clone(),
                    (
                        holding_borrow_cost(candidate.row, cadence_sessions, &costs)?,
                        candidate.row.borrow_fee_annualized.is_some(),
                    ),
                ))
            })
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        let construction_candidates = candidates
            .iter()
            .map(|candidate| portfolio_construction::Candidate {
                id: candidate.row.instrument_id.clone(),
                direction: match candidate.direction {
                    Direction::Long => portfolio_construction::Direction::Long,
                    Direction::Short => portfolio_construction::Direction::Short,
                },
                edge: candidate.edge,
                volatility: candidate.row.vol60,
            })
            .collect::<Vec<_>>();
        let incumbents = previous
            .iter()
            .filter_map(|(id, (weight, _, _))| {
                ((*weight).abs() > f64::EPSILON).then_some((
                    id.clone(),
                    if *weight > 0.0 {
                        portfolio_construction::Direction::Long
                    } else {
                        portfolio_construction::Direction::Short
                    },
                ))
            })
            .collect::<BTreeMap<_, _>>();
        let ranked = portfolio_construction::buffered_ranked_ids(
            &construction_candidates,
            ranking,
            &incumbents,
            max_positions,
            retention_rank,
        )?;
        let mut by_id = candidates
            .into_iter()
            .map(|candidate| (candidate.row.instrument_id.clone(), candidate))
            .collect::<BTreeMap<_, _>>();
        let candidates = ranked
            .into_iter()
            .map(|id| by_id.remove(&id).expect("ranked candidate exists"))
            .collect::<Vec<_>>();
        let selected = candidates
            .iter()
            .map(|candidate| portfolio_construction::Candidate {
                id: candidate.row.instrument_id.clone(),
                direction: match candidate.direction {
                    Direction::Long => portfolio_construction::Direction::Long,
                    Direction::Short => portfolio_construction::Direction::Short,
                },
                edge: candidate.edge,
                volatility: candidate.row.vol60,
            })
            .collect::<Vec<_>>();
        let proposals = portfolio_construction::propose(
            &selected,
            &portfolio_construction::SizingConfig {
                method: sizing,
                unit_abs_weight: position_weight,
                min_abs_weight: config.min_position_weight,
                max_abs_weight: position_weight,
                reference_edge: config.reference_edge,
                reference_volatility: config.reference_volatility,
            },
        )?;
        let groups = candidates
            .iter()
            .map(|candidate| {
                (
                    candidate.row.instrument_id.clone(),
                    candidate.row.sector.clone(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let allocation = if let Some(maximum) = max_sector_gross {
            portfolio_construction::allocate_with_group_cap(
                &proposals,
                allocation_budget,
                &groups,
                maximum,
            )?
        } else {
            portfolio_construction::allocate(&proposals, allocation_budget)?
        };
        let weights = &allocation.weights;
        let target: BTreeMap<_, _> = candidates
            .iter()
            .filter_map(|candidate| {
                weights.get(&candidate.row.instrument_id).map(|weight| {
                    (
                        candidate.row.instrument_id.clone(),
                        (
                            *weight,
                            candidate.row.bucket.clone(),
                            candidate.execution_cost,
                        ),
                    )
                })
            })
            .collect();
        let mut positions = Vec::new();
        let mut long_pnl = 0.0;
        let mut short_pnl = 0.0;
        let mut cost_drag = 0.0;
        let mut net = 0.0;
        let mut allocated_costs: BTreeMap<String, f64> = BTreeMap::new();
        let mut identities = previous
            .keys()
            .chain(target.keys())
            .cloned()
            .collect::<Vec<_>>();
        identities.sort();
        identities.dedup();
        let mut turnover = 0.0;
        for instrument_id in identities {
            let previous_weight = previous
                .get(&instrument_id)
                .map(|(weight, _, _)| *weight)
                .unwrap_or(0.0);
            let target_weight = target
                .get(&instrument_id)
                .map(|(weight, _, _)| *weight)
                .unwrap_or(0.0);
            let execution_cost = target
                .get(&instrument_id)
                .or_else(|| previous.get(&instrument_id))
                .map(|(_, _, cost)| *cost)
                .expect("identity comes from target or previous");
            let delta = (target_weight - previous_weight).abs();
            turnover += delta;
            let mut amount = execution_cost_diagnostics.charge_one_way(delta, execution_cost);
            let previous_short = (-previous_weight).max(0.0);
            let target_short = (-target_weight).max(0.0);
            let availability_cost =
                (target_short - previous_short).max(0.0) * costs.short_availability_bps / 10_000.0;
            amount += availability_cost;
            borrow_diagnostics.availability_penalty_drag += availability_cost;
            let (borrow_rate, observed) = holding_borrow_costs
                .get(&instrument_id)
                .copied()
                .unwrap_or((costs.short_borrow_bps / 10_000.0, false));
            let holding_cost = target_short * borrow_rate;
            amount += holding_cost;
            if target_short > 0.0 {
                borrow_diagnostics.short_position_periods += 1;
                if observed {
                    borrow_diagnostics.short_position_periods_with_fee += 1;
                    borrow_diagnostics.observed_holding_cost_drag += holding_cost;
                } else {
                    borrow_diagnostics.fallback_holding_cost_drag += holding_cost;
                }
            }
            // The last measured holding period includes the terminal close so
            // the backtest does not leave a free liquidation outside the NAV.
            if date_index + 1 == selected_dates.len() {
                amount +=
                    execution_cost_diagnostics.charge_one_way(target_weight.abs(), execution_cost);
                turnover += target_weight.abs();
            }
            let allocation_weight = if target_weight != 0.0 {
                target_weight
            } else {
                previous_weight
            };
            if allocation_weight >= 0.0 {
                long_pnl -= amount;
            } else {
                short_pnl -= amount;
            }
            cost_drag += amount;
            if target_weight != 0.0 {
                allocated_costs.insert(instrument_id, amount);
            }
        }
        execution_cost_diagnostics.selected_position_periods += target.len();
        execution_cost_diagnostics.selected_position_periods_with_observed_spread += target
            .values()
            .filter(|(_, _, cost)| cost.observed_spread)
            .count();
        for candidate in candidates {
            let Some(&signed_weight) = weights.get(&candidate.row.instrument_id) else {
                continue;
            };
            let sign = signed_weight.signum();
            let absolute_weight = signed_weight.abs();
            let cost_amount = allocated_costs
                .get(&candidate.row.instrument_id)
                .copied()
                .unwrap_or(0.0);
            let pnl = absolute_weight * sign * candidate.row.target - cost_amount;
            if sign > 0.0 {
                long_pnl += absolute_weight * candidate.row.target;
            } else {
                short_pnl -= absolute_weight * candidate.row.target;
            }
            net += signed_weight;
            positions.push(PositionResult {
                instrument_id: candidate.row.instrument_id.clone(),
                symbol: candidate.row.symbol.clone(),
                bucket: candidate.row.bucket.clone(),
                sector: candidate.row.sector.clone(),
                direction: candidate.direction,
                predicted_return: candidate.absolute_prediction,
                relative_prediction: candidate.relative_prediction,
                market_return_prediction: candidate.market_return_prediction,
                net_edge: candidate.edge,
                realised_return: candidate.row.target,
                weight: signed_weight,
                cost: cost_amount,
                pnl,
            });
        }
        let gross = positions.iter().map(|position| position.weight.abs()).sum();
        let period_return = long_pnl + short_pnl;
        nav *= 1.0 + period_return;
        let benchmark_period_return = config
            .benchmark
            .as_ref()
            .map(|history| benchmark_return(history, date, cadence_sessions))
            .transpose()?;
        let direction_market_return = direction.as_ref().map(|attribution| {
            attribution.decision.budget.target_net.unwrap_or(0.0)
                * benchmark_period_return.unwrap_or(0.0)
        });
        if let (Some(current_nav), Some(value)) = (benchmark_nav.as_mut(), benchmark_period_return)
        {
            *current_nav *= 1.0 + value;
        }
        steps.push(Step {
            date,
            nav,
            period_return,
            gross,
            net,
            allocation: Some(allocation.diagnostics),
            direction,
            market_return_prediction,
            direction_market_return,
            turnover,
            long_pnl,
            short_pnl,
            cost_drag,
            benchmark_period_return,
            benchmark_nav,
            active_return: benchmark_period_return.map(|value| period_return - value),
            positions,
        });
        previous = target;
    }
    let metrics = metrics(&steps, cadence_sessions);
    let diagnostics = prediction_diagnostics(&diagnostic_blocks);
    let direction_metrics = direction_layer_metrics(&steps, cadence_sessions);
    let selection_layer = selection_layer_metrics(
        model,
        &by_date,
        &selected_dates,
        cadence_sessions,
        max_positions,
        &costs,
    )?;
    let benchmark = config
        .benchmark
        .as_ref()
        .map(|history| benchmark_comparison(&steps, &metrics, cadence_sessions, history))
        .transpose()?;
    let mut disclosures = vec![
        "SURVIVORSHIP_CONTAMINATED: current 2026 constituents are projected backward; this result cannot authorize capital.".into(),
        "Historical borrow quantity is unavailable; shorts carry a configured availability penalty but feasibility is not proven.".into(),
        "Yahoo adjusted daily history is research input; production collection remains IB plus a licensed point-in-time universe.".into(),
        "OMXSGI comparison uses official Nasdaq gross-index SOD levels from the next exchange session through the configured holding horizon; it is a broad market reference, not a forced exposure target.".into(),
    ];
    if config.direction_config.is_some() {
        disclosures.push("The standalone direction-layer series applies smoothed target net exposure to OMXSGI before execution costs; it diagnoses timing only and cannot be treated as a tradable net result.".into());
    }
    if (config.prediction_horizon_scale - 1.0).abs() > f64::EPSILON {
        disclosures.push(format!(
            "The model predicts a {}-session return and its forecast is multiplied by {:.6} before economic hurdles for this {}-session holding replay. Realised P&L always uses the matrix's unscaled {}-session executable label.",
            config.model_horizon_sessions,
            config.prediction_horizon_scale,
            cadence_sessions,
            cadence_sessions,
        ));
    }
    if relative_reward {
        disclosures.push("Relative stock forecasts are translated to absolute expected returns by adding the separately trained causal OMXSGI horizon-return forecast before BUY/SELL classification and economic hurdles; the relative score is used unchanged for prediction diagnostics.".into());
    }
    if residual_composition {
        disclosures.push("The absolute stock model is used for cross-sectional ordering only: its equal-weight same-date prediction mean is removed and a separately trained causal OMXSGI forecast is added before BUY/SELL classification and economic hurdles. Centering changes no stock rank and uses no future value.".into());
    }
    if selection_layer.is_some() {
        disclosures.push("The selection-layer diagnostic forces equal long and short top/bottom sleeves solely to isolate ranking skill, with measured execution and borrow costs. It is not a portfolio target and does not impose market neutrality on live construction.".into());
    }
    if retention_rank > max_positions {
        disclosures.push(format!(
            "Turnover hysteresis retains an incumbent only while it still clears the economic hurdle, keeps the same predicted side, and ranks within the top {retention_rank}; vacancies are filled from the strongest candidates."
        ));
    }
    if let Some(maximum) = max_sector_gross {
        disclosures.push(format!(
            "Each sector has a maximum {:.1}% gross exposure. Binding sectors scale down proportionally and the excess stays in cash.",
            maximum * 100.0
        ));
    }
    if rows.iter().any(|row| row.borrow_fee_annualized.is_some()) {
        disclosures.push("Short holding costs use completed-session IB historical FEE_RATE when present, annualized over the holding sessions; missing records use the configured fixed fallback. FEE_RATE is cost, not locate availability.".into());
    }
    if rows
        .iter()
        .any(|row| row.median_closing_spread_bps_20.is_some())
    {
        disclosures.push("Execution spread uses each security's causal 20-session median Nasdaq closing bid/ask spread when at least ten observations exist; it is a next-open execution proxy, not a fill record. Missing rows use the configured fallback.".into());
    }
    disclosures.push("IBKR Sweden percentage commission is modeled as 0.05% per side. The tiered SEK 10 and fixed SEK 49 per-order minima require NAV-aware minimum-trade-value enforcement and are not represented by a constant-bps replay.".into());
    Ok(BacktestResult {
        kind: "stockholm_walk_forward_fold".into(),
        model_id: model.model_id.clone(),
        model_family: model.model_family.clone(),
        feature_set_version: model.feature_set_version.clone(),
        reward: model.reward.clone(),
        objective: model.objective.clone(),
        ensemble_seeds: model.ensemble_seeds,
        target_clip: model.target_clip,
        reward_scale: model.reward_scale,
        calibration: model.calibration.clone(),
        prediction_composition: config.prediction_composition,
        blend_components: model.blend_components.clone(),
        survivorship_status: model.survivorship_status.clone(),
        start,
        end,
        cadence_sessions,
        model_horizon_sessions: config.model_horizon_sessions,
        prediction_horizon_scale: config.prediction_horizon_scale,
        ranking,
        sizing,
        allocation_budget: fallback_budget,
        reference_edge: config.reference_edge,
        reference_volatility: config.reference_volatility,
        min_position_weight: config.min_position_weight,
        direction_config: config.direction_config,
        market_forecast_model_id: config.market_forecast_model_id.clone(),
        position_weight,
        max_positions,
        retention_rank,
        max_sector_gross,
        costs,
        metrics,
        diagnostics: Some(diagnostics),
        direction_metrics,
        selection_layer,
        benchmark,
        borrow_diagnostics,
        execution_cost_diagnostics,
        steps,
        disclosures,
    })
}

fn build_direction_schedule(
    history: &BenchmarkHistory,
    config: &portfolio_construction::DirectionConfig,
    through: Date,
) -> Result<BTreeMap<Date, DirectionAttribution>, String> {
    let bars = history
        .bars
        .iter()
        .filter(|bar| bar.date <= through)
        .map(|bar| features_stockholm::MarketBar {
            date: bar.date,
            close: bar.end_value,
        })
        .collect::<Vec<_>>();
    let observations = features_stockholm::market_trend(&bars)?;
    let mut state = portfolio_construction::DirectionState::default();
    observations
        .into_iter()
        .map(|observation| {
            let decision = state.update(
                observation.score,
                observation.annualised_volatility_20,
                config,
            )?;
            Ok((
                observation.date,
                DirectionAttribution {
                    observation,
                    decision,
                },
            ))
        })
        .collect()
}

fn direction_layer_metrics(steps: &[Step], cadence: usize) -> Option<DirectionLayerMetrics> {
    let attributed = steps
        .iter()
        .filter_map(|step| Some((step.direction.as_ref()?, step.direction_market_return?)))
        .collect::<Vec<_>>();
    if attributed.is_empty() {
        return None;
    }
    let returns = attributed
        .iter()
        .map(|(_, period_return)| *period_return)
        .collect::<Vec<_>>();
    let metrics = return_metrics(&returns, cadence);
    let periods = attributed.len();
    let regime_count = |regime| {
        attributed
            .iter()
            .filter(|(attribution, _)| attribution.decision.regime == regime)
            .count()
    };
    Some(DirectionLayerMetrics {
        periods,
        total_return: metrics.total_return,
        annualised_return: metrics.annualised_return,
        annualised_volatility: metrics.annualised_volatility,
        sharpe: metrics.sharpe,
        max_drawdown: metrics.max_drawdown,
        mean_budget_gross: attributed
            .iter()
            .map(|(attribution, _)| attribution.decision.budget.max_gross)
            .sum::<f64>()
            / periods as f64,
        mean_budget_net: attributed
            .iter()
            .map(|(attribution, _)| attribution.decision.budget.target_net.unwrap_or(0.0))
            .sum::<f64>()
            / periods as f64,
        strong_up_periods: regime_count(portfolio_construction::MarketRegime::StrongUp),
        up_periods: regime_count(portfolio_construction::MarketRegime::Up),
        neutral_periods: regime_count(portfolio_construction::MarketRegime::Neutral),
        down_periods: regime_count(portfolio_construction::MarketRegime::Down),
        strong_down_periods: regime_count(portfolio_construction::MarketRegime::StrongDown),
        cost_status: "gross_before_execution_costs".into(),
    })
}

fn selection_layer_metrics(
    model: &Model,
    by_date: &BTreeMap<Date, Vec<&TrainingRow>>,
    selected_dates: &[Date],
    cadence: usize,
    max_positions: usize,
    costs: &CostConfig,
) -> Result<Option<SelectionLayerMetrics>, String> {
    let positions_per_side = max_positions / 2;
    if positions_per_side == 0 {
        return Ok(None);
    }
    let unit_weight = 0.5 / positions_per_side as f64;
    let mut previous: BTreeMap<String, (f64, ExecutionCost)> = BTreeMap::new();
    let mut steps = Vec::with_capacity(selected_dates.len());
    for (date_index, date) in selected_dates.iter().copied().enumerate() {
        let rows = &by_date[&date];
        if rows.len() < positions_per_side * 2 {
            continue;
        }
        let mut ranked = rows
            .iter()
            .map(|row| Ok((*row, model.predict(row)?)))
            .collect::<Result<Vec<_>, String>>()?;
        ranked.sort_by(|(left_row, left_score), (right_row, right_score)| {
            left_score
                .total_cmp(right_score)
                .then_with(|| left_row.instrument_id.cmp(&right_row.instrument_id))
        });
        let mut target = BTreeMap::new();
        for (row, _) in ranked.iter().take(positions_per_side) {
            target.insert(
                row.instrument_id.clone(),
                (-unit_weight, execution_cost(row, costs)?, *row),
            );
        }
        for (row, _) in ranked.iter().rev().take(positions_per_side) {
            target.insert(
                row.instrument_id.clone(),
                (unit_weight, execution_cost(row, costs)?, *row),
            );
        }

        let gross_return_before_costs = target
            .values()
            .map(|(weight, _, row)| weight * row.target)
            .sum::<f64>();
        let mut cost_drag = 0.0;
        let mut turnover = 0.0;
        let mut identities = previous
            .keys()
            .chain(target.keys())
            .cloned()
            .collect::<Vec<_>>();
        identities.sort();
        identities.dedup();
        for instrument_id in identities {
            let previous_weight = previous
                .get(&instrument_id)
                .map(|(weight, _)| *weight)
                .unwrap_or(0.0);
            let target_weight = target
                .get(&instrument_id)
                .map(|(weight, _, _)| *weight)
                .unwrap_or(0.0);
            let cost = target
                .get(&instrument_id)
                .map(|(_, cost, _)| *cost)
                .or_else(|| previous.get(&instrument_id).map(|(_, cost)| *cost))
                .expect("selection identity comes from target or previous");
            let delta = (target_weight - previous_weight).abs();
            turnover += delta;
            cost_drag += delta * cost.total_bps() / 20_000.0;
            let previous_short = (-previous_weight).max(0.0);
            let target_short = (-target_weight).max(0.0);
            cost_drag +=
                (target_short - previous_short).max(0.0) * costs.short_availability_bps / 10_000.0;
            if let Some((_, _, row)) = target.get(&instrument_id) {
                cost_drag += target_short * holding_borrow_cost(row, cadence, costs)?;
            }
            // Include terminal liquidation so this diagnostic has the same
            // complete-holding accounting as the executable portfolio.
            if date_index + 1 == selected_dates.len() {
                turnover += target_weight.abs();
                cost_drag += target_weight.abs() * cost.total_bps() / 20_000.0;
            }
        }
        steps.push(SelectionLayerStep {
            date,
            gross_return_before_costs,
            cost_drag,
            net_return: gross_return_before_costs - cost_drag,
            turnover,
            long_positions: positions_per_side,
            short_positions: positions_per_side,
        });
        previous = target
            .into_iter()
            .map(|(id, (weight, cost, _))| (id, (weight, cost)))
            .collect();
    }
    if steps.is_empty() {
        return Ok(None);
    }
    let net_returns = steps.iter().map(|step| step.net_return).collect::<Vec<_>>();
    let gross_returns = steps
        .iter()
        .map(|step| step.gross_return_before_costs)
        .collect::<Vec<_>>();
    let net = return_metrics(&net_returns, cadence);
    let gross = return_metrics(&gross_returns, cadence);
    Ok(Some(SelectionLayerMetrics {
        periods: steps.len(),
        total_return: net.total_return,
        annualised_return: net.annualised_return,
        annualised_volatility: net.annualised_volatility,
        sharpe: net.sharpe,
        max_drawdown: net.max_drawdown,
        positive_periods: net_returns.iter().filter(|value| **value > 0.0).count(),
        gross_return_before_costs: gross.total_return,
        cost_drag: steps.iter().map(|step| step.cost_drag).sum(),
        positions_per_side,
        construction: "research_only_equal_weight_top_bottom_dollar_neutral".into(),
        steps,
    }))
}

#[derive(Debug, Clone, Copy)]
enum FixedMomentumMode {
    Directional,
    LongOnly,
    LongShort,
}

/// Replay three fixed 12-1 momentum controls on the same executable labels and
/// cost machinery as the learned portfolio. The directional arm takes the
/// strongest absolute signals regardless of side, so its net exposure is an
/// outcome of available stock trends rather than a forced hedge ratio.
pub fn fixed_momentum_backtest(
    rows: &[TrainingRow],
    config: &FixedMomentumConfig,
) -> Result<FixedMomentumBacktestResult, String> {
    if config.end < config.start {
        return Err("fixed momentum end precedes start".into());
    }
    if config.cadence_sessions == 0 || config.max_positions == 0 {
        return Err("fixed momentum cadence and max positions must be positive".into());
    }
    if !config.position_weight.is_finite()
        || config.position_weight <= 0.0
        || config.position_weight > 1.0
        || config.position_weight * config.max_positions as f64 > 1.0 + 1e-12
    {
        return Err(
            "fixed momentum position weight must be positive and total maximum gross cannot exceed one"
                .into(),
        );
    }
    let mut by_date: BTreeMap<Date, Vec<&TrainingRow>> = BTreeMap::new();
    for row in rows {
        if row.date < config.start || row.date > config.end {
            continue;
        }
        match row.momentum_12_1 {
            Some(value) if value.is_finite() => by_date.entry(row.date).or_default().push(row),
            Some(_) => {
                return Err(format!(
                    "matrix row {} on {} has non-finite 12-1 momentum",
                    row.instrument_id, row.date
                ));
            }
            None => {
                return Err(format!(
                    "matrix row {} on {} lacks Rust-owned 12-1 momentum; rebuild the matrix",
                    row.instrument_id, row.date
                ));
            }
        }
    }
    if by_date.is_empty() {
        return Err("fixed momentum window has no matrix rows".into());
    }
    let selected_dates = by_date
        .keys()
        .copied()
        .step_by(config.cadence_sessions)
        .collect::<Vec<_>>();
    let directional = fixed_momentum_performance(
        &by_date,
        &selected_dates,
        FixedMomentumMode::Directional,
        config,
    )?;
    let long_only = fixed_momentum_performance(
        &by_date,
        &selected_dates,
        FixedMomentumMode::LongOnly,
        config,
    )?;
    let long_short_diagnostic = fixed_momentum_performance(
        &by_date,
        &selected_dates,
        FixedMomentumMode::LongShort,
        config,
    )?;
    Ok(FixedMomentumBacktestResult {
        kind: "stockholm_fixed_momentum_acceptance_control".into(),
        signal: "adjusted_close_t_minus_21 / adjusted_close_t_minus_252 - 1".into(),
        survivorship_status: config.survivorship_status.clone(),
        start: config.start,
        end: config.end,
        cadence_sessions: config.cadence_sessions,
        max_positions: config.max_positions,
        position_weight: config.position_weight,
        costs: config.costs.clone(),
        directional,
        long_only,
        long_short_diagnostic,
        disclosures: vec![
            "The fixed rule and its 12-1 lookback were declared before this replay; it is an acceptance control, not a newly fitted model.".into(),
            "The directional arm chooses the strongest absolute positive or negative stock trends without a long/short quota. Each name keeps the fixed maximum weight and unused capacity stays cash.".into(),
            "The long/short arm is a dollar-neutral ranking diagnostic only; it is not the intended live allocation policy.".into(),
            "Historical borrow quantity is unavailable. Short holding fees use causal IB FEE_RATE where present and the configured fallback otherwise; new shorts also pay an availability penalty.".into(),
            "The current-security history still omits many inactive and delisted shares, so survivorship contamination prevents capital authorization.".into(),
        ],
    })
}

fn fixed_momentum_performance(
    by_date: &BTreeMap<Date, Vec<&TrainingRow>>,
    selected_dates: &[Date],
    mode: FixedMomentumMode,
    config: &FixedMomentumConfig,
) -> Result<FixedMomentumPerformance, String> {
    let mut previous: BTreeMap<String, (f64, ExecutionCost)> = BTreeMap::new();
    let mut nav = 1.0;
    let mut steps = Vec::with_capacity(selected_dates.len());
    for (date_index, date) in selected_dates.iter().copied().enumerate() {
        let mut ranked = by_date[&date]
            .iter()
            .map(|row| (*row, row.momentum_12_1.expect("validated above")))
            .filter(|(_, momentum)| *momentum != 0.0)
            .collect::<Vec<_>>();
        let selected = match mode {
            FixedMomentumMode::Directional => {
                ranked.sort_by(|(left_row, left), (right_row, right)| {
                    right
                        .abs()
                        .total_cmp(&left.abs())
                        .then_with(|| left_row.instrument_id.cmp(&right_row.instrument_id))
                });
                ranked
                    .into_iter()
                    .take(config.max_positions)
                    .collect::<Vec<_>>()
            }
            FixedMomentumMode::LongOnly => {
                ranked.retain(|(_, momentum)| *momentum > 0.0);
                ranked.sort_by(|(left_row, left), (right_row, right)| {
                    right
                        .total_cmp(left)
                        .then_with(|| left_row.instrument_id.cmp(&right_row.instrument_id))
                });
                ranked
                    .into_iter()
                    .take(config.max_positions)
                    .collect::<Vec<_>>()
            }
            FixedMomentumMode::LongShort => {
                let per_side = config.max_positions / 2;
                let mut longs = ranked
                    .iter()
                    .copied()
                    .filter(|(_, momentum)| *momentum > 0.0)
                    .collect::<Vec<_>>();
                let mut shorts = ranked
                    .into_iter()
                    .filter(|(_, momentum)| *momentum < 0.0)
                    .collect::<Vec<_>>();
                longs.sort_by(|(left_row, left), (right_row, right)| {
                    right
                        .total_cmp(left)
                        .then_with(|| left_row.instrument_id.cmp(&right_row.instrument_id))
                });
                shorts.sort_by(|(left_row, left), (right_row, right)| {
                    left.total_cmp(right)
                        .then_with(|| left_row.instrument_id.cmp(&right_row.instrument_id))
                });
                longs
                    .into_iter()
                    .take(per_side)
                    .chain(shorts.into_iter().take(per_side))
                    .collect::<Vec<_>>()
            }
        };
        let mut target = BTreeMap::new();
        for (row, momentum) in selected {
            let weight = momentum.signum() * config.position_weight;
            target.insert(
                row.instrument_id.clone(),
                (weight, execution_cost(row, &config.costs)?, row),
            );
        }

        let long_pnl = target
            .values()
            .filter(|(weight, _, _)| *weight > 0.0)
            .map(|(weight, _, row)| weight * row.target)
            .sum::<f64>();
        let short_pnl = target
            .values()
            .filter(|(weight, _, _)| *weight < 0.0)
            .map(|(weight, _, row)| weight * row.target)
            .sum::<f64>();
        let gross_return_before_costs = long_pnl + short_pnl;
        let gross = target.values().map(|(weight, _, _)| weight.abs()).sum();
        let net = target.values().map(|(weight, _, _)| weight).sum();
        let long_positions = target
            .values()
            .filter(|(weight, _, _)| *weight > 0.0)
            .count();
        let short_positions = target.len() - long_positions;
        let mut turnover = 0.0;
        let mut cost_drag = 0.0;
        let mut identities = previous
            .keys()
            .chain(target.keys())
            .cloned()
            .collect::<Vec<_>>();
        identities.sort();
        identities.dedup();
        for instrument_id in identities {
            let previous_weight = previous
                .get(&instrument_id)
                .map(|(weight, _)| *weight)
                .unwrap_or(0.0);
            let target_weight = target
                .get(&instrument_id)
                .map(|(weight, _, _)| *weight)
                .unwrap_or(0.0);
            let execution = target
                .get(&instrument_id)
                .map(|(_, execution, _)| *execution)
                .or_else(|| {
                    previous
                        .get(&instrument_id)
                        .map(|(_, execution)| *execution)
                })
                .expect("momentum identity comes from target or previous");
            let delta = (target_weight - previous_weight).abs();
            turnover += delta;
            cost_drag += delta * execution.total_bps() / 20_000.0;
            let previous_short = (-previous_weight).max(0.0);
            let target_short = (-target_weight).max(0.0);
            cost_drag += (target_short - previous_short).max(0.0)
                * config.costs.short_availability_bps
                / 10_000.0;
            if let Some((_, _, row)) = target.get(&instrument_id) {
                cost_drag += target_short
                    * holding_borrow_cost(row, config.cadence_sessions, &config.costs)?;
            }
            if date_index + 1 == selected_dates.len() {
                turnover += target_weight.abs();
                cost_drag += target_weight.abs() * execution.total_bps() / 20_000.0;
            }
        }
        let net_return = gross_return_before_costs - cost_drag;
        nav *= 1.0 + net_return;
        let benchmark_period_return = config
            .benchmark
            .as_ref()
            .map(|history| benchmark_return(history, date, config.cadence_sessions))
            .transpose()?;
        steps.push(FixedMomentumStep {
            date,
            nav,
            gross_return_before_costs,
            cost_drag,
            net_return,
            turnover,
            gross,
            net,
            long_pnl,
            short_pnl,
            long_positions,
            short_positions,
            benchmark_period_return,
        });
        previous = target
            .into_iter()
            .map(|(instrument_id, (weight, execution, _))| (instrument_id, (weight, execution)))
            .collect();
    }
    let metrics = fixed_momentum_metrics(&steps, config.cadence_sessions);
    let benchmark = config
        .benchmark
        .as_ref()
        .map(|history| {
            let portfolio_returns = steps.iter().map(|step| step.net_return).collect::<Vec<_>>();
            let benchmark_returns = steps
                .iter()
                .map(|step| {
                    step.benchmark_period_return
                        .ok_or("benchmark return missing from momentum step")
                })
                .collect::<Result<Vec<_>, _>>()?;
            benchmark_comparison_from_returns(
                &portfolio_returns,
                &metrics,
                &benchmark_returns,
                config.cadence_sessions,
                history,
            )
        })
        .transpose()?;
    let construction = match mode {
        FixedMomentumMode::Directional => {
            "fixed_weight_strongest_absolute_12_1_sign_determines_side"
        }
        FixedMomentumMode::LongOnly => "fixed_weight_top_positive_12_1_long_only",
        FixedMomentumMode::LongShort => "diagnostic_fixed_weight_top_positive_bottom_negative_12_1",
    };
    Ok(FixedMomentumPerformance {
        construction: construction.into(),
        metrics,
        benchmark,
        steps,
    })
}

fn fixed_momentum_metrics(steps: &[FixedMomentumStep], cadence: usize) -> Metrics {
    let returns = steps.iter().map(|step| step.net_return).collect::<Vec<_>>();
    let values = return_metrics(&returns, cadence);
    Metrics {
        periods: steps.len(),
        total_return: values.total_return,
        annualised_return: values.annualised_return,
        annualised_volatility: values.annualised_volatility,
        sharpe: values.sharpe,
        max_drawdown: values.max_drawdown,
        positive_periods: returns.iter().filter(|value| **value > 0.0).count(),
        long_pnl: steps.iter().map(|step| step.long_pnl).sum(),
        short_pnl: steps.iter().map(|step| step.short_pnl).sum(),
        cost_drag: steps.iter().map(|step| step.cost_drag).sum(),
        mean_gross: mean(&steps.iter().map(|step| step.gross).collect::<Vec<_>>()),
        mean_net: mean(&steps.iter().map(|step| step.net).collect::<Vec<_>>()),
        long_positions: steps.iter().map(|step| step.long_positions).sum(),
        short_positions: steps.iter().map(|step| step.short_positions).sum(),
    }
}

fn prediction_diagnostics(blocks: &[Vec<(f64, f64)>]) -> PredictionDiagnostics {
    const N_BUCKETS: usize = 10;
    let mut correlations = Vec::new();
    let mut buckets = vec![Vec::<(f64, f64)>::new(); N_BUCKETS];
    let mut observations = 0_usize;
    let mut direction_correct = 0_usize;
    let mut absolute_error = 0.0;
    for block in blocks {
        if block.is_empty() {
            continue;
        }
        let predictions = block
            .iter()
            .map(|(prediction, _)| *prediction)
            .collect::<Vec<_>>();
        let realised = block.iter().map(|(_, target)| *target).collect::<Vec<_>>();
        if let Some(value) = spearman(&predictions, &realised) {
            correlations.push(value);
        }
        let mut order = (0..block.len()).collect::<Vec<_>>();
        order.sort_by(|left, right| {
            predictions[*left]
                .total_cmp(&predictions[*right])
                .then_with(|| left.cmp(right))
        });
        for (rank, index) in order.into_iter().enumerate() {
            let bucket = (rank * N_BUCKETS / block.len()).min(N_BUCKETS - 1);
            buckets[bucket].push(block[index]);
        }
        for (prediction, target) in block {
            observations += 1;
            absolute_error += (prediction - target).abs();
            if prediction.signum() == target.signum() {
                direction_correct += 1;
            }
        }
    }
    let bucket_rows = buckets
        .into_iter()
        .enumerate()
        .map(|(bucket, values)| {
            let n = values.len();
            let denominator = n.max(1) as f64;
            PredictionBucket {
                bucket: bucket + 1,
                observations: n,
                mean_prediction: values.iter().map(|(value, _)| value).sum::<f64>() / denominator,
                mean_realised_return: values.iter().map(|(_, value)| value).sum::<f64>()
                    / denominator,
                directional_accuracy: values
                    .iter()
                    .filter(|(prediction, target)| prediction.signum() == target.signum())
                    .count() as f64
                    / denominator,
            }
        })
        .collect();
    PredictionDiagnostics {
        observations,
        decision_dates: blocks.iter().filter(|block| !block.is_empty()).count(),
        mean_rank_ic: if correlations.is_empty() {
            0.0
        } else {
            correlations.iter().sum::<f64>() / correlations.len() as f64
        },
        positive_rank_ic_dates: correlations.iter().filter(|value| **value > 0.0).count(),
        directional_accuracy: direction_correct as f64 / observations.max(1) as f64,
        mean_absolute_error: absolute_error / observations.max(1) as f64,
        buckets: bucket_rows,
    }
}

fn spearman(left: &[f64], right: &[f64]) -> Option<f64> {
    features_common::spearman(left, right)
}

/// Attribute an already-frozen Rust replay to a benchmark without rescoring
/// the matrix or changing positions, costs, or portfolio returns.
pub fn add_benchmark(
    mut result: BacktestResult,
    history: &BenchmarkHistory,
) -> Result<BacktestResult, String> {
    let mut nav = 1.0;
    for step in &mut result.steps {
        let value = benchmark_return(history, step.date, result.cadence_sessions)?;
        nav *= 1.0 + value;
        step.benchmark_period_return = Some(value);
        step.benchmark_nav = Some(nav);
        step.active_return = Some(step.period_return - value);
    }
    result.benchmark = Some(benchmark_comparison(
        &result.steps,
        &result.metrics,
        result.cadence_sessions,
        history,
    )?);
    let disclosure = "OMXSGI comparison uses official Nasdaq gross-index SOD levels from the next exchange session through the configured holding horizon; it is a broad market reference, not a forced exposure target.";
    if !result.disclosures.iter().any(|value| value == disclosure) {
        result.disclosures.push(disclosure.into());
    }
    Ok(result)
}

fn benchmark_return(
    history: &BenchmarkHistory,
    decision_date: Date,
    horizon: usize,
) -> Result<f64, String> {
    if history
        .bars
        .windows(2)
        .any(|pair| pair[0].date >= pair[1].date)
    {
        return Err(format!(
            "benchmark {} sessions are not strictly increasing",
            history.symbol
        ));
    }
    let entry_index = history
        .bars
        .partition_point(|bar| bar.date <= decision_date);
    let exit_index = entry_index
        .checked_add(horizon)
        .ok_or("benchmark horizon overflow")?;
    let entry = history.bars.get(entry_index).ok_or_else(|| {
        format!(
            "benchmark {} has no session after {decision_date}",
            history.symbol
        )
    })?;
    let exit = history.bars.get(exit_index).ok_or_else(|| {
        format!(
            "benchmark {} lacks {horizon} sessions after {}",
            history.symbol, entry.date
        )
    })?;
    let value = exit.start_value / entry.start_value - 1.0;
    if !value.is_finite() {
        return Err(format!(
            "benchmark {} has invalid values on {} or {}",
            history.symbol, entry.date, exit.date
        ));
    }
    Ok(value)
}

fn benchmark_comparison(
    steps: &[Step],
    portfolio: &Metrics,
    cadence: usize,
    history: &BenchmarkHistory,
) -> Result<BenchmarkComparison, String> {
    let benchmark_returns = steps
        .iter()
        .map(|step| {
            step.benchmark_period_return
                .ok_or("benchmark return missing from aligned step")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let portfolio_returns = steps
        .iter()
        .map(|step| step.period_return)
        .collect::<Vec<_>>();
    benchmark_comparison_from_returns(
        &portfolio_returns,
        portfolio,
        &benchmark_returns,
        cadence,
        history,
    )
}

fn benchmark_comparison_from_returns(
    portfolio_returns: &[f64],
    portfolio: &Metrics,
    benchmark_returns: &[f64],
    cadence: usize,
    history: &BenchmarkHistory,
) -> Result<BenchmarkComparison, String> {
    if portfolio_returns.len() != benchmark_returns.len() {
        return Err("portfolio and benchmark return counts differ".into());
    }
    let benchmark_stats = return_metrics(benchmark_returns, cadence);
    let active = portfolio_returns
        .iter()
        .zip(benchmark_returns)
        .map(|(portfolio, benchmark)| portfolio - benchmark)
        .collect::<Vec<_>>();
    let periods_per_year = 252.0 / cadence as f64;
    let active_mean = mean(&active);
    let tracking_error = population_std(&active) * periods_per_year.sqrt();
    let covariance = population_covariance(&portfolio_returns, &benchmark_returns);
    let portfolio_std = population_std(&portfolio_returns);
    let benchmark_std = population_std(&benchmark_returns);
    let benchmark_variance = benchmark_std.powi(2);
    Ok(BenchmarkComparison {
        symbol: history.symbol.clone(),
        name: history.name.clone(),
        return_type: history.return_type.clone(),
        currency: history.currency.clone(),
        source: history.source.clone(),
        periods: benchmark_returns.len(),
        total_return: benchmark_stats.total_return,
        annualised_return: benchmark_stats.annualised_return,
        annualised_volatility: benchmark_stats.annualised_volatility,
        sharpe: benchmark_stats.sharpe,
        max_drawdown: benchmark_stats.max_drawdown,
        portfolio_minus_benchmark_total_return: portfolio.total_return
            - benchmark_stats.total_return,
        portfolio_minus_benchmark_annualised_return: portfolio.annualised_return
            - benchmark_stats.annualised_return,
        tracking_error,
        information_ratio: if tracking_error > 0.0 {
            active_mean * periods_per_year / tracking_error
        } else {
            0.0
        },
        correlation: if portfolio_std > 0.0 && benchmark_std > 0.0 {
            covariance / (portfolio_std * benchmark_std)
        } else {
            0.0
        },
        beta: if benchmark_variance > 0.0 {
            covariance / benchmark_variance
        } else {
            0.0
        },
    })
}

#[derive(Debug)]
struct ReturnMetrics {
    total_return: f64,
    annualised_return: f64,
    annualised_volatility: f64,
    sharpe: f64,
    max_drawdown: f64,
}

fn return_metrics(returns: &[f64], cadence: usize) -> ReturnMetrics {
    let periods_per_year = 252.0 / cadence as f64;
    let total_nav = returns.iter().fold(1.0, |nav, value| nav * (1.0 + value));
    let total_return = total_nav - 1.0;
    let annualised_volatility = population_std(returns) * periods_per_year.sqrt();
    let mut nav = 1.0_f64;
    let mut peak = 1.0_f64;
    let mut max_drawdown = 0.0_f64;
    for value in returns {
        nav *= 1.0 + value;
        peak = peak.max(nav);
        max_drawdown = max_drawdown.min(nav / peak - 1.0);
    }
    ReturnMetrics {
        total_return,
        annualised_return: if returns.is_empty() {
            0.0
        } else {
            (1.0 + total_return)
                .max(0.0)
                .powf(periods_per_year / returns.len() as f64)
                - 1.0
        },
        annualised_volatility,
        sharpe: if annualised_volatility > 0.0 {
            mean(returns) * periods_per_year / annualised_volatility
        } else {
            0.0
        },
        max_drawdown,
    }
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

fn population_std(values: &[f64]) -> f64 {
    let average = mean(values);
    if values.is_empty() {
        0.0
    } else {
        (values
            .iter()
            .map(|value| (value - average).powi(2))
            .sum::<f64>()
            / values.len() as f64)
            .sqrt()
    }
}

fn population_covariance(left: &[f64], right: &[f64]) -> f64 {
    if left.is_empty() || left.len() != right.len() {
        return 0.0;
    }
    let left_mean = mean(left);
    let right_mean = mean(right);
    left.iter()
        .zip(right)
        .map(|(left, right)| (left - left_mean) * (right - right_mean))
        .sum::<f64>()
        / left.len() as f64
}

fn metrics(steps: &[Step], cadence: usize) -> Metrics {
    if steps.is_empty() {
        return Metrics {
            periods: 0,
            total_return: 0.0,
            annualised_return: 0.0,
            annualised_volatility: 0.0,
            sharpe: 0.0,
            max_drawdown: 0.0,
            positive_periods: 0,
            long_pnl: 0.0,
            short_pnl: 0.0,
            cost_drag: 0.0,
            mean_gross: 0.0,
            mean_net: 0.0,
            long_positions: 0,
            short_positions: 0,
        };
    }
    let returns = steps
        .iter()
        .map(|step| step.period_return)
        .collect::<Vec<_>>();
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance = returns
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / returns.len().max(2) as f64;
    let periods_per_year = 252.0 / cadence as f64;
    let vol = variance.sqrt() * periods_per_year.sqrt();
    let total_return = steps.last().map(|step| step.nav - 1.0).unwrap_or(0.0);
    let annualised_return = (1.0 + total_return)
        .max(0.0)
        .powf(periods_per_year / steps.len() as f64)
        - 1.0;
    let mut peak = 1.0_f64;
    let mut max_drawdown = 0.0_f64;
    for step in steps {
        peak = peak.max(step.nav);
        max_drawdown = max_drawdown.min(step.nav / peak - 1.0);
    }
    Metrics {
        periods: steps.len(),
        total_return,
        annualised_return,
        annualised_volatility: vol,
        sharpe: if vol > 0.0 {
            mean * periods_per_year / vol
        } else {
            0.0
        },
        max_drawdown,
        positive_periods: steps.iter().filter(|step| step.period_return > 0.0).count(),
        long_pnl: steps.iter().map(|step| step.long_pnl).sum(),
        short_pnl: steps.iter().map(|step| step.short_pnl).sum(),
        cost_drag: steps.iter().map(|step| step.cost_drag).sum(),
        mean_gross: steps.iter().map(|step| step.gross).sum::<f64>() / steps.len() as f64,
        mean_net: steps.iter().map(|step| step.net).sum::<f64>() / steps.len() as f64,
        long_positions: steps
            .iter()
            .flat_map(|step| &step.positions)
            .filter(|position| matches!(position.direction, Direction::Long))
            .count(),
        short_positions: steps
            .iter()
            .flat_map(|step| &step.positions)
            .filter(|position| matches!(position.direction, Direction::Short))
            .count(),
    }
}

mod date_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use time::Date;

    pub fn serialize<S: Serializer>(date: &Date, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&date.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Date, D::Error> {
        let value = String::deserialize(deserializer)?;
        let format = time::macros::format_description!("[year]-[month]-[day]");
        Date::parse(&value, format).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(value: &str) -> Date {
        Date::parse(
            value,
            time::macros::format_description!("[year]-[month]-[day]"),
        )
        .unwrap()
    }

    fn constant_model(value: f64) -> Model {
        Model {
            format_version: FORMAT_VERSION.into(),
            model_version: MODEL_VERSION.into(),
            feature_set_version: features_stockholm::FEATURE_SET_VERSION.into(),
            label_version: features_stockholm::LABEL_VERSION.into(),
            trained_through: day("2023-12-31"),
            trained_at: "fixture".into(),
            n_rows: 1,
            n_dates: 1,
            features: vec!["x_ret_1".into()],
            survivorship_status: "SURVIVORSHIP_CONTAMINATED".into(),
            model_family: default_model_family(),
            reward: default_reward(),
            objective: default_objective(),
            ensemble_seeds: 1,
            target_clip: None,
            reward_scale: None,
            ridge_lambda: None,
            linear_intercept: None,
            linear_weights: None,
            tree_blend_weight: None,
            tree_info: vec![lightgbm_json::Tree {
                tree_index: 0,
                num_leaves: 1,
                num_cat: Some(0),
                shrinkage: Some(1.0),
                tree_structure: lightgbm_json::Node {
                    split_feature: None,
                    threshold: None,
                    decision_type: None,
                    default_left: None,
                    missing_type: None,
                    left_child: None,
                    right_child: None,
                    leaf_value: Some(value),
                    diagnostics: Default::default(),
                },
            }],
            calibration: None,
            blend_components: Vec::new(),
            model_id: "fixture".into(),
        }
    }

    #[test]
    fn per_risk_model_score_is_converted_back_to_return() {
        let mut model = constant_model(0.5);
        model.reward = "return_per_risk".into();
        assert!((model.predict(&row(day("2024-01-02"))).unwrap() - 0.01).abs() < 1e-12);
    }

    #[test]
    fn relative_per_risk_model_score_is_converted_back_to_return() {
        let mut model = constant_model(0.5);
        model.reward = "relative_return_per_risk".into();
        assert!((model.predict(&row(day("2024-01-02"))).unwrap() - 0.01).abs() < 1e-12);
    }

    #[test]
    fn rank_model_score_is_calibrated_back_to_return() {
        let mut model = constant_model(0.5);
        model.reward = "relative_rank".into();
        model.reward_scale = Some(0.04);
        assert!((model.predict(&row(day("2024-01-02"))).unwrap() - 0.02).abs() < 1e-12);
    }

    #[test]
    fn purged_calibration_shrinks_the_prediction_before_construction() {
        let mut model = constant_model(0.10);
        model.calibration = Some(PredictionCalibration {
            method: "purged_oos_affine_shrinkage_v1".into(),
            start: day("2023-01-01"),
            end: day("2023-12-31"),
            observations: 1_000,
            intercept: 0.01,
            slope: 0.20,
            residual_standard_deviation: 0.05,
        });
        assert!((model.predict(&row(day("2024-01-02"))).unwrap() - 0.03).abs() < 1e-12);
    }

    #[test]
    fn observed_borrow_fee_replaces_the_fixed_holding_cost() {
        let mut observed = row(day("2024-01-02"));
        observed.borrow_fee_annualized = Some(0.252);
        let costs = CostConfig::default();
        assert!((holding_borrow_cost(&observed, 10, &costs).unwrap() - 0.01).abs() < 1e-12);
        observed.borrow_fee_annualized = None;
        assert!(
            (holding_borrow_cost(&observed, 10, &costs).unwrap()
                - costs.short_borrow_bps / 10_000.0)
                .abs()
                < 1e-12
        );
    }

    #[test]
    fn runtime_accepts_the_complete_public_short_feature_contract() {
        assert_eq!(
            expected_features(features_stockholm::PUBLIC_SHORT_FEATURE_SET_VERSION).unwrap(),
            features_stockholm::public_short_model_feature_names()
        );
    }

    #[test]
    fn runtime_accepts_the_complete_pdmr_feature_contract() {
        assert_eq!(
            expected_features(features_stockholm::PDMR_FEATURE_SET_VERSION).unwrap(),
            features_stockholm::pdmr_model_feature_names()
        );
    }

    #[test]
    fn runtime_accepts_the_complete_fundamental_feature_contract() {
        assert_eq!(
            expected_features(features_stockholm::FUNDAMENTAL_FEATURE_SET_VERSION).unwrap(),
            features_stockholm::fundamental_model_feature_names()
        );
    }

    #[test]
    fn runtime_accepts_the_complete_quarterly_fundamental_feature_contract() {
        assert_eq!(
            expected_features(features_stockholm::QUARTERLY_FUNDAMENTAL_FEATURE_SET_VERSION)
                .unwrap(),
            features_stockholm::quarterly_fundamental_model_feature_names()
        );
    }

    #[test]
    fn runtime_accepts_the_complete_pdmr_macro_feature_contract() {
        assert_eq!(
            expected_features(features_stockholm::PDMR_MACRO_FEATURE_SET_VERSION).unwrap(),
            features_stockholm::pdmr_macro_model_feature_names()
        );
    }

    #[test]
    fn runtime_accepts_the_complete_pdmr_microstructure_feature_contract() {
        assert_eq!(
            expected_features(features_stockholm::PDMR_MICROSTRUCTURE_FEATURE_SET_VERSION).unwrap(),
            features_stockholm::pdmr_microstructure_model_feature_names()
        );
    }

    #[test]
    fn runtime_accepts_the_complete_borrow_feature_contract() {
        assert_eq!(
            expected_features(features_stockholm::PDMR_MICROSTRUCTURE_BORROW_FEATURE_SET_VERSION)
                .unwrap(),
            features_stockholm::pdmr_microstructure_borrow_model_feature_names()
        );
    }

    #[test]
    fn runtime_accepts_the_complete_borrow_news_feature_contract() {
        assert_eq!(
            expected_features(
                features_stockholm::PDMR_MICROSTRUCTURE_BORROW_NEWS_FEATURE_SET_VERSION,
            )
            .unwrap(),
            features_stockholm::pdmr_microstructure_borrow_news_model_feature_names()
        );
    }

    #[test]
    fn runtime_accepts_the_complete_report_text_feature_contract() {
        assert_eq!(
            expected_features(
                features_stockholm::PDMR_MICROSTRUCTURE_BORROW_NEWS_REPORT_TEXT_FEATURE_SET_VERSION,
            )
            .unwrap(),
            features_stockholm::pdmr_microstructure_borrow_news_report_text_model_feature_names()
        );
    }

    #[test]
    fn runtime_accepts_the_global_risk_feature_contract() {
        assert_eq!(
            expected_features(features_stockholm::BASELINE_GLOBAL_RISK_FEATURE_SET_VERSION)
                .unwrap(),
            features_stockholm::baseline_global_risk_model_feature_names()
        );
    }

    #[test]
    fn runtime_accepts_the_complete_report_attachment_feature_contract() {
        assert_eq!(
            expected_features(
                features_stockholm::PDMR_MICROSTRUCTURE_BORROW_NEWS_REPORT_ATTACHMENTS_FEATURE_SET_VERSION,
            )
            .unwrap(),
            features_stockholm::pdmr_microstructure_borrow_news_report_text_model_feature_names()
        );
    }

    #[test]
    fn ridge_model_is_evaluated_in_manifest_feature_order() {
        let mut model = constant_model(99.0);
        model.model_family = "ridge".into();
        model.ridge_lambda = Some(25.0);
        model.linear_intercept = Some(0.1);
        model.linear_weights = Some(vec![0.5]);
        model.tree_info.clear();
        assert!((model.predict(&row(day("2024-01-02"))).unwrap() - 0.35).abs() < 1e-12);
    }

    #[test]
    fn hybrid_model_blends_predictions_in_the_same_return_units() {
        let mut model = constant_model(0.15);
        model.model_family = "hybrid".into();
        model.ridge_lambda = Some(25.0);
        model.linear_intercept = Some(0.1);
        model.linear_weights = Some(vec![0.5]);
        model.tree_blend_weight = Some(0.5);
        let expected = 0.5 * 0.15 + 0.5 * 0.35;
        assert!((model.predict(&row(day("2024-01-02"))).unwrap() - expected).abs() < 1e-12);
    }

    #[test]
    fn diagnostics_use_all_predictions_and_report_cross_sectional_ic() {
        let diagnostics = prediction_diagnostics(&[
            vec![(-0.02, -0.01), (0.0, 0.01), (0.03, 0.02)],
            vec![(-0.01, -0.03), (0.02, 0.01), (0.04, 0.05)],
        ]);
        assert_eq!(diagnostics.observations, 6);
        assert_eq!(diagnostics.decision_dates, 2);
        assert_eq!(diagnostics.positive_rank_ic_dates, 2);
        assert!((diagnostics.mean_rank_ic - 1.0).abs() < 1e-12);
        assert_eq!(
            diagnostics
                .buckets
                .iter()
                .map(|bucket| bucket.observations)
                .sum::<usize>(),
            6
        );
    }

    #[test]
    fn selection_layer_is_a_costed_diagnostic_not_the_live_budget() {
        let date = day("2024-01-02");
        let rows = [-2.0, -1.0, 1.0, 2.0]
            .into_iter()
            .enumerate()
            .map(|(index, score)| {
                let mut value = row(date);
                value.instrument_id = format!("TX{index}");
                value.features.insert("x_ret_1".into(), score);
                value.target = score * 0.05;
                value
            })
            .collect::<Vec<_>>();
        let mut model = constant_model(0.0);
        model.model_family = "ridge".into();
        model.ridge_lambda = Some(25.0);
        model.linear_intercept = Some(0.0);
        model.linear_weights = Some(vec![1.0]);
        model.tree_info.clear();
        let by_date = BTreeMap::from([(date, rows.iter().collect::<Vec<_>>())]);
        let diagnostic =
            selection_layer_metrics(&model, &by_date, &[date], 20, 4, &zero_execution_costs())
                .unwrap()
                .unwrap();

        assert_eq!(diagnostic.positions_per_side, 2);
        assert!((diagnostic.total_return - 0.075).abs() < 1e-12);
        assert_eq!(diagnostic.cost_drag, 0.0);
        assert_eq!(diagnostic.steps[0].long_positions, 2);
        assert_eq!(diagnostic.steps[0].short_positions, 2);
    }

    #[test]
    fn fixed_momentum_directional_arm_chooses_strongest_signals_without_side_quota() {
        let date = day("2024-02-01");
        let rows = [
            ("A", 0.40, 0.10),
            ("B", 0.20, 0.05),
            ("C", -0.30, -0.08),
            ("D", -0.10, -0.03),
        ]
        .into_iter()
        .map(|(instrument, momentum, target)| {
            let mut value = row(date);
            value.instrument_id = instrument.into();
            value.symbol = instrument.into();
            value.momentum_12_1 = Some(momentum);
            value.target = target;
            value
        })
        .collect::<Vec<_>>();
        let result = fixed_momentum_backtest(
            &rows,
            &FixedMomentumConfig {
                start: date,
                end: date,
                cadence_sessions: 20,
                max_positions: 2,
                position_weight: 0.5,
                costs: zero_execution_costs(),
                benchmark: None,
                survivorship_status: "test".into(),
            },
        )
        .unwrap();

        assert_eq!(result.directional.steps[0].long_positions, 1);
        assert_eq!(result.directional.steps[0].short_positions, 1);
        assert!((result.directional.steps[0].net_return - 0.09).abs() < 1e-12);
        assert_eq!(result.long_only.steps[0].long_positions, 2);
        assert_eq!(result.long_only.steps[0].short_positions, 0);
        assert!((result.long_only.steps[0].net_return - 0.075).abs() < 1e-12);
    }

    fn row(date: Date) -> TrainingRow {
        TrainingRow {
            date,
            instrument_id: "TX1".into(),
            symbol: "TEST".into(),
            isin: "SE0000000001".into(),
            sector: "Industrials".into(),
            bucket: UniverseBucket::LargeCap,
            momentum_12_1: Some(0.1),
            target: 0.01,
            market_target: Some(0.005),
            relative_target: Some(0.005),
            return_per_risk_target: Some(0.5),
            relative_return_per_risk_target: Some(0.25),
            relative_rank_target: Some(0.5),
            entry_price: 100.0,
            exit_price: 101.0,
            adv20_sek: 10_000_000.0,
            vol60: 0.02,
            borrow_fee_annualized: None,
            median_closing_spread_bps_20: None,
            sample_weight: 1.0,
            features: BTreeMap::from([("x_ret_1".into(), 0.5)]),
        }
    }

    fn zero_execution_costs() -> CostConfig {
        CostConfig {
            round_trip_bps: 0.0,
            round_trip_commission_bps: 0.0,
            round_trip_impact_bps: 0.0,
            fallback_spread_bps: 0.0,
            market_friction_multiple: 1.0,
            first_north_extra_bps: 0.0,
            short_borrow_bps: 0.0,
            short_availability_bps: 0.0,
            safety_margin_bps: 0.0,
        }
    }

    fn rising_benchmark(count: usize) -> BenchmarkHistory {
        let start = day("2024-01-01");
        BenchmarkHistory {
            format_version: "fixture".into(),
            symbol: "OMXSGI".into(),
            name: "fixture".into(),
            return_type: "gross_total_return".into(),
            currency: "SEK".into(),
            source: "fixture".into(),
            generated_at: "fixture".into(),
            bars: (0..count)
                .map(|index| {
                    let value = 100.0 * 1.001_f64.powi(index as i32);
                    equity_data::BenchmarkBar {
                        date: start + time::Duration::days(index as i64),
                        start_value: value,
                        end_value: value,
                        high_value: None,
                        low_value: None,
                    }
                })
                .collect(),
        }
    }

    #[test]
    fn direction_is_not_forced_and_unchanged_positions_do_not_round_trip() {
        let rows = [row(day("2024-01-02")), row(day("2024-01-03"))];
        let costs = CostConfig {
            round_trip_bps: 100.0,
            round_trip_commission_bps: 100.0,
            ..zero_execution_costs()
        };
        let result = backtest(
            &constant_model(0.02),
            &rows,
            &BacktestConfig {
                start: day("2024-01-02"),
                end: day("2024-01-03"),
                cadence_sessions: 1,
                model_horizon_sessions: 1,
                prediction_horizon_scale: 1.0,
                max_positions: 1,
                retention_rank: 1,
                max_sector_gross: None,
                ranking: portfolio_construction::RankingMethod::Edge,
                sizing: portfolio_construction::SizingMethod::Equal,
                allocation_budget: portfolio_construction::Budget::gross_only(1.0).unwrap(),
                position_weight: 0.05,
                min_position_weight: 0.0,
                reference_edge: 0.01,
                reference_volatility: 0.02,
                direction_config: None,
                prediction_composition: PredictionComposition::Direct,
                market_return_forecasts: None,
                market_forecast_model_id: None,
                costs,
                benchmark: None,
            },
        )
        .unwrap();
        assert_eq!(result.metrics.long_positions, 2);
        assert_eq!(result.metrics.short_positions, 0);
        assert!(result.steps.iter().all(|step| step.net == 0.05));
        assert!(result.steps.iter().all(|step| {
            let allocation = step.allocation.as_ref().unwrap();
            (allocation.unused_gross - 0.95).abs() < 1e-12 && allocation.realised_short == 0.0
        }));
        // One 50 bp entry and one 50 bp terminal exit on a 5% position.
        assert!((result.metrics.cost_drag - 0.0005).abs() < 1e-12);
        assert!((result.steps.iter().map(|step| step.turnover).sum::<f64>() - 0.1).abs() < 1e-12);
    }

    #[test]
    fn shorter_horizon_forecast_is_scaled_before_the_economic_hurdle() {
        let costs = CostConfig {
            round_trip_bps: 50.0,
            round_trip_commission_bps: 50.0,
            ..zero_execution_costs()
        };
        let result = backtest(
            &constant_model(0.002),
            &[row(day("2024-01-02"))],
            &BacktestConfig {
                start: day("2024-01-02"),
                end: day("2024-01-02"),
                cadence_sessions: 5,
                model_horizon_sessions: 1,
                prediction_horizon_scale: 5.0,
                max_positions: 1,
                retention_rank: 1,
                max_sector_gross: None,
                ranking: portfolio_construction::RankingMethod::Edge,
                sizing: portfolio_construction::SizingMethod::Equal,
                allocation_budget: portfolio_construction::Budget::gross_only(0.05).unwrap(),
                position_weight: 0.05,
                min_position_weight: 0.0,
                reference_edge: 0.01,
                reference_volatility: 0.02,
                direction_config: None,
                prediction_composition: PredictionComposition::Direct,
                market_return_forecasts: None,
                market_forecast_model_id: None,
                costs,
                benchmark: None,
            },
        )
        .unwrap();
        assert_eq!(result.model_horizon_sessions, 1);
        assert_eq!(result.prediction_horizon_scale, 5.0);
        assert_eq!(result.metrics.long_positions, 1);
        assert!((result.steps[0].positions[0].predicted_return - 0.01).abs() < 1e-12);
    }

    #[test]
    fn relative_selection_is_composed_with_market_return_before_choosing_side() {
        let benchmark = rising_benchmark(330);
        let decision_date = benchmark.bars[300].date;
        let mut model = constant_model(-0.01);
        model.reward = "relative_return".into();
        let result = backtest(
            &model,
            &[row(decision_date)],
            &BacktestConfig {
                start: decision_date,
                end: decision_date,
                cadence_sessions: 1,
                model_horizon_sessions: 1,
                prediction_horizon_scale: 1.0,
                max_positions: 1,
                retention_rank: 1,
                max_sector_gross: None,
                ranking: portfolio_construction::RankingMethod::Edge,
                sizing: portfolio_construction::SizingMethod::Equal,
                allocation_budget: portfolio_construction::Budget::gross_only(1.0).unwrap(),
                position_weight: 0.05,
                min_position_weight: 0.0,
                reference_edge: 0.01,
                reference_volatility: 0.02,
                direction_config: Some(
                    portfolio_construction::DirectionConfig::baseline(1.0).unwrap(),
                ),
                prediction_composition: PredictionComposition::Direct,
                market_return_forecasts: Some(BTreeMap::from([(decision_date, 0.03)])),
                market_forecast_model_id: Some("market-fixture".into()),
                costs: zero_execution_costs(),
                benchmark: Some(benchmark),
            },
        )
        .unwrap();
        let position = &result.steps[0].positions[0];
        assert!(matches!(position.direction, Direction::Long));
        assert!((position.relative_prediction.unwrap() + 0.01).abs() < 1e-12);
        assert!((position.market_return_prediction.unwrap() - 0.03).abs() < 1e-12);
        assert!((position.predicted_return - 0.02).abs() < 1e-12);
    }

    #[test]
    fn absolute_ordering_can_be_centered_and_composed_with_a_market_forecast() {
        let benchmark = rising_benchmark(330);
        let decision_date = benchmark.bars[300].date;
        let result = backtest(
            &constant_model(0.02),
            &[row(decision_date)],
            &BacktestConfig {
                start: decision_date,
                end: decision_date,
                cadence_sessions: 1,
                model_horizon_sessions: 1,
                prediction_horizon_scale: 1.0,
                max_positions: 1,
                retention_rank: 1,
                max_sector_gross: None,
                ranking: portfolio_construction::RankingMethod::Edge,
                sizing: portfolio_construction::SizingMethod::Equal,
                allocation_budget: portfolio_construction::Budget::gross_only(1.0).unwrap(),
                position_weight: 0.05,
                min_position_weight: 0.0,
                reference_edge: 0.01,
                reference_volatility: 0.02,
                direction_config: Some(
                    portfolio_construction::DirectionConfig::baseline(1.0).unwrap(),
                ),
                prediction_composition: PredictionComposition::CrossSectionalResidualPlusMarket,
                market_return_forecasts: Some(BTreeMap::from([(decision_date, -0.03)])),
                market_forecast_model_id: Some("market-fixture".into()),
                costs: zero_execution_costs(),
                benchmark: Some(benchmark),
            },
        )
        .unwrap();
        let position = &result.steps[0].positions[0];
        assert!(matches!(position.direction, Direction::Short));
        assert_eq!(position.relative_prediction, Some(0.0));
        assert_eq!(position.market_return_prediction, Some(-0.03));
        assert!((position.predicted_return + 0.03).abs() < 1e-12);
    }

    #[test]
    fn measured_spread_replaces_fallback_and_cost_components_reconcile() {
        let mut measured = row(day("2024-01-02"));
        measured.median_closing_spread_bps_20 = Some(20.0);
        let costs = CostConfig {
            fallback_spread_bps: 100.0,
            ..CostConfig::default()
        };
        let result = backtest(
            &constant_model(0.02),
            &[measured],
            &BacktestConfig {
                start: day("2024-01-02"),
                end: day("2024-01-02"),
                cadence_sessions: 1,
                model_horizon_sessions: 1,
                prediction_horizon_scale: 1.0,
                max_positions: 1,
                retention_rank: 1,
                max_sector_gross: None,
                ranking: portfolio_construction::RankingMethod::Edge,
                sizing: portfolio_construction::SizingMethod::Equal,
                allocation_budget: portfolio_construction::Budget::gross_only(0.05).unwrap(),
                position_weight: 0.05,
                min_position_weight: 0.0,
                reference_edge: 0.01,
                reference_volatility: 0.02,
                direction_config: None,
                prediction_composition: PredictionComposition::Direct,
                market_return_forecasts: None,
                market_forecast_model_id: None,
                costs,
                benchmark: None,
            },
        )
        .unwrap();
        let diagnostics = result.execution_cost_diagnostics;
        assert_eq!(diagnostics.matrix_rows_with_observed_spread, 1);
        assert_eq!(
            diagnostics.selected_position_periods_with_observed_spread,
            1
        );
        // A 5% position turns over twice. Commission/spread/impact are 10/20/5
        // bps round trip, respectively; the 100 bps fallback is not charged.
        assert!((diagnostics.commission_drag - 0.00005).abs() < 1e-12);
        assert!((diagnostics.observed_spread_drag - 0.0001).abs() < 1e-12);
        assert!((diagnostics.impact_drag - 0.000025).abs() < 1e-12);
        assert_eq!(diagnostics.fallback_spread_drag, 0.0);
        assert!((result.metrics.cost_drag - 0.000175).abs() < 1e-12);
    }

    #[test]
    fn market_friction_stress_does_not_multiply_commission() {
        let mut measured = row(day("2024-01-02"));
        measured.median_closing_spread_bps_20 = Some(20.0);
        let costs = CostConfig {
            market_friction_multiple: 2.0,
            round_trip_bps: 60.0,
            ..CostConfig::default()
        };
        let cost = execution_cost(&measured, &costs).unwrap();
        assert_eq!(cost.commission_bps, 10.0);
        assert_eq!(cost.impact_bps, 10.0);
        assert_eq!(cost.spread_bps, 40.0);
        assert_eq!(cost.total_bps(), 60.0);
    }

    #[test]
    fn borrow_diagnostics_separate_observed_cost_from_availability_penalty() {
        let mut observed = row(day("2024-01-02"));
        observed.borrow_fee_annualized = Some(0.252);
        let result = backtest(
            &constant_model(-0.10),
            &[observed],
            &BacktestConfig {
                start: day("2024-01-02"),
                end: day("2024-01-02"),
                cadence_sessions: 1,
                model_horizon_sessions: 1,
                prediction_horizon_scale: 1.0,
                max_positions: 1,
                retention_rank: 1,
                max_sector_gross: None,
                ranking: portfolio_construction::RankingMethod::Edge,
                sizing: portfolio_construction::SizingMethod::Equal,
                allocation_budget: portfolio_construction::Budget::gross_only(1.0).unwrap(),
                position_weight: 0.05,
                min_position_weight: 0.0,
                reference_edge: 0.01,
                reference_volatility: 0.02,
                direction_config: None,
                prediction_composition: PredictionComposition::Direct,
                market_return_forecasts: None,
                market_forecast_model_id: None,
                costs: CostConfig {
                    short_borrow_bps: 10.0,
                    short_availability_bps: 20.0,
                    ..zero_execution_costs()
                },
                benchmark: None,
            },
        )
        .unwrap();
        let borrow = result.borrow_diagnostics;
        assert_eq!(borrow.matrix_rows, 1);
        assert_eq!(borrow.matrix_rows_with_fee, 1);
        assert_eq!(borrow.short_position_periods, 1);
        assert_eq!(borrow.short_position_periods_with_fee, 1);
        assert!((borrow.observed_holding_cost_drag - 0.00005).abs() < 1e-12);
        assert_eq!(borrow.fallback_holding_cost_drag, 0.0);
        assert!((borrow.availability_penalty_drag - 0.0001).abs() < 1e-12);
    }

    #[test]
    fn direction_overlay_is_warmed_before_the_fold_and_reported_separately() {
        let benchmark = rising_benchmark(330);
        let rows = [row(benchmark.bars[300].date), row(benchmark.bars[305].date)];
        let result = backtest(
            &constant_model(0.02),
            &rows,
            &BacktestConfig {
                start: rows[0].date,
                end: rows[1].date,
                cadence_sessions: 1,
                model_horizon_sessions: 1,
                prediction_horizon_scale: 1.0,
                max_positions: 1,
                retention_rank: 1,
                max_sector_gross: None,
                ranking: portfolio_construction::RankingMethod::Edge,
                sizing: portfolio_construction::SizingMethod::Equal,
                allocation_budget: portfolio_construction::Budget::gross_only(1.0).unwrap(),
                position_weight: 0.05,
                min_position_weight: 0.0,
                reference_edge: 0.01,
                reference_volatility: 0.02,
                direction_config: Some(
                    portfolio_construction::DirectionConfig::baseline(1.0).unwrap(),
                ),
                prediction_composition: PredictionComposition::Direct,
                market_return_forecasts: None,
                market_forecast_model_id: None,
                costs: zero_execution_costs(),
                benchmark: Some(benchmark),
            },
        )
        .unwrap();
        assert!(result.steps.iter().all(|step| {
            let direction = step.direction.as_ref().unwrap();
            direction.decision.regime == portfolio_construction::MarketRegime::StrongUp
                && (direction.decision.budget.max_gross - 1.0).abs() < 1e-12
                && (direction.decision.budget.target_net.unwrap() - 0.5).abs() < 1e-12
                && step.direction_market_return.unwrap() > 0.0
        }));
        let direction = result.direction_metrics.unwrap();
        assert_eq!(direction.periods, 2);
        assert_eq!(direction.strong_up_periods, 2);
        assert!(direction.total_return > 0.0);
    }

    #[test]
    fn benchmark_uses_next_session_sod_over_the_same_horizon() {
        let history = BenchmarkHistory {
            format_version: "fixture".into(),
            symbol: "OMXSGI".into(),
            name: "fixture".into(),
            return_type: "gross_total_return".into(),
            currency: "SEK".into(),
            source: "fixture".into(),
            generated_at: "fixture".into(),
            bars: [
                ("2024-01-02", 99.0),
                ("2024-01-03", 100.0),
                ("2024-01-04", 102.0),
                ("2024-01-05", 103.0),
            ]
            .into_iter()
            .map(|(date, value)| equity_data::BenchmarkBar {
                date: day(date),
                start_value: value,
                end_value: value,
                high_value: None,
                low_value: None,
            })
            .collect(),
        };
        let result = backtest(
            &constant_model(0.02),
            &[row(day("2024-01-02"))],
            &BacktestConfig {
                start: day("2024-01-02"),
                end: day("2024-01-02"),
                cadence_sessions: 1,
                model_horizon_sessions: 1,
                prediction_horizon_scale: 1.0,
                max_positions: 1,
                retention_rank: 1,
                max_sector_gross: None,
                ranking: portfolio_construction::RankingMethod::Edge,
                sizing: portfolio_construction::SizingMethod::Equal,
                allocation_budget: portfolio_construction::Budget::gross_only(0.05).unwrap(),
                position_weight: 0.05,
                min_position_weight: 0.0,
                reference_edge: 0.01,
                reference_volatility: 0.02,
                direction_config: None,
                prediction_composition: PredictionComposition::Direct,
                market_return_forecasts: None,
                market_forecast_model_id: None,
                costs: zero_execution_costs(),
                benchmark: Some(history),
            },
        )
        .unwrap();
        assert!((result.steps[0].benchmark_period_return.unwrap() - 0.02).abs() < 1e-12);
        assert_eq!(result.benchmark.unwrap().symbol, "OMXSGI");
    }
}
