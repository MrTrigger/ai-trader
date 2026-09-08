//! Declared research records with source fingerprints and comparable daily metrics.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    experiments: Vec<Experiment>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Development,
    Rejected,
    Invalid,
    Superseded,
    Frozen,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Experiment {
    id: String,
    family: String,
    status: Status,
    trial_count: Option<usize>,
    #[serde(default)]
    notes: Vec<String>,
    #[serde(default)]
    corrections: Vec<String>,
    #[serde(default)]
    supersedes: Vec<String>,
    fold_dir: Option<PathBuf>,
    expected_folds: Option<usize>,
    initial_cash: Option<String>,
    #[serde(default)]
    sources: Vec<PathBuf>,
}

#[derive(Deserialize)]
struct Replay {
    suite_fingerprint: Option<String>,
    source_config_path: Option<PathBuf>,
    source_config_sha256: Option<String>,
    scenario_id: Option<String>,
    initial_cash: Option<String>,
    schema_version: Option<String>,
    #[serde(default)]
    execution: Value,
    #[serde(default)]
    slippage_multiple: Value,
    steps: Vec<Step>,
    #[serde(default)]
    metadata: Value,
    #[serde(default)]
    metrics: Value,
}

#[derive(Deserialize)]
struct Step {
    as_of: DateTime<Utc>,
    #[serde(with = "rust_decimal::serde::str")]
    nav: Decimal,
    attribution: Option<BTreeMap<String, String>>,
}

#[derive(Deserialize)]
struct SuiteInputs {
    fingerprint: String,
    files: BTreeMap<String, String>,
    manifest_path: PathBuf,
    manifest: Value,
}

#[derive(Deserialize)]
struct SuiteManifest {
    source_dir: PathBuf,
    data_root: PathBuf,
    out_dir: PathBuf,
    folds: Vec<SuiteFold>,
    scenarios: Vec<SuiteScenario>,
}

#[derive(Deserialize)]
struct SuiteFold {
    id: usize,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
}

#[derive(Deserialize)]
struct SuiteScenario {
    id: String,
    config_dir: Option<PathBuf>,
    initial_cash: String,
    execution: Value,
}

struct VerifiedInputs {
    inputs: SuiteInputs,
    manifest: SuiteManifest,
    hashes: BTreeMap<String, String>,
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn load_suite_inputs(path: &Path, root: &Path) -> Result<VerifiedInputs, String> {
    let mut hashes = BTreeMap::new();
    let bytes = fingerprint(path, &mut hashes)?;
    let inputs: SuiteInputs =
        serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let computed = hash(&serde_json::to_vec(&inputs.files).map_err(|e| e.to_string())?);
    if inputs.fingerprint != computed {
        return Err("suite input inventory fingerprint mismatch".into());
    }
    let manifest_bytes = fingerprint(&root.join(&inputs.manifest_path), &mut hashes)?;
    if inputs
        .files
        .get(&inputs.manifest_path.display().to_string())
        != Some(&hash(&manifest_bytes))
    {
        return Err("suite manifest hash is missing or changed".into());
    }
    let source_manifest: Value =
        serde_json::from_slice(&manifest_bytes).map_err(|e| e.to_string())?;
    if source_manifest != inputs.manifest {
        return Err("embedded suite manifest differs from the fingerprinted source".into());
    }
    let manifest: SuiteManifest =
        serde_json::from_value(inputs.manifest.clone()).map_err(|e| e.to_string())?;
    if !inputs.files.contains_key("executable")
        || !inputs
            .files
            .keys()
            .any(|p| Path::new(p).starts_with(manifest.data_root.join("bars")))
    {
        return Err("suite inventory lacks executable or bar-data provenance".into());
    }
    Ok(VerifiedInputs {
        inputs,
        manifest,
        hashes,
    })
}

struct SuiteContext<'a> {
    root: &'a Path,
    directory: &'a Path,
    fold_id: usize,
    fold_count: usize,
    interval_s: i64,
    config_bytes: &'a [u8],
    config: &'a toml::Value,
    report_bytes: &'a [u8],
}

fn verify_suite_fold(
    replay: &Replay,
    context: &SuiteContext<'_>,
    verified: &VerifiedInputs,
    files: &mut BTreeMap<String, String>,
) -> Result<Value, String> {
    let inputs = &verified.inputs;
    let manifest = &verified.manifest;
    files.extend(verified.hashes.clone());
    if replay.suite_fingerprint.as_deref() != Some(&inputs.fingerprint) {
        return Err("fold suite fingerprint differs from inputs.json".into());
    }
    if replay.schema_version.as_deref() != Some("rust-backtest-2") {
        return Err("suite fold requires rust-backtest-2 schema".into());
    }
    let scenario_id = context
        .directory
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("invalid scenario directory")?;
    let scenarios: Vec<_> = manifest
        .scenarios
        .iter()
        .filter(|s| s.id == scenario_id)
        .collect();
    if replay.scenario_id.as_deref() != Some(scenario_id) || scenarios.len() != 1 {
        return Err("fold scenario does not match its directory and manifest".into());
    }
    let scenario = scenarios[0];
    if root_path(context.root, &manifest.out_dir.join(scenario_id))?
        != root_path(context.root, context.directory)?
    {
        return Err("fold directory differs from the declared suite output".into());
    }
    if replay.execution != scenario.execution
        || replay.initial_cash.as_deref().map(decimal).transpose()?
            != Some(decimal(&scenario.initial_cash)?)
        || replay.slippage_multiple.as_str().map(decimal).transpose()? != Some(Decimal::ONE)
    {
        return Err("fold execution or initial cash differs from its scenario".into());
    }
    let expected_config = scenario
        .config_dir
        .as_ref()
        .unwrap_or(&manifest.source_dir)
        .join(format!("config-{}.toml", context.fold_id));
    let copied_hash = hash(context.config_bytes);
    if replay.source_config_path.as_ref() != Some(&expected_config)
        || replay.source_config_sha256.as_deref() != Some(&copied_hash)
        || inputs.files.get(&expected_config.display().to_string()) != Some(&copied_hash)
    {
        return Err("copied config does not match suite source config provenance".into());
    }
    for key in [
        "model_path",
        "timing_model_path",
        "market_matrix_path",
        "liquidity_profile",
    ] {
        if let Some(path) = context
            .config
            .get("portfolio")
            .and_then(|p| p.get(key))
            .and_then(toml::Value::as_str)
            .filter(|p| !p.is_empty())
        {
            let actual = files.get(&context.root.join(path).display().to_string());
            if actual.is_none() || actual != inputs.files.get(path) {
                return Err(format!(
                    "configured {key} differs from suite input provenance"
                ));
            }
        }
    }
    let ids: BTreeSet<_> = manifest.folds.iter().map(|f| f.id).collect();
    if manifest.folds.len() != context.fold_count || ids != (1..=context.fold_count).collect() {
        return Err("suite fold declarations differ from expected folds".into());
    }
    let declared = manifest
        .folds
        .iter()
        .find(|f| f.id == context.fold_id)
        .ok_or("missing suite fold declaration")?;
    let duration = (declared.end - declared.start).num_seconds();
    if context.interval_s <= 0
        || duration < 0
        || duration % context.interval_s != 0
        || replay.steps.len() as i64 != duration / context.interval_s + 1
        || replay.steps.first().map(|s| s.as_of) != Some(declared.start)
        || replay.steps.last().map(|s| s.as_of) != Some(declared.end)
        || replay.steps.iter().any(|s| s.attribution.is_none())
    {
        return Err("suite fold has incomplete declared window or attribution".into());
    }
    let marker = fingerprint(
        &context
            .directory
            .join(format!("fold-{}.sha256", context.fold_id)),
        files,
    )?;
    if std::str::from_utf8(&marker)
        .map_err(|e| e.to_string())?
        .trim()
        != hash(context.report_bytes)
    {
        return Err("suite fold completion hash mismatch".into());
    }
    Ok(
        json!({"status":"verified_suite_linkage", "suite_fingerprint":inputs.fingerprint,
        "scenario_id":scenario_id, "source_config_path":expected_config,
        "data_inventory_rehashed":false,
        "data_inventory_note":"The saved inventory fingerprint is verified; raw data and the historical executable are not rehashed during registration."}),
    )
}

fn root_path(root: &Path, path: &Path) -> Result<PathBuf, String> {
    root.join(path)
        .canonicalize()
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn fingerprint(path: &Path, files: &mut BTreeMap<String, String>) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    files.insert(
        path.display().to_string(),
        format!("{:x}", Sha256::digest(&bytes)),
    );
    Ok(bytes)
}

fn decimal(value: &str) -> Result<Decimal, String> {
    value
        .parse()
        .map_err(|e| format!("invalid decimal {value:?}: {e}"))
}

fn sharpe(returns: &[f64]) -> Option<f64> {
    if returns.len() < 2 {
        return None;
    }
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance =
        returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (returns.len() - 1) as f64;
    (variance > 0.0).then(|| mean / variance.sqrt() * 365.0_f64.sqrt())
}

const ATTRIBUTION: [&str; 9] = [
    "long_price_pnl",
    "short_price_pnl",
    "funding_pnl",
    "commission_cost",
    "spread_cost",
    "impact_cost",
    "delay_pnl",
    "unfilled_notional",
    "reconciliation_error",
];

fn fold(
    replay: Replay,
    initial_cash: Decimal,
    interval_s: i64,
) -> Result<(Value, BTreeMap<NaiveDate, f64>), String> {
    if replay.steps.is_empty() || initial_cash <= Decimal::ZERO {
        return Err("empty replay or nonpositive initial cash".into());
    }
    if interval_s <= 0 || 86_400 % interval_s != 0 {
        return Err("register supports intervals that divide one day".into());
    }
    let mut previous_nav = initial_cash;
    let mut previous_stamp: Option<DateTime<Utc>> = None;
    let mut daily = BTreeMap::new();
    let mut sums: BTreeMap<&str, Decimal> = ATTRIBUTION
        .into_iter()
        .map(|k| (k, Decimal::ZERO))
        .collect();
    let mut attributed_steps = 0;
    let mut max_reconciliation = Decimal::ZERO;
    for step in &replay.steps {
        if previous_stamp.is_some_and(|stamp| (step.as_of - stamp).num_seconds() != interval_s) {
            return Err("replay has an incomplete or unordered decision grid".into());
        }
        if step.nav <= Decimal::ZERO {
            return Err("nonpositive NAV cannot form a complete return series".into());
        }
        let gross = (step.nav / previous_nav)
            .to_f64()
            .ok_or("NAV ratio overflow")?;
        *daily.entry(step.as_of.date_naive()).or_insert(1.0) *= gross;
        if let Some(attribution) = &step.attribution {
            attributed_steps += 1;
            let mut values = BTreeMap::new();
            for field in ATTRIBUTION {
                let value = decimal(
                    attribution
                        .get(field)
                        .ok_or_else(|| format!("attribution missing {field}"))?,
                )?;
                *sums.get_mut(field).unwrap() += value;
                values.insert(field, value);
            }
            let explained = values["long_price_pnl"]
                + values["short_price_pnl"]
                + values["funding_pnl"]
                + values["delay_pnl"]
                - values["commission_cost"]
                - values["spread_cost"]
                - values["impact_cost"];
            let error = step.nav - previous_nav - explained;
            let tolerance = Decimal::new(1, 6);
            if (error - values["reconciliation_error"]).abs() > tolerance {
                return Err(format!(
                    "{} attribution does not reconcile with NAV",
                    step.as_of
                ));
            }
            max_reconciliation = max_reconciliation.max(error.abs());
        }
        previous_nav = step.nav;
        previous_stamp = Some(step.as_of);
    }
    for gross in daily.values_mut() {
        *gross -= 1.0;
    }
    let attribution = if attributed_steps == 0 {
        json!({"status": "unavailable", "reason": "saved replay has no attribution"})
    } else if attributed_steps != replay.steps.len() {
        return Err("attribution only covers part of the replay".into());
    } else {
        json!({"status": "available", "totals": sums.iter().map(|(k,v)| (*k,v.to_string())).collect::<BTreeMap<_,_>>(),
            "max_absolute_reconciliation_error": max_reconciliation.to_string(),
            "reconciled": max_reconciliation <= Decimal::new(1,6)})
    };
    let daily_values: Vec<_> = daily.values().copied().collect();
    Ok((
        json!({
            "start": replay.steps[0].as_of,
            "end": replay.steps.last().unwrap().as_of,
            "steps": replay.steps.len(), "days": daily.len(),
        "initial_cash": initial_cash.to_string(),
        "replay_metadata": replay.metadata,
        "replay_schema_version": replay.schema_version,
        "execution": replay.execution,
        "slippage_multiple": replay.slippage_multiple,
            "daily_sharpe": sharpe(&daily_values),
            "saved_sharpe": replay.metrics.get("sharpe"),
            "total_return": (previous_nav / initial_cash - Decimal::ONE).to_string(),
            "attribution": attribution,
        }),
        daily,
    ))
}

fn build(manifest: Manifest, root: &Path) -> Result<Value, String> {
    let mut ids = BTreeSet::new();
    for experiment in &manifest.experiments {
        if experiment.id.trim().is_empty() || !ids.insert(experiment.id.clone()) {
            return Err(format!(
                "empty or duplicate experiment id {:?}",
                experiment.id
            ));
        }
        if experiment.trial_count == Some(0) {
            return Err(format!(
                "{}: trial_count must be positive or null",
                experiment.id
            ));
        }
    }
    for experiment in &manifest.experiments {
        for id in &experiment.supersedes {
            if !ids.contains(id) || id == &experiment.id {
                return Err(format!(
                    "{}: invalid supersedes reference {id}",
                    experiment.id
                ));
            }
        }
    }
    let mut records = Vec::new();
    let mut dates_by_experiment = Vec::new();
    let mut suite_inputs = BTreeMap::new();
    for experiment in manifest.experiments {
        let mut files = BTreeMap::new();
        for source in &experiment.sources {
            fingerprint(&root.join(source), &mut files)?;
        }
        let mut folds = Vec::new();
        let mut pooled = BTreeMap::new();
        if let Some(directory) = &experiment.fold_dir {
            let count = experiment
                .expected_folds
                .filter(|n| *n > 0)
                .ok_or_else(|| format!("{} requires positive expected_folds", experiment.id))?;
            let directory = root.join(directory);
            let actual: BTreeSet<_> = std::fs::read_dir(&directory)
                .map_err(|e| format!("{}: {e}", directory.display()))?
                .map(|entry| {
                    entry
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .map_err(|e| e.to_string())
                })
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|name| name.starts_with("fold-") && name.ends_with(".json"))
                .collect();
            let expected: BTreeSet<_> = (1..=count).map(|i| format!("fold-{i}.json")).collect();
            if actual != expected {
                return Err(format!(
                    "{}: incomplete or unexpected folds; expected {expected:?}, found {actual:?}",
                    experiment.id
                ));
            }
            let inputs_path = directory
                .parent()
                .ok_or("fold directory has no parent")?
                .join("inputs.json");
            if inputs_path.exists() && !suite_inputs.contains_key(&inputs_path) {
                suite_inputs.insert(inputs_path.clone(), load_suite_inputs(&inputs_path, root)?);
            }
            for i in 1..=count {
                let bytes = fingerprint(&directory.join(format!("fold-{i}.json")), &mut files)?;
                let replay: Replay = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("{} fold {i}: {e}", experiment.id))?;
                let config_bytes =
                    fingerprint(&directory.join(format!("config-{i}.toml")), &mut files)?;
                let config: toml::Value = std::str::from_utf8(&config_bytes)
                    .map_err(|e| e.to_string())?
                    .parse::<toml::Value>()
                    .map_err(|e| e.to_string())?;
                for key in [
                    "model_path",
                    "timing_model_path",
                    "market_matrix_path",
                    "liquidity_profile",
                ] {
                    if let Some(path) = config
                        .get("portfolio")
                        .and_then(|p| p.get(key))
                        .and_then(toml::Value::as_str)
                        .filter(|p| !p.is_empty())
                    {
                        fingerprint(&root.join(path), &mut files)?;
                    }
                }
                let interval = config
                    .get("portfolio")
                    .and_then(|p| p.get("interval_s"))
                    .and_then(toml::Value::as_integer)
                    .ok_or("config is missing portfolio.interval_s")?;
                let metadata_cash = replay
                    .initial_cash
                    .as_deref()
                    .or_else(|| replay.metadata.get("initial_cash").and_then(Value::as_str));
                if let (Some(saved), Some(declared)) =
                    (metadata_cash, experiment.initial_cash.as_deref())
                {
                    if decimal(saved)? != decimal(declared)? {
                        return Err(format!(
                            "{} fold {i}: initial cash disagrees with metadata",
                            experiment.id
                        ));
                    }
                }
                let initial_cash = decimal(
                    metadata_cash
                        .or(experiment.initial_cash.as_deref())
                        .ok_or("initial_cash is required in metadata or manifest")?,
                )?;
                let provenance = if let Some(inputs) = suite_inputs.get(&inputs_path) {
                    verify_suite_fold(
                        &replay,
                        &SuiteContext {
                            root,
                            directory: &directory,
                            fold_id: i,
                            fold_count: count,
                            interval_s: interval,
                            config_bytes: &config_bytes,
                            config: &config,
                            report_bytes: &bytes,
                        },
                        inputs,
                        &mut files,
                    )
                    .map_err(|e| format!("{} fold {i}: {e}", experiment.id))?
                } else if replay.suite_fingerprint.is_some()
                    || replay.scenario_id.is_some()
                    || replay.source_config_path.is_some()
                    || replay.source_config_sha256.is_some()
                {
                    return Err(format!(
                        "{}: suite fold is missing inputs.json",
                        experiment.id
                    ));
                } else {
                    json!({"status":"unavailable", "reason":"legacy replay has no suite input provenance"})
                };
                let (mut report, daily) = fold(replay, initial_cash, interval)
                    .map_err(|e| format!("{} fold {i}: {e}", experiment.id))?;
                report["fold"] = json!(i);
                report["provenance"] = provenance;
                for (date, value) in daily {
                    if pooled.insert(date, value).is_some() {
                        return Err(format!("{}: overlapping fold date {date}", experiment.id));
                    }
                }
                folds.push(report);
            }
        } else if experiment.expected_folds.is_some() {
            return Err(format!(
                "{} has expected_folds but no fold_dir",
                experiment.id
            ));
        }
        let fold_sharpes: Vec<_> = folds
            .iter()
            .filter_map(|f| f["daily_sharpe"].as_f64())
            .collect();
        let saved_sharpes: Vec<_> = folds
            .iter()
            .filter_map(|f| f["saved_sharpe"].as_f64())
            .collect();
        let average = |values: &[f64]| {
            (!values.is_empty() && values.len() == folds.len())
                .then(|| values.iter().sum::<f64>() / values.len() as f64)
        };
        let values: Vec<_> = pooled.values().copied().collect();
        dates_by_experiment.push((
            experiment.id.clone(),
            pooled.keys().copied().collect::<BTreeSet<_>>(),
        ));
        records.push(json!({"experiment": experiment, "sha256": files, "folds": folds,
            "pooled_daily_sharpe": sharpe(&values), "mean_fold_daily_sharpe": average(&fold_sharpes),
            "mean_saved_fold_sharpe": average(&saved_sharpes), "pooled_days": pooled.len(),
            "start": pooled.keys().next(), "end": pooled.keys().next_back()}));
    }
    let mut reused = Vec::new();
    for (i, (left, dates)) in dates_by_experiment.iter().enumerate() {
        for (right, other) in &dates_by_experiment[i + 1..] {
            let common: Vec<_> = dates.intersection(other).copied().collect();
            if !common.is_empty() {
                reused.push(json!({"experiments": [left, right], "overlapping_days": common.len(),
                    "start": common.first(), "end": common.last(), "status": "reused_evaluation_dates"}));
            }
        }
    }
    Ok(json!({"schema_version": 1, "annualization": 365,
        "method": "Compound decision returns by UTC date; include first step versus initial cash; pool nonoverlapping fold days; sample standard deviation; zero-variance Sharpe is null.",
        "limitations": ["Statuses and trial counts are declarations, never inferred promotion.",
            "Hashes identify supplied artifacts, not an unavailable historical code commit or data snapshot.",
            "Shared dates identify reused evidence, not independent validation.",
            "This curated register is not an exhaustive count of all searches."],
        "experiments": records, "reused_evidence": reused}))
}

pub fn command(args: &[String]) -> Result<(), String> {
    let mut manifest_path = None;
    let mut out_path = None;
    let mut pairs = args.chunks_exact(2);
    for pair in &mut pairs {
        let target = match pair[0].as_str() {
            "--manifest" => &mut manifest_path,
            "--out" => &mut out_path,
            other => return Err(format!("unknown experiment-register option {other}")),
        };
        if target.replace(PathBuf::from(&pair[1])).is_some() {
            return Err(format!("duplicate option {}", pair[0]));
        }
    }
    if !pairs.remainder().is_empty() {
        return Err("experiment-register options require values".into());
    }
    let manifest_path = manifest_path.ok_or("--manifest is required")?;
    let out_path = out_path.ok_or("--out is required")?;
    let bytes =
        std::fs::read(&manifest_path).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let mut report = build(
        manifest,
        &std::env::current_dir().map_err(|e| e.to_string())?,
    )?;
    report["manifest_sha256"] = json!(format!("{:x}", Sha256::digest(&bytes)));
    if out_path == manifest_path {
        return Err("output must differ from manifest".into());
    }
    std::fs::write(
        &out_path,
        serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("{}: {e}", out_path.display()))?;
    println!("wrote {}", out_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "experiment-register-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(root.join("folds")).unwrap();
            std::fs::write(
                root.join("folds/config-1.toml"),
                "[portfolio]\ninterval_s = 86400\nmodel_path = 'model.json'\n",
            )
            .unwrap();
            std::fs::write(root.join("model.json"), "model v1").unwrap();
            Self(root)
        }
        fn save(&self, navs: &[&str]) {
            std::fs::write(
                self.0.join("folds/fold-1.json"),
                serde_json::to_vec(&replay_value(navs)).unwrap(),
            )
            .unwrap();
        }
        fn manifest(&self) -> Manifest {
            serde_json::from_value(json!({"experiments": [{"id":"a", "family":"ranker", "status":"development", "trial_count":null,
                "fold_dir":"folds", "expected_folds":1, "initial_cash":"100"}]})).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn replay_value(navs: &[&str]) -> Value {
        json!({"steps": navs.iter().enumerate().map(|(i,n)| json!({
            "as_of":format!("2025-01-{:02}T00:00:00Z",i+1),"nav":n})).collect::<Vec<_>>(),
            "metrics":{"sharpe":9.0}})
    }
    #[test]
    fn first_step_is_included_and_pooled_is_distinct_from_saved_mean() {
        let f = Fixture::new();
        f.save(&["90", "100", "101"]);
        let result = build(f.manifest(), &f.0).unwrap();
        let e = &result["experiments"][0];
        assert_eq!(e["pooled_days"], 3);
        assert_eq!(e["mean_saved_fold_sharpe"], 9.0);
        let expected = sharpe(&[-0.1, 100.0 / 90.0 - 1.0, 0.01]).unwrap();
        assert!((e["pooled_daily_sharpe"].as_f64().unwrap() - expected).abs() < 1e-10);
        assert_eq!(e["folds"][0]["attribution"]["status"], "unavailable");
    }
    #[test]
    fn rejects_duplicate_ids_missing_models_and_incomplete_folds() {
        let f = Fixture::new();
        f.save(&["100", "101"]);
        let mut m = f.manifest();
        m.experiments.push(f.manifest().experiments.remove(0));
        assert!(build(m, &f.0).unwrap_err().contains("duplicate"));
        let mut m = f.manifest();
        m.experiments[0].expected_folds = Some(2);
        assert!(build(m, &f.0).unwrap_err().contains("incomplete"));
        std::fs::remove_file(f.0.join("model.json")).unwrap();
        assert!(build(f.manifest(), &f.0)
            .unwrap_err()
            .contains("model.json"));
    }
    #[test]
    fn source_mutations_change_hashes_and_shared_dates_are_reused() {
        let f = Fixture::new();
        f.save(&["100", "101", "99"]);
        let before = build(f.manifest(), &f.0).unwrap();
        std::fs::write(f.0.join("model.json"), "model v2").unwrap();
        let mut m = f.manifest();
        let mut second = f.manifest().experiments.remove(0);
        second.id = "b".into();
        m.experiments.push(second);
        let after = build(m, &f.0).unwrap();
        assert_ne!(
            before["experiments"][0]["sha256"],
            after["experiments"][0]["sha256"]
        );
        assert_eq!(after["reused_evidence"][0]["overlapping_days"], 3);
        assert_eq!(
            after["experiments"][0]["experiment"]["status"],
            "development"
        );
    }
    #[test]
    fn rejects_bad_attribution_and_checks_real_nav_delta() {
        let mut value = replay_value(&["101", "102"]);
        let mut a: BTreeMap<_, _> = ATTRIBUTION.into_iter().map(|k| (k, "0")).collect();
        a.insert("long_price_pnl", "1");
        for step in value["steps"].as_array_mut().unwrap() {
            step["attribution"] = json!(a);
        }
        let replay = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            fold(replay, Decimal::from(100), 86400).unwrap().0["attribution"]["reconciled"],
            true
        );
        value["steps"][1]["attribution"]["commission_cost"] = json!("1");
        assert!(fold(
            serde_json::from_value(value).unwrap(),
            Decimal::from(100),
            86400
        )
        .unwrap_err()
        .contains("reconcile"));
    }
    #[test]
    fn rejects_grid_gaps_and_missing_cash() {
        let mut value = replay_value(&["100", "101"]);
        value["steps"][1]["as_of"] = json!("2025-01-03T00:00:00Z");
        assert!(fold(
            serde_json::from_value(value).unwrap(),
            Decimal::from(100),
            86400
        )
        .unwrap_err()
        .contains("grid"));
        let f = Fixture::new();
        f.save(&["100", "101"]);
        let mut m = f.manifest();
        m.experiments[0].initial_cash = None;
        assert!(build(m, &f.0).unwrap_err().contains("initial_cash"));
    }

    #[test]
    fn pooled_sharpe_is_not_the_mean_of_fold_sharpes() {
        let f = Fixture::new();
        f.save(&["110", "108", "115"]);
        let mut second = replay_value(&["90", "95", "92"]);
        for (i, step) in second["steps"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            step["as_of"] = json!(format!("2025-01-{:02}T00:00:00Z", i + 4));
        }
        std::fs::write(
            f.0.join("folds/fold-2.json"),
            serde_json::to_vec(&second).unwrap(),
        )
        .unwrap();
        std::fs::copy(
            f.0.join("folds/config-1.toml"),
            f.0.join("folds/config-2.toml"),
        )
        .unwrap();
        let mut m = f.manifest();
        m.experiments[0].expected_folds = Some(2);
        let report = build(m, &f.0).unwrap();
        let e = &report["experiments"][0];
        let a = e["pooled_daily_sharpe"].as_f64().unwrap();
        let b = e["mean_fold_daily_sharpe"].as_f64().unwrap();
        assert!((a - b).abs() > 0.1);
    }

    #[test]
    fn rejects_partial_attribution_and_conflicting_initial_cash() {
        let mut value = replay_value(&["100", "101"]);
        value["steps"][0]["attribution"] = json!(ATTRIBUTION
            .into_iter()
            .map(|k| (k, "0"))
            .collect::<BTreeMap<_, _>>());
        assert!(fold(
            serde_json::from_value(value).unwrap(),
            Decimal::from(100),
            86400
        )
        .unwrap_err()
        .contains("part"));
        let f = Fixture::new();
        let mut value = replay_value(&["100", "101"]);
        value["initial_cash"] = json!("200");
        std::fs::write(
            f.0.join("folds/fold-1.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        assert!(build(f.manifest(), &f.0).unwrap_err().contains("disagrees"));
    }

    impl Fixture {
        fn suite() -> Self {
            Self::suite_with_config_dir(None)
        }

        fn suite_with_config_dir(config_dir: Option<&str>) -> Self {
            let f = Self::new();
            let execution = json!({"model":"flat", "delay_hours":0});
            let manifest = json!({"source_dir":"source", "data_root":"data", "out_dir":".",
                "folds":[{"id":1,"start":"2025-01-01T00:00:00Z","end":"2025-01-02T00:00:00Z"}],
                "scenarios":[{"id":"folds","config_dir":config_dir,"initial_cash":"100","execution":execution}]});
            let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
            std::fs::write(f.0.join("suite.json"), &manifest_bytes).unwrap();
            let config_hash = hash(&std::fs::read(f.0.join("folds/config-1.toml")).unwrap());
            let source_config = format!("{}/config-1.toml", config_dir.unwrap_or("source"));
            let files = BTreeMap::from([
                (source_config.clone(), config_hash.clone()),
                ("model.json".to_string(), hash(b"model v1")),
                ("data/bars/example.parquet".to_string(), hash(b"data")),
                ("executable".to_string(), hash(b"binary")),
                ("suite.json".to_string(), hash(&manifest_bytes)),
            ]);
            let fingerprint = hash(&serde_json::to_vec(&files).unwrap());
            std::fs::write(f.0.join("inputs.json"), serde_json::to_vec(&json!({
                "fingerprint":fingerprint,"files":files,"manifest_path":"suite.json","manifest":manifest})).unwrap()).unwrap();
            let mut replay = replay_value(&["100", "101"]);
            replay["suite_fingerprint"] = json!(fingerprint);
            replay["source_config_path"] = json!(source_config);
            replay["source_config_sha256"] = json!(config_hash);
            replay["scenario_id"] = json!("folds");
            replay["schema_version"] = json!("rust-backtest-2");
            replay["initial_cash"] = json!("100");
            replay["execution"] = execution;
            replay["slippage_multiple"] = json!("1");
            for (i, step) in replay["steps"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .enumerate()
            {
                let mut a: BTreeMap<_, _> = ATTRIBUTION.into_iter().map(|k| (k, "0")).collect();
                a.insert("long_price_pnl", if i == 0 { "0" } else { "1" });
                step["attribution"] = json!(a);
            }
            f.save_suite_replay(&replay);
            f
        }

        fn save_suite_replay(&self, replay: &Value) {
            let bytes = serde_json::to_vec(replay).unwrap();
            std::fs::write(self.0.join("folds/fold-1.sha256"), hash(&bytes)).unwrap();
            std::fs::write(self.0.join("folds/fold-1.json"), bytes).unwrap();
        }

        fn read_json(&self, path: &str) -> Value {
            serde_json::from_slice(&std::fs::read(self.0.join(path)).unwrap()).unwrap()
        }
    }

    #[test]
    fn suite_provenance_verifies_config_model_manifest_and_completed_report() {
        let f = Fixture::suite();
        let report = build(f.manifest(), &f.0).unwrap();
        assert_eq!(
            report["experiments"][0]["folds"][0]["provenance"]["status"],
            "verified_suite_linkage"
        );
        assert_eq!(
            report["experiments"][0]["folds"][0]["provenance"]["data_inventory_rehashed"],
            false
        );
        std::fs::write(f.0.join("model.json"), b"changed model").unwrap();
        assert!(build(f.manifest(), &f.0)
            .unwrap_err()
            .contains("model_path"));
    }

    #[test]
    fn suite_verifies_scenario_specific_config_provenance() {
        let f = Fixture::suite_with_config_dir(Some("variant"));
        let report = build(f.manifest(), &f.0).unwrap();
        assert_eq!(
            report["experiments"][0]["folds"][0]["provenance"]["source_config_path"],
            "variant/config-1.toml"
        );
    }

    #[test]
    fn suite_rejects_wrong_scenario_metadata_and_incomplete_window() {
        for (field, value, message) in [
            ("scenario_id", json!("other"), "scenario"),
            ("suite_fingerprint", json!("wrong"), "fingerprint"),
            ("source_config_sha256", json!("wrong"), "config"),
            ("schema_version", json!("old"), "schema"),
            ("execution", json!({"model":"impact"}), "execution"),
        ] {
            let f = Fixture::suite();
            let mut replay = f.read_json("folds/fold-1.json");
            replay[field] = value;
            f.save_suite_replay(&replay);
            assert!(
                build(f.manifest(), &f.0).unwrap_err().contains(message),
                "{field}"
            );
        }
        let f = Fixture::suite();
        let mut replay = f.read_json("folds/fold-1.json");
        replay["steps"].as_array_mut().unwrap().pop();
        f.save_suite_replay(&replay);
        assert!(build(f.manifest(), &f.0).unwrap_err().contains("window"));
    }

    #[test]
    fn suite_rejects_inventory_and_embedded_manifest_corruption() {
        let f = Fixture::suite();
        let mut inputs = f.read_json("inputs.json");
        inputs["files"]["data/bars/example.parquet"] = json!("changed data hash");
        std::fs::write(
            f.0.join("inputs.json"),
            serde_json::to_vec(&inputs).unwrap(),
        )
        .unwrap();
        assert!(build(f.manifest(), &f.0)
            .unwrap_err()
            .contains("inventory fingerprint"));
        let f = Fixture::suite();
        let mut inputs = f.read_json("inputs.json");
        inputs["manifest"]["scenarios"][0]["execution"]["model"] = json!("impact");
        std::fs::write(
            f.0.join("inputs.json"),
            serde_json::to_vec(&inputs).unwrap(),
        )
        .unwrap();
        assert!(build(f.manifest(), &f.0).unwrap_err().contains("embedded"));
    }

    #[test]
    fn suite_rejects_missing_or_stale_completion_markers_and_copied_configs() {
        let f = Fixture::suite();
        std::fs::remove_file(f.0.join("folds/fold-1.sha256")).unwrap();
        assert!(build(f.manifest(), &f.0).unwrap_err().contains("sha256"));
        let f = Fixture::suite();
        std::fs::write(f.0.join("folds/fold-1.sha256"), "wrong").unwrap();
        assert!(build(f.manifest(), &f.0)
            .unwrap_err()
            .contains("completion hash"));
        let f = Fixture::suite();
        let mut config = std::fs::read_to_string(f.0.join("folds/config-1.toml")).unwrap();
        config.push_str("# changed\n");
        std::fs::write(f.0.join("folds/config-1.toml"), config).unwrap();
        assert!(build(f.manifest(), &f.0)
            .unwrap_err()
            .contains("copied config"));
    }
}
