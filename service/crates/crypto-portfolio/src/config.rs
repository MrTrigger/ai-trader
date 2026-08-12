use std::collections::BTreeMap;
use std::path::Path;
use std::str::FromStr;

use rust_decimal::Decimal;
use toml::Value;

#[derive(Debug, Clone)]
pub struct RiskLimits {
    /// Fraction of one hour's volume a single position may represent. The cap
    /// is the LOWER of this and `max_position`, so a thin name is limited by
    /// the market and a liquid one by the mandate.
    pub participation_limit: Decimal,
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
    pub constructor: String,
    pub signal: String,
    pub max_holdings: usize,
    pub min_cross_section: usize,
    pub rebalance_every: usize,
    pub universe: Vec<String>,
    pub benchmark: Option<String>,
    pub min_dollar_volume: Decimal,
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
    pub model_path: Option<String>,
    pub limits: RiskLimits,
    pub clusters: BTreeMap<String, String>,
    pub costs: CostModel,
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
            min_dollar_volume: dec(p, "min_dollar_volume")?,
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
            model_path: p
                .get("model_path")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(str::to_owned),
            limits,
            clusters,
            costs,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), String> {
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
