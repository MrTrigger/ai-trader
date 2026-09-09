use std::collections::BTreeMap;
use std::path::Path;
use std::str::FromStr;

use rust_decimal::Decimal;
use toml::Value;

#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RankerSelection {
    #[default]
    Global,
    PerSide,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RankerAllocation {
    #[default]
    Balanced,
    Breadth,
    Confidence,
    Timing,
}

#[derive(Debug, Clone)]
pub struct RiskLimits {
    /// Fraction of one hour's volume a single position may represent. The cap
    /// is the LOWER of this and `max_position`, so a thin name is limited by
    /// the market and a liquid one by the mandate.
    pub participation_limit: Decimal,
    pub capacity_balance: bool,
    pub max_gross_exposure: Decimal,
    pub max_position: Decimal,
    pub max_position_count: usize,
    pub min_position_notional: Decimal,
    pub max_net_exposure: Option<Decimal>,
    pub max_cluster_exposure: Option<Decimal>,
    pub max_benchmark_beta: Option<Decimal>,
}

#[derive(Debug, Clone)]
pub struct CostModel {
    pub commission_bps: Decimal,
    /// The fallback spread, charged to any name the liquidity profile has not
    /// measured. One number for every asset is right for the liquid majority
    /// and an order of magnitude out on the tail, which is what the profile
    /// exists to correct.
    pub spread_bps: Decimal,
    pub impact_coefficient: Decimal,
    pub adv_lookback_days: usize,
    pub calibrated: bool,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub bot_id: String,
    pub ruleset_version: String,
    pub quote_currency: String,
    pub interval_s: i64,
    pub target_gross_exposure: Decimal,
    /// Ex-ante sum of absolute weight times volatility for vol-parity constructors.
    pub vol_parity_budget: Decimal,
    pub constructor: String,
    pub signal: String,
    pub max_holdings: usize,
    pub min_cross_section: usize,
    pub rebalance_every: usize,
    pub universe: Vec<String>,
    pub benchmark: Option<String>,
    pub benchmark_only: bool,
    pub min_dollar_volume: Decimal,
    /// Eligibility floor on the volume of the hours this bot actually trades
    /// in, from the liquidity profile. `min_dollar_volume` screens on a whole
    /// day, which a name can clear on volume that arrives in hours we are
    /// asleep for — KAITO clears the daily floor comfortably and trades
    /// $55,720 in the hour we send into it.
    ///
    /// Zero disables it, which is the behaviour that shipped. A name the
    /// profile has not measured keeps its eligibility rather than being
    /// assumed thin, for the same reason the participation cap leaves it on
    /// the mandate cap: absence of a measurement is not a measurement.
    pub min_hourly_quote_volume: Decimal,
    /// How many slices the executor will split each order into. The cost model
    /// needs it because impact meets one slice against one hour, not the whole
    /// order against a day — see `estimate`. It is the runner that decides the
    /// schedule; this is the planner's belief about it, and a mismatch makes
    /// the estimate wrong in a knowable direction rather than a mysterious one.
    pub execution_slices: usize,
    /// Where the measured liquidity profile lives, if there is one. Absent
    /// means the flat spread and the daily-volume impact term, which is the
    /// behaviour that shipped.
    pub liquidity_profile: Option<std::path::PathBuf>,
    pub min_volatility: Decimal,
    pub min_history_bars: u32,
    pub rebalance_cost_multiple: Decimal,
    pub turnover_budget: Decimal,
    /// Round-2 no-trade band: a resize (Increase/Reduce, same sign) whose
    /// |drift| is under this fraction of the CURRENT position is skipped.
    /// Entries and exits are never banded. Zero (the default, and the
    /// frozen bot's value) disables it.
    pub rebalance_drift_band: Decimal,
    /// Market matrix and timing model for `market_model_tilt` or the ranker's
    /// optional `timing` allocation policy.
    pub market_matrix_path: Option<String>,
    pub timing_model_path: Option<String>,
    /// Round-3 selection pool: how many names, by rank-day liquidity, the
    /// strength-selection signal considers before keeping the strongest
    /// `max_holdings`. Default 30 (the round-2 V6 value).
    pub selection_pool: usize,
    /// Candidate count across both sides for the two score-sized ranker
    /// constructors. Independent of directional selection_pool/max_holdings.
    pub ranker_selection_count: usize,
    pub ranker_selection: RankerSelection,
    pub ranker_allocation: RankerAllocation,
    pub ranker_net_band: Decimal,
    pub ranker_trend_weight: Decimal,
    pub ranker_capacity_selection: bool,
    /// Selection hysteresis for the score-selected ranker book: an incumbent
    /// keeps its slot while it ranks inside `ranker_selection_count * multiple`
    /// and its held side still agrees with the signal. None disables it.
    pub ranker_hold_rank_multiple: Option<Decimal>,
    /// Staggered sleeves: only one of N fixed asset sleeves may change weight
    /// on a given day. One (the default) trades the whole book daily.
    pub ranker_tranches: usize,
    pub ranker_trade_assets: Option<Vec<String>>,
    /// BTC momentum's capital share; the frozen ranker receives the remainder.
    /// None and zero preserve the original single-strategy path.
    pub ranker_btc_momentum_fraction: Option<Decimal>,
    /// Use BTC long when its prior completed 30-day return is positive, otherwise the ranker.
    pub ranker_btc_regime: bool,
    /// Remove constructed shorts whose prior completed 30-day return is positive.
    pub ranker_short_trend_veto: bool,
    /// Deduct adverse expected 24h funding from the ranker's price-return
    /// conviction. Only valid for the existing 24h-label ml_ranker recipe.
    pub funding_cost_adjustment: bool,
    pub native_funding_adjustment: bool,
    pub model_path: Option<String>,
    /// Add one full fixed directional mandate to the relative ranker.
    pub ranker_directional_model_path: Option<String>,
    pub model_schedule: Option<crate::model_schedule::Schedule>,
    pub limits: RiskLimits,
    pub clusters: BTreeMap<String, String>,
    pub costs: CostModel,
    pub overlay: Overlay,
}

/// Trend overlay parameters (`docs/research/trend-overlay.md`). Absent
/// `[overlay]` = the pre-registered base rule: band 0.30, horizons 30/90,
/// no vol scaling. Also supplies the ranker's optional breadth allocation and
/// the balanced book's shadow measurement.
#[derive(Debug, Clone, PartialEq)]
pub struct Overlay {
    pub band: Decimal,
    pub horizon_a: u32,
    pub horizon_b: u32,
    /// If set, N is multiplied by `min(1, vol_target / median x_vol_30)`.
    pub vol_target: Option<Decimal>,
}

impl Default for Overlay {
    fn default() -> Self {
        Overlay {
            band: Decimal::new(30, 2),
            horizon_a: 30,
            horizon_b: 90,
            vol_target: None,
        }
    }
}

fn table<'a>(root: &'a Value, key: &str) -> Result<&'a toml::value::Table, String> {
    root.get(key)
        .and_then(Value::as_table)
        .ok_or_else(|| format!("missing [{key}]"))
}

fn text(t: &toml::value::Table, key: &str) -> Result<String, String> {
    t.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("missing string {key}"))
}

fn dec(t: &toml::value::Table, key: &str) -> Result<Decimal, String> {
    Decimal::from_str(&text(t, key)?).map_err(|e| format!("{key}: {e}"))
}

fn opt_dec(t: &toml::value::Table, key: &str) -> Result<Option<Decimal>, String> {
    match t.get(key).and_then(Value::as_str) {
        None | Some("") => Ok(None),
        Some(v) => Decimal::from_str(v)
            .map(Some)
            .map_err(|e| format!("{key}: {e}")),
    }
}

fn usize_value(t: &toml::value::Table, key: &str) -> Result<usize, String> {
    t.get(key)
        .and_then(Value::as_integer)
        .and_then(|v| usize::try_from(v).ok())
        .ok_or_else(|| format!("missing positive integer {key}"))
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let source = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let root: Value =
            toml::from_str(&source).map_err(|e| format!("{}: {e}", path.display()))?;
        let meta = table(&root, "meta")?;
        let p = table(&root, "portfolio")?;
        let l = table(&root, "limits")?;
        let c = table(&root, "costs")?;

        let mut clusters = BTreeMap::new();
        if let Ok(groups) = table(&root, "clusters") {
            for (group, assets) in groups {
                let assets = assets
                    .as_array()
                    .ok_or_else(|| format!("cluster {group} is not an array"))?;
                for asset in assets {
                    let asset = asset
                        .as_str()
                        .ok_or_else(|| format!("cluster {group} has non-string asset"))?
                        .to_uppercase();
                    if let Some(old) = clusters.insert(asset.clone(), group.clone()) {
                        if old != *group {
                            return Err(format!("{asset} is in two clusters ({old} and {group})"));
                        }
                    }
                }
            }
        }

        let universe = p
            .get("universe")
            .and_then(Value::as_array)
            .ok_or("portfolio.universe is not an array")?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_uppercase)
                    .ok_or("universe contains non-string")
            })
            .collect::<Result<Vec<_>, _>>()?;

        let limits = RiskLimits {
            capacity_balance: l
                .get("capacity_balance")
                .map(|v| v.as_bool().ok_or("capacity_balance must be a boolean"))
                .transpose()?
                .unwrap_or(false),
            max_gross_exposure: dec(l, "max_gross_exposure")?,
            max_position: dec(l, "max_position")?,
            // Spec 8: the second half of a position's cap. 5% of an hour's
            // volume is the workable limit; 2% strands more capital than the
            // impact it avoids. Zero disables the cap entirely, which is the
            // behaviour that shipped.
            participation_limit: l
                .get("participation_limit")
                .map(|_| dec(l, "participation_limit"))
                .transpose()?
                .unwrap_or(Decimal::ZERO),
            max_position_count: usize_value(l, "max_position_count")?,
            min_position_notional: dec(l, "min_position_notional")?,
            max_net_exposure: opt_dec(l, "max_net_exposure")?,
            max_cluster_exposure: opt_dec(l, "max_cluster_exposure")?,
            max_benchmark_beta: opt_dec(l, "max_benchmark_beta")?,
        };
        let costs = CostModel {
            commission_bps: dec(c, "commission_bps")?,
            spread_bps: dec(c, "spread_bps")?,
            impact_coefficient: dec(c, "impact_coefficient")?,
            adv_lookback_days: usize_value(c, "adv_lookback_days")?,
            calibrated: c
                .get("calibrated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        };

        let cfg = Self {
            bot_id: text(meta, "bot_id")?,
            ruleset_version: text(meta, "ruleset_version")?,
            quote_currency: text(p, "quote_currency")?,
            interval_s: p
                .get("interval_s")
                .and_then(Value::as_integer)
                .ok_or("missing interval_s")?,
            target_gross_exposure: dec(p, "target_gross_exposure")?,
            vol_parity_budget: p
                .get("vol_parity_budget")
                .map(|_| dec(p, "vol_parity_budget"))
                .transpose()?
                .unwrap_or(Decimal::new(25, 2)),
            constructor: text(p, "constructor")?,
            signal: text(p, "signal")?,
            max_holdings: usize_value(p, "max_holdings")?,
            min_cross_section: usize_value(p, "min_cross_section")?,
            rebalance_every: usize_value(p, "rebalance_every")?,
            universe,
            benchmark: p
                .get("benchmark")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(str::to_owned),
            benchmark_only: p
                .get("benchmark_only")
                .map(|v| v.as_bool().ok_or("benchmark_only must be a boolean"))
                .transpose()?
                .unwrap_or(false),
            min_dollar_volume: dec(p, "min_dollar_volume")?,
            min_hourly_quote_volume: p
                .get("min_hourly_quote_volume")
                .map(|_| dec(p, "min_hourly_quote_volume"))
                .transpose()?
                .unwrap_or(Decimal::ZERO),
            execution_slices: p
                .get("execution_slices")
                .map(|_| usize_value(p, "execution_slices"))
                .transpose()?
                .unwrap_or(1),
            liquidity_profile: p
                .get("liquidity_profile")
                .and_then(Value::as_str)
                .map(std::path::PathBuf::from),
            min_volatility: dec(p, "min_volatility")?,
            min_history_bars: u32::try_from(usize_value(p, "min_history_bars")?)
                .map_err(|e| e.to_string())?,
            rebalance_cost_multiple: dec(p, "rebalance_cost_multiple")?,
            turnover_budget: dec(p, "turnover_budget")?,
            rebalance_drift_band: p
                .get("rebalance_drift_band")
                .map(|_| dec(p, "rebalance_drift_band"))
                .transpose()?
                .unwrap_or(Decimal::ZERO),
            market_matrix_path: p
                .get("market_matrix_path")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(str::to_owned),
            timing_model_path: p
                .get("timing_model_path")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(str::to_owned),
            selection_pool: p
                .get("selection_pool")
                .map(|_| usize_value(p, "selection_pool"))
                .transpose()?
                .unwrap_or(30),
            ranker_selection_count: p
                .get("ranker_selection_count")
                .map(|_| usize_value(p, "ranker_selection_count"))
                .transpose()?
                .unwrap_or(24),
            ranker_selection: p
                .get("ranker_selection")
                .map(|v| v.clone().try_into())
                .transpose()
                .map_err(|e| format!("invalid ranker_selection: {e}"))?
                .unwrap_or_default(),
            ranker_allocation: p
                .get("ranker_allocation")
                .map(|v| v.clone().try_into())
                .transpose()
                .map_err(|e| format!("invalid ranker_allocation: {e}"))?
                .unwrap_or_default(),
            ranker_net_band: p
                .get("ranker_net_band")
                .map(|_| dec(p, "ranker_net_band"))
                .transpose()?
                .unwrap_or(Decimal::new(1, 1)),
            ranker_trend_weight: p
                .get("ranker_trend_weight")
                .map(|_| dec(p, "ranker_trend_weight"))
                .transpose()?
                .unwrap_or(Decimal::ZERO),
            funding_cost_adjustment: p
                .get("funding_cost_adjustment")
                .map(|value| {
                    value
                        .as_bool()
                        .ok_or("funding_cost_adjustment must be a boolean")
                })
                .transpose()?
                .unwrap_or(false),
            native_funding_adjustment: p
                .get("native_funding_adjustment")
                .map(|value| {
                    value
                        .as_bool()
                        .ok_or("native_funding_adjustment must be a boolean")
                })
                .transpose()?
                .unwrap_or(false),
            ranker_trade_assets: p
                .get("ranker_trade_assets")
                .map(|value| {
                    value
                        .as_array()
                        .ok_or("ranker_trade_assets must be an array of symbols")?
                        .iter()
                        .map(|asset| {
                            asset
                                .as_str()
                                .map(str::to_owned)
                                .ok_or("ranker_trade_assets must contain strings")
                        })
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?,
            ranker_capacity_selection: p
                .get("ranker_capacity_selection")
                .map(|v| {
                    v.as_bool()
                        .ok_or("ranker_capacity_selection must be a boolean")
                })
                .transpose()?
                .unwrap_or(false),
            ranker_hold_rank_multiple: opt_dec(p, "ranker_hold_rank_multiple")?,
            ranker_tranches: p
                .get("ranker_tranches")
                .map(|_| usize_value(p, "ranker_tranches"))
                .transpose()?
                .unwrap_or(1),
            ranker_btc_momentum_fraction: opt_dec(p, "ranker_btc_momentum_fraction")?,
            ranker_btc_regime: p
                .get("ranker_btc_regime")
                .map(|v| v.as_bool().ok_or("ranker_btc_regime must be a boolean"))
                .transpose()?
                .unwrap_or(false),
            ranker_short_trend_veto: p
                .get("ranker_short_trend_veto")
                .map(|v| {
                    v.as_bool()
                        .ok_or("ranker_short_trend_veto must be a boolean")
                })
                .transpose()?
                .unwrap_or(false),
            ranker_directional_model_path: p
                .get("ranker_directional_model_path")
                .map(|value| {
                    value
                        .as_str()
                        .filter(|path| !path.trim().is_empty())
                        .map(str::to_owned)
                        .ok_or("ranker_directional_model_path must be a nonempty string")
                })
                .transpose()?,
            model_path: p
                .get("model_path")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(str::to_owned),
            model_schedule: p
                .get("model_schedule_path")
                .map(|v| {
                    v.as_str()
                        .ok_or_else(|| "model_schedule_path must be a path string".to_string())
                        .and_then(|path| crate::model_schedule::Schedule::load(Path::new(path)))
                })
                .transpose()?,
            limits,
            clusters,
            costs,
            overlay: match root.get("overlay").and_then(Value::as_table) {
                None => Overlay::default(),
                Some(o) => Overlay {
                    band: dec(o, "band")?,
                    horizon_a: u32::try_from(usize_value(o, "horizon_a")?)
                        .map_err(|e| e.to_string())?,
                    horizon_b: u32::try_from(usize_value(o, "horizon_b")?)
                        .map_err(|e| e.to_string())?,
                    vol_target: opt_dec(o, "vol_target")?,
                },
            },
        };
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), String> {
        if let Some(assets) = &self.ranker_trade_assets {
            let unique: std::collections::BTreeSet<_> = assets.iter().collect();
            if assets.is_empty()
                || unique.len() != assets.len()
                || assets
                    .iter()
                    .any(|asset| asset.is_empty() || asset.chars().any(char::is_whitespace))
            {
                return Err(
                    "ranker_trade_assets requires unique nonempty symbols and a nonempty list"
                        .into(),
                );
            }
            if self.signal != "ml_ranker"
                || self.constructor != "tsmom_vol_parity"
                || self.ranker_btc_momentum_fraction.is_some()
                || self.ranker_btc_regime
                || self.ranker_short_trend_veto
                || self.ranker_directional_model_path.is_some()
                || self.funding_cost_adjustment
                || self.native_funding_adjustment
                || self.limits.capacity_balance
                || self.ranker_capacity_selection
                || self.model_schedule.is_some()
            {
                return Err("ranker_trade_assets requires ml_ranker/tsmom_vol_parity without blend, regime, short veto, directional overlay, funding adjustment, capacity balance or model schedule".into());
            }
        }
        if self.ranker_tranches == 0 {
            return Err("ranker_tranches must be at least one".into());
        }
        if self.ranker_tranches > 1
            && (self.constructor != "risk_adjusted_score_selected" || self.signal != "ml_ranker")
        {
            return Err("ranker_tranches above one requires ml_ranker with risk_adjusted_score_selected".into());
        }
        if let Some(multiple) = self.ranker_hold_rank_multiple {
            if multiple < Decimal::ONE
                || self.constructor != "risk_adjusted_score_selected"
                || self.signal != "ml_ranker"
                || self.ranker_selection != RankerSelection::Global
            {
                return Err("ranker_hold_rank_multiple must be at least one and requires ml_ranker with risk_adjusted_score_selected and global selection".into());
            }
        }
        if self.vol_parity_budget <= Decimal::ZERO || self.vol_parity_budget > Decimal::ONE {
            return Err("vol_parity_budget must be positive and at most one".into());
        }
        if self.vol_parity_budget != Decimal::new(25, 2)
            && !matches!(
                self.constructor.as_str(),
                "tsmom_vol_parity" | "ranker_cost_aware_vol_parity"
            )
        {
            return Err("nondefault vol_parity_budget requires tsmom_vol_parity or ranker_cost_aware_vol_parity".into());
        }
        if self.constructor == "ranker_cost_aware_vol_parity" && self.signal != "ml_ranker" {
            return Err(
                "ranker_cost_aware_vol_parity requires ml_ranker expected-return convictions"
                    .into(),
            );
        }
        if self.ranker_directional_model_path.is_some()
            && (self.signal != "ml_ranker"
                || self.constructor != "risk_adjusted_score_selected"
                || self.ranker_allocation != RankerAllocation::Balanced
                || self.ranker_btc_momentum_fraction.is_some()
                || self.ranker_btc_regime
                || self.ranker_short_trend_veto
                || self.funding_cost_adjustment
                || self.native_funding_adjustment
                || self.limits.capacity_balance
                || self.model_schedule.is_some()
                || self.target_gross_exposure != Decimal::new(160, 2))
        {
            return Err("ranker_directional_model_path requires balanced selected ml_ranker, explicit gross 1.60, and excludes BTC blend/regime/short veto/funding adjustment/capacity balance/model schedules".into());
        }
        if self.constructor == "risk_adjusted_score_unbalanced" && self.signal != "ml_ranker" {
            return Err(
                "risk_adjusted_score_unbalanced requires ml_ranker expected-return convictions"
                    .into(),
            );
        }
        if self.ranker_short_trend_veto
            && (self.signal != "ml_ranker"
                || self.constructor != "risk_adjusted_score_selected"
                || self.ranker_allocation != RankerAllocation::Balanced
                || self.ranker_btc_momentum_fraction.is_some()
                || self.ranker_btc_regime
                || self.native_funding_adjustment
                || self.target_gross_exposure != Decimal::new(8, 1)
                || self.limits.max_position != Decimal::new(25, 2)
                || self.limits.max_cluster_exposure != Some(Decimal::new(4, 1)))
        {
            return Err("ranker_short_trend_veto requires balanced ml_ranker/risk_adjusted_score_selected, gross 0.80, position cap 0.25, cluster cap 0.40, and excludes static blend/regime/native funding adjustment".into());
        }
        if self.ranker_btc_regime
            && (self.ranker_btc_momentum_fraction.is_some()
                || self.native_funding_adjustment
                || self.signal != "ml_ranker"
                || self.constructor != "risk_adjusted_score_selected"
                || self.ranker_allocation != RankerAllocation::Balanced
                || self.benchmark.as_deref() != Some("BTC")
                || self.benchmark_only
                || self.target_gross_exposure != Decimal::new(8, 1))
        {
            return Err("ranker_btc_regime requires balanced ml_ranker/risk_adjusted_score_selected, benchmark BTC, gross 0.80, and excludes static blend/native funding adjustment".into());
        }
        if self.native_funding_adjustment
            && (self.signal != "ml_ranker"
                || self.funding_cost_adjustment
                || self.ranker_btc_momentum_fraction.is_some())
        {
            return Err("native_funding_adjustment requires ml_ranker and excludes external funding adjustment and ranker/BTC blending".into());
        }

        if let Some(fraction) = self.ranker_btc_momentum_fraction {
            if !(Decimal::ZERO..=Decimal::ONE).contains(&fraction) {
                return Err("ranker_btc_momentum_fraction must lie between zero and one".into());
            }
            if fraction > Decimal::ZERO && self.target_gross_exposure != Decimal::new(8, 1) {
                return Err("active ranker BTC blend requires the fixed 0.80 gross mandate".into());
            }
            if self.signal != "ml_ranker"
                || self.constructor != "risk_adjusted_score_selected"
                || self.ranker_allocation != RankerAllocation::Balanced
                || self.benchmark.as_deref() != Some("BTC")
                || self.benchmark_only
            {
                return Err("ranker BTC blend requires balanced ml_ranker/risk_adjusted_score_selected and benchmark BTC".into());
            }
        }

        if self.benchmark_only
            && (self.benchmark.is_none()
                || !matches!(
                    self.signal.as_str(),
                    "tsmom_ensemble"
                        | "tsmom_fast_ensemble"
                        | "tsmom_z"
                        | "donchian_ensemble"
                        | "donchian_ensemble_ls"
                        | "market_model_tilt"
                        | "benchmark_consensus"
                ))
        {
            return Err(
                "benchmark_only requires a benchmark and a supported absolute directional signal"
                    .into(),
            );
        }
        if self.signal == "benchmark_consensus" && !self.benchmark_only {
            return Err("benchmark consensus requires benchmark_only".into());
        }
        if self.model_schedule.is_some() && self.signal != "ml_ranker" {
            return Err("model schedule requires ml_ranker".into());
        }
        if self.ranker_capacity_selection
            && (!self.limits.capacity_balance
                || self.target_gross_exposure <= Decimal::ZERO
                || self.constructor != "risk_adjusted_score_selected"
                || self.signal != "ml_ranker")
        {
            return Err(
                "ranker_capacity_selection requires capacity_balance and score-selected ml_ranker"
                    .into(),
            );
        }
        if self.limits.capacity_balance
            && (self.limits.participation_limit <= Decimal::ZERO
                || self.ranker_allocation != RankerAllocation::Balanced)
        {
            return Err(
                "capacity_balance requires positive participation_limit and balanced allocation"
                    .into(),
            );
        }
        if self.ranker_net_band < Decimal::ZERO
            || (self.ranker_allocation != RankerAllocation::Balanced
                && self.ranker_net_band > self.target_gross_exposure)
        {
            return Err("ranker_net_band must lie between zero and target gross exposure".into());
        }
        if !(Decimal::ZERO..=Decimal::ONE).contains(&self.ranker_trend_weight) {
            return Err("ranker_trend_weight must lie between zero and one".into());
        }
        if self.ranker_selection == RankerSelection::PerSide && self.ranker_selection_count % 2 != 0
        {
            return Err("per_side selection requires an even ranker_selection_count".into());
        }
        if (self.ranker_selection != RankerSelection::Global
            || self.ranker_allocation != RankerAllocation::Balanced
            || self.ranker_trend_weight != Decimal::ZERO)
            && (self.constructor != "risk_adjusted_score_selected" || self.signal != "ml_ranker")
        {
            return Err("ranker selection/trend policies require ml_ranker and risk_adjusted_score_selected".into());
        }
        if self.ranker_allocation == RankerAllocation::Timing
            && (self.timing_model_path.is_none() || self.market_matrix_path.is_none())
        {
            return Err(
                "timing allocation requires timing_model_path and market_matrix_path".into(),
            );
        }
        if !(4..=1000).contains(&self.ranker_selection_count) {
            return Err("ranker_selection_count must be between 4 and 1000".into());
        }
        if self.funding_cost_adjustment && self.signal != "ml_ranker" {
            return Err(
                "funding_cost_adjustment requires ml_ranker with 24h price-return labels".into(),
            );
        }
        if self.interval_s != 86_400 {
            return Err("crypto portfolio currently requires daily decision bars".into());
        }
        if self.max_holdings == 0 || self.rebalance_every == 0 {
            return Err("holdings and rebalance cadence must be positive".into());
        }
        if self.limits.max_position_count == 0 {
            return Err("max_position_count must be positive".into());
        }
        if self.target_gross_exposure < Decimal::ZERO {
            return Err("target gross cannot be negative".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trade_assets_parse_strictly_and_reject_ignored_policies() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config/default.toml");
        let source = std::fs::read_to_string(&path).unwrap();
        let mut cfg = Config::load(&path).unwrap();
        assert!(cfg.ranker_trade_assets.is_none());
        let file = std::env::temp_dir().join(format!(
            "valid-trade-assets-config-{}.toml",
            std::process::id()
        ));
        let valid = source
            .replace(
                "constructor = \"risk_adjusted\"",
                "constructor = \"tsmom_vol_parity\"",
            )
            .replace(
                "[portfolio]",
                "[portfolio]\nranker_trade_assets = [\"BTC\", \"ETH\"]",
            );
        std::fs::write(&file, valid).unwrap();
        assert_eq!(
            Config::load(&file).unwrap().ranker_trade_assets,
            Some(vec!["BTC".into(), "ETH".into()])
        );
        std::fs::remove_file(file).unwrap();

        cfg.signal = "ml_ranker".into();
        cfg.constructor = "tsmom_vol_parity".into();
        cfg.ranker_trade_assets = Some(vec!["BTC".into(), "ETH".into()]);
        assert!(cfg.validate().is_ok());
        for assets in [
            vec![],
            vec!["".into()],
            vec![" ".into()],
            vec!["BTC".into(), "BTC".into()],
        ] {
            let mut invalid = cfg.clone();
            invalid.ranker_trade_assets = Some(assets);
            assert!(invalid
                .validate()
                .unwrap_err()
                .contains("ranker_trade_assets"));
        }
        for policy in 0..10 {
            let mut invalid = cfg.clone();
            match policy {
                0 => invalid.signal = "tsmom_ensemble".into(),
                1 => invalid.constructor = "equal_weight".into(),
                2 => invalid.ranker_btc_momentum_fraction = Some(Decimal::ZERO),
                3 => invalid.ranker_btc_regime = true,
                4 => invalid.ranker_short_trend_veto = true,
                5 => invalid.ranker_directional_model_path = Some("ignored.json".into()),
                6 => invalid.funding_cost_adjustment = true,
                7 => invalid.native_funding_adjustment = true,
                8 => invalid.limits.capacity_balance = true,
                _ => invalid.ranker_capacity_selection = true,
            }
            assert!(invalid
                .validate()
                .unwrap_err()
                .contains("ranker_trade_assets"));
        }
        for value in [
            "true",
            "1",
            "\"BTC\"",
            "[1]",
            "[]",
            "[\"\"]",
            "[\"BTC\", \"BTC\"]",
        ] {
            let invalid = source.replace(
                "[portfolio]",
                &format!("[portfolio]\nranker_trade_assets = {value}"),
            );
            let file = std::env::temp_dir()
                .join(format!("trade-assets-config-{}.toml", std::process::id()));
            std::fs::write(&file, invalid).unwrap();
            assert!(Config::load(&file)
                .unwrap_err()
                .contains("ranker_trade_assets"));
            std::fs::remove_file(file).unwrap();
        }
    }

    #[test]
    fn vol_parity_budget_defaults_and_rejects_invalid_or_ignored_settings() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config/default.toml");
        let mut cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.vol_parity_budget, Decimal::new(25, 2));
        cfg.constructor = "risk_adjusted_score_selected".into();
        assert!(cfg.validate().is_ok());
        cfg.vol_parity_budget = Decimal::new(5, 1);
        assert!(cfg
            .validate()
            .unwrap_err()
            .contains("nondefault vol_parity_budget"));
        for constructor in ["tsmom_vol_parity", "ranker_cost_aware_vol_parity"] {
            cfg.constructor = constructor.into();
            cfg.signal = "ml_ranker".into();
            for budget in [Decimal::new(5, 1), Decimal::ONE] {
                cfg.vol_parity_budget = budget;
                assert!(cfg.validate().is_ok());
            }
            for budget in [Decimal::ZERO, -Decimal::ONE, Decimal::new(101, 2)] {
                cfg.vol_parity_budget = budget;
                assert!(cfg.validate().is_err());
            }
        }
        let source = std::fs::read_to_string(path).unwrap();
        let file =
            std::env::temp_dir().join(format!("vol-budget-config-{}.toml", std::process::id()));
        for invalid in ["false", "0.5", "1", "[]", "\"\"", "\"bad\""] {
            std::fs::write(
                &file,
                source.replace(
                    "[portfolio]",
                    &format!("[portfolio]\nvol_parity_budget = {invalid}"),
                ),
            )
            .unwrap();
            assert!(Config::load(&file)
                .unwrap_err()
                .contains("vol_parity_budget"));
        }
        std::fs::write(
            &file,
            source.replace("[portfolio]", "[portfolio]\nvol_parity_budget = \"0.25\""),
        )
        .unwrap();
        assert_eq!(
            Config::load(&file).unwrap().vol_parity_budget,
            Decimal::new(25, 2)
        );
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn cost_aware_vol_parity_requires_ml_ranker() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config/default.toml");
        let mut cfg = Config::load(&path).unwrap();
        cfg.constructor = "ranker_cost_aware_vol_parity".into();
        cfg.signal = "tsmom_ensemble".into();
        assert!(cfg.validate().unwrap_err().contains("requires ml_ranker"));
        cfg.signal = "ml_ranker".into();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn directional_overlay_config_is_explicit_and_excludes_other_overlays() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config/default.toml");
        let mut cfg = Config::load(&path).unwrap();
        assert!(cfg.ranker_directional_model_path.is_none());
        cfg.ranker_directional_model_path = Some("secondary.json".into());
        cfg.signal = "ml_ranker".into();
        cfg.constructor = "risk_adjusted_score_selected".into();
        cfg.target_gross_exposure = Decimal::new(16, 1);
        assert!(cfg.validate().is_ok());
        let valid = cfg.clone();
        for case in 0..10 {
            cfg = valid.clone();
            match case {
                0 => cfg.signal = "tsmom_ensemble".into(),
                1 => cfg.constructor = "risk_adjusted_unbalanced".into(),
                2 => cfg.ranker_allocation = RankerAllocation::Breadth,
                3 => cfg.ranker_btc_momentum_fraction = Some(Decimal::ZERO),
                4 => cfg.ranker_btc_regime = true,
                5 => cfg.ranker_short_trend_veto = true,
                6 => cfg.funding_cost_adjustment = true,
                7 => cfg.native_funding_adjustment = true,
                8 => cfg.limits.capacity_balance = true,
                _ => {
                    cfg.model_schedule = Some(crate::model_schedule::Schedule {
                        source: "schedule.json".into(),
                        entries: Vec::new(),
                    })
                }
            }
            assert!(cfg.validate().is_err(), "case {case}");
        }
        cfg = valid;
        cfg.target_gross_exposure = Decimal::new(8, 1);
        assert!(cfg.validate().is_err());
        let file = std::env::temp_dir().join(format!("overlay-config-{}.toml", std::process::id()));
        let source = std::fs::read_to_string(path).unwrap();
        for bad in ["false", "42", "\"\""] {
            std::fs::write(
                &file,
                source.replace(
                    "[portfolio]",
                    &format!("[portfolio]\nranker_directional_model_path = {bad}"),
                ),
            )
            .unwrap();
            assert!(Config::load(&file)
                .unwrap_err()
                .contains("must be a nonempty string"));
        }
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn score_unbalanced_requires_return_forecasts_without_changing_limits() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config/default.toml");
        let mut cfg = Config::load(&path).unwrap();
        cfg.constructor = "risk_adjusted_score_unbalanced".into();
        cfg.signal = "tsmom_ensemble".into();
        assert!(cfg.validate().unwrap_err().contains("requires ml_ranker"));
        cfg.signal = "ml_ranker".into();
        assert!(cfg.validate().is_ok());
        cfg.limits.max_position = Decimal::new(1, 1);
        cfg.target_gross_exposure = Decimal::new(6, 1);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn short_trend_veto_is_opt_in_and_rejects_incompatible_configs() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config/default.toml");
        let mut cfg = Config::load(&path).unwrap();
        assert!(!cfg.ranker_short_trend_veto);
        cfg.ranker_short_trend_veto = true;
        cfg.signal = "ml_ranker".into();
        cfg.constructor = "risk_adjusted_score_selected".into();
        assert!(cfg.validate().is_ok());
        let valid = cfg.clone();
        for change in 0..9 {
            cfg = valid.clone();
            match change {
                0 => cfg.signal = "tsmom_ensemble".into(),
                1 => cfg.constructor = "vol_neutral".into(),
                2 => cfg.ranker_allocation = RankerAllocation::Breadth,
                3 => cfg.ranker_btc_momentum_fraction = Some(Decimal::ZERO),
                4 => cfg.ranker_btc_regime = true,
                5 => cfg.native_funding_adjustment = true,
                6 => cfg.target_gross_exposure = Decimal::ONE,
                7 => cfg.limits.max_position = Decimal::new(5, 1),
                _ => cfg.limits.max_cluster_exposure = None,
            }
            assert!(cfg.validate().is_err(), "incompatible case {change}");
        }
        let file =
            std::env::temp_dir().join(format!("invalid-short-veto-{}.toml", std::process::id()));
        let source = std::fs::read_to_string(path).unwrap().replace(
            "[portfolio]",
            "[portfolio]\nranker_short_trend_veto = \"true\"",
        );
        std::fs::write(&file, source).unwrap();
        assert!(Config::load(&file)
            .unwrap_err()
            .contains("ranker_short_trend_veto must be a boolean"));
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn regime_config_is_opt_in_and_rejects_incompatible_policies() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config/default.toml");
        let mut cfg = Config::load(&path).unwrap();
        assert!(!cfg.ranker_btc_regime);
        cfg.ranker_btc_regime = true;
        cfg.signal = "ml_ranker".into();
        cfg.constructor = "risk_adjusted_score_selected".into();
        cfg.benchmark = Some("BTC".into());
        assert!(cfg.validate().is_ok());
        let valid = cfg.clone();
        cfg.ranker_btc_momentum_fraction = Some(Decimal::ZERO);
        assert!(cfg.validate().is_err());
        cfg = valid.clone();
        cfg.native_funding_adjustment = true;
        assert!(cfg.validate().is_err());
        cfg = valid.clone();
        cfg.signal = "tsmom_ensemble".into();
        assert!(cfg.validate().is_err());
        cfg = valid.clone();
        cfg.constructor = "risk_adjusted_unbalanced".into();
        assert!(cfg.validate().is_err());
        cfg = valid.clone();
        cfg.ranker_allocation = RankerAllocation::Breadth;
        assert!(cfg.validate().is_err());
        cfg = valid.clone();
        cfg.target_gross_exposure = Decimal::ONE;
        assert!(cfg.validate().is_err());
        cfg = valid.clone();
        cfg.benchmark = Some("ETH".into());
        assert!(cfg.validate().is_err());
        cfg = valid.clone();
        cfg.benchmark_only = true;
        assert!(cfg.validate().is_err());
        let file =
            std::env::temp_dir().join(format!("invalid-regime-config-{}.toml", std::process::id()));
        let source = std::fs::read_to_string(path)
            .unwrap()
            .replace("[portfolio]", "[portfolio]\nranker_btc_regime = \"true\"");
        std::fs::write(&file, source).unwrap();
        assert!(Config::load(&file)
            .unwrap_err()
            .contains("ranker_btc_regime must be a boolean"));
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn native_funding_is_opt_in_and_excludes_other_funding_and_blend_policies() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config/default.toml");
        let mut cfg = Config::load(&path).unwrap();
        assert!(!cfg.native_funding_adjustment);
        cfg.signal = "ml_ranker".into();
        cfg.native_funding_adjustment = true;
        assert!(cfg.validate().is_ok());
        cfg.funding_cost_adjustment = true;
        assert!(cfg.validate().is_err());
        cfg.funding_cost_adjustment = false;
        cfg.ranker_btc_momentum_fraction = Some(Decimal::ZERO);
        assert!(cfg.validate().is_err());
        cfg.ranker_btc_momentum_fraction = None;
        cfg.signal = "tsmom_ensemble".into();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn ranker_btc_blend_is_explicit_and_rejects_incompatible_policies() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config/default.toml");
        let mut cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.ranker_btc_momentum_fraction, None);
        cfg.signal = "ml_ranker".into();
        cfg.constructor = "risk_adjusted_score_selected".into();
        cfg.benchmark = Some("BTC".into());
        for fraction in [Decimal::ZERO, Decimal::new(5, 1), Decimal::ONE] {
            cfg.ranker_btc_momentum_fraction = Some(fraction);
            assert!(cfg.validate().is_ok());
        }
        for fraction in [-Decimal::ONE, Decimal::new(11, 1)] {
            cfg.ranker_btc_momentum_fraction = Some(fraction);
            assert!(cfg.validate().is_err());
        }
        cfg.ranker_btc_momentum_fraction = Some(Decimal::new(5, 1));
        cfg.signal = "tsmom_ensemble".into();
        assert!(cfg.validate().is_err());
        cfg.signal = "ml_ranker".into();
        cfg.constructor = "risk_adjusted_unbalanced".into();
        assert!(cfg.validate().is_err());
        cfg.constructor = "risk_adjusted_score_selected".into();
        cfg.ranker_allocation = RankerAllocation::Breadth;
        assert!(cfg.validate().is_err());
        cfg.ranker_allocation = RankerAllocation::Balanced;
        cfg.target_gross_exposure = Decimal::ONE;
        assert!(cfg.validate().is_err());
        cfg.target_gross_exposure = Decimal::new(8, 1);
        cfg.benchmark = Some("ETH".into());
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn benchmark_filter_cannot_change_the_ranker_training_cohort() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config/default.toml");
        let mut cfg = Config::load(&path).unwrap();
        assert!(!cfg.benchmark_only);
        cfg.benchmark_only = true;
        cfg.signal = "ml_ranker".into();
        assert!(cfg.validate().is_err());
        cfg.signal = "tsmom_ensemble".into();
        assert!(cfg.validate().is_ok());
        cfg.benchmark = None;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn ranker_policy_constraints_reject_incoherent_experiments() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config/default.toml");
        let mut cfg = Config::load(&path).unwrap();
        cfg.ranker_allocation = RankerAllocation::Breadth;
        assert!(cfg
            .validate()
            .unwrap_err()
            .contains("risk_adjusted_score_selected"));
        cfg.constructor = "risk_adjusted_score_selected".into();
        assert!(cfg.validate().is_ok());
        cfg.ranker_net_band = cfg.target_gross_exposure + Decimal::ONE;
        assert!(cfg.validate().is_err());
        cfg.ranker_net_band = Decimal::new(1, 1);
        cfg.ranker_selection = RankerSelection::PerSide;
        cfg.ranker_selection_count = 5;
        assert!(cfg.validate().unwrap_err().contains("even"));
        cfg.ranker_selection_count = 24;
        cfg.ranker_trend_weight = Decimal::new(11, 1);
        assert!(cfg.validate().is_err());
        cfg.ranker_trend_weight = Decimal::ZERO;
        cfg.ranker_allocation = RankerAllocation::Timing;
        assert!(cfg.validate().unwrap_err().contains("timing_model_path"));
    }

    #[test]
    fn funding_adjustment_requires_a_boolean_and_the_ranker() {
        let default = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config/default.toml");
        let source = std::fs::read_to_string(default).unwrap();
        let path =
            std::env::temp_dir().join(format!("crypto-funding-config-{}.toml", std::process::id()));
        for value in ["true", "false", "1", "\"true\""] {
            let config = source.replace(
                "[portfolio]",
                &format!("[portfolio]\nfunding_cost_adjustment = {value}"),
            );
            std::fs::write(&path, config).unwrap();
            let result = Config::load(&path);
            if value == "true" || value == "false" {
                assert_eq!(result.unwrap().funding_cost_adjustment, value == "true");
            } else {
                assert!(result.unwrap_err().contains("boolean"));
            }
        }
        let mut cfg = Config::load(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config/default.toml"),
        )
        .unwrap();
        cfg.funding_cost_adjustment = true;
        cfg.signal = "xs_momentum".into();
        assert!(cfg.validate().unwrap_err().contains("ml_ranker"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn ranker_selection_count_defaults_and_bounds_are_independent_of_risk_limit() {
        let default = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config/default.toml");
        let source = std::fs::read_to_string(&default).unwrap();
        let cfg = Config::load(&default).unwrap();
        assert_eq!(cfg.ranker_selection_count, 24);
        let path = std::env::temp_dir().join(format!(
            "crypto-ranker-count-config-{}.toml",
            std::process::id()
        ));
        for value in [
            "4", "24", "40", "1000", "0", "3", "1001", "-1", "4.5", "\"40\"", "true",
        ] {
            let config = source.replace(
                "[portfolio]",
                &format!("[portfolio]\nranker_selection_count = {value}"),
            );
            std::fs::write(&path, config).unwrap();
            let result = Config::load(&path);
            match value {
                "4" | "24" | "40" | "1000" => {
                    let parsed = result.unwrap();
                    assert_eq!(
                        parsed.ranker_selection_count,
                        value.parse::<usize>().unwrap()
                    );
                    assert_eq!(
                        parsed.limits.max_position_count,
                        cfg.limits.max_position_count
                    );
                    assert_eq!(parsed.max_holdings, cfg.max_holdings);
                    assert_eq!(parsed.selection_pool, cfg.selection_pool);
                }
                _ => assert!(result.unwrap_err().contains("ranker_selection_count")),
            }
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn loads_the_deployed_config() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config/default.toml");
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.bot_id, "crypto-portfolio");
        // The deployed strategy: the learned ranker under the risk-adjusted
        // construction. If this assertion surprises you, the deployment changed
        // and this test is doing its job.
        assert_eq!(cfg.signal, "ml_ranker");
        assert_eq!(cfg.constructor, "risk_adjusted");
        assert_eq!(cfg.costs.commission_bps, Decimal::from_str("4.5").unwrap());
    }
}
