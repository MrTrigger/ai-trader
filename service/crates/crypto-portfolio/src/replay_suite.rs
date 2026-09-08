//! Replay frozen folds once per declared execution scenario, sharing prepared data.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{backtest, config::Config, execution::Settings};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Suite {
    source_dir: PathBuf,
    data_root: PathBuf,
    out_dir: PathBuf,
    folds: Vec<Fold>,
    scenarios: Vec<Scenario>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fold {
    id: usize,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Scenario {
    id: String,
    config_dir: Option<PathBuf>,
    #[serde(with = "rust_decimal::serde::str")]
    initial_cash: Decimal,
    execution: Settings,
}

fn load_configs(
    suite: &Suite,
    scenario: &Scenario,
    files: &mut BTreeMap<String, String>,
) -> Result<Vec<Config>, String> {
    let source = scenario.config_dir.as_ref().unwrap_or(&suite.source_dir);
    suite
        .folds
        .iter()
        .map(|fold| {
            let path = source.join(format!("config-{}.toml", fold.id));
            let cfg = Config::load(&path)?;
            if cfg.interval_s != 86400 || cfg.rebalance_every != 1 {
                return Err("suite requires a daily strategy".into());
            }
            for artifact in [
                &cfg.model_path,
                &cfg.timing_model_path,
                &cfg.market_matrix_path,
            ]
            .into_iter()
            .flatten()
            {
                files.insert(artifact.clone(), hash_file(Path::new(artifact))?);
            }
            files.insert(path.to_string_lossy().into_owned(), hash_file(&path)?);
            Ok(cfg)
        })
        .collect()
}

fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn inventory(root: &Path, files: &mut BTreeMap<String, String>) -> Result<(), String> {
    for entry in std::fs::read_dir(root).map_err(|e| format!("{}: {e}", root.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            inventory(&path, files)?;
        } else if matches!(
            path.extension().and_then(|s| s.to_str()),
            Some("json" | "jsonl" | "parquet")
        ) {
            files.insert(path.to_string_lossy().into_owned(), hash_file(&path)?);
        }
    }
    Ok(())
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(tmp, path).map_err(|e| e.to_string())
}

fn validate_cached(
    value: &serde_json::Value,
    fold: &Fold,
    scenario: &Scenario,
    fingerprint: &str,
    config_hash: &str,
) -> Result<(), String> {
    if value["suite_fingerprint"] != fingerprint
        || value["schema_version"] != "rust-backtest-2"
        || value["source_config_sha256"] != config_hash
        || value["scenario_id"] != scenario.id
        || value["initial_cash"] != scenario.initial_cash.to_string()
        || value["execution"]
            != serde_json::to_value(&scenario.execution).map_err(|e| e.to_string())?
    {
        return Err("cached replay metadata does not match the declared scenario".into());
    }
    let steps = value["steps"]
        .as_array()
        .ok_or("cached replay has no steps")?;
    if steps.len() as i64 != (fold.end - fold.start).num_days() + 1 {
        return Err("cached replay is incomplete".into());
    }
    for (i, step) in steps.iter().enumerate() {
        let date: DateTime<Utc> = step["as_of"]
            .as_str()
            .ok_or("missing step timestamp")?
            .parse()
            .map_err(|e| format!("invalid step timestamp: {e}"))?;
        if date != fold.start + chrono::Duration::days(i as i64) || step["attribution"].is_null() {
            return Err("cached replay has a missing date or attribution".into());
        }
    }
    Ok(())
}

pub fn command(args: &[String]) -> Result<(), String> {
    if args.len() != 2 || args[0] != "--manifest" {
        return Err("usage: replay-suite --manifest <json>".into());
    }
    let path = args
        .windows(2)
        .find(|w| w[0] == "--manifest")
        .map(|w| PathBuf::from(&w[1]))
        .ok_or("--manifest is required")?;
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    let suite: Suite = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if suite.folds.is_empty() || suite.scenarios.is_empty() {
        return Err("suite needs folds and scenarios".into());
    }
    let mut previous = None;
    let mut ids = std::collections::BTreeSet::new();
    let mut files = BTreeMap::new();
    for fold in &suite.folds {
        if !ids.insert(fold.id)
            || fold.start > fold.end
            || previous.is_some_and(|end| fold.start != end + chrono::Duration::days(1))
        {
            return Err("folds must have unique IDs and ordered contiguous daily windows".into());
        }
        previous = Some(fold.end);
    }
    ids.clear();
    let mut scenario_ids = std::collections::BTreeSet::new();
    for scenario in &suite.scenarios {
        if scenario.id.is_empty()
            || !scenario
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
            || !scenario_ids.insert(&scenario.id)
            || scenario.initial_cash <= Decimal::ZERO
        {
            return Err("scenarios need unique safe IDs and positive initial cash".into());
        }
        scenario.execution.validate()?;
    }
    std::fs::create_dir_all(&suite.out_dir).map_err(|e| e.to_string())?;
    if suite.out_dir.canonicalize().map_err(|e| e.to_string())?
        == suite.source_dir.canonicalize().map_err(|e| e.to_string())?
    {
        return Err("suite output cannot replace source folds".into());
    }
    for scenario in &suite.scenarios {
        let source = scenario.config_dir.as_ref().unwrap_or(&suite.source_dir);
        if source
            .canonicalize()
            .map_err(|e| e.to_string())?
            .starts_with(suite.out_dir.canonicalize().map_err(|e| e.to_string())?)
        {
            return Err("suite source configurations cannot be inside its output directory".into());
        }
    }
    let configs: Vec<Vec<Config>> = suite
        .scenarios
        .iter()
        .map(|s| load_configs(&suite, s, &mut files))
        .collect::<Result<_, _>>()?;
    let base = &configs[0][0];
    if configs
        .iter()
        .flatten()
        .any(|cfg| cfg.benchmark != base.benchmark)
    {
        return Err("shared replay preparation requires matching benchmark".into());
    }
    eprintln!("replay-suite: hashing bars, funding, universes, models and executable");
    for name in ["bars", "funding", "universe"] {
        inventory(&suite.data_root.join(name), &mut files)?;
    }
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    files.insert("executable".into(), hash_file(&executable)?);
    files.insert(path.to_string_lossy().into_owned(), hash_file(&path)?);
    let fingerprint = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&files).map_err(|e| e.to_string())?)
    );
    let provenance_path = suite.out_dir.join("inputs.json");
    let provenance = serde_json::json!({"fingerprint": fingerprint, "files": files,
        "manifest_path": path, "manifest": serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|e| e.to_string())?});
    if provenance_path.exists() {
        let prior: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&provenance_path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if prior != provenance {
            return Err(
                "suite inputs changed; use a new output directory to preserve the prior evidence"
                    .into(),
            );
        }
    } else {
        write_json(&provenance_path, &provenance)?;
    }
    eprintln!("replay-suite: preparing shared causal features");
    let hourly = configs
        .iter()
        .flatten()
        .any(|cfg| cfg.signal == "ml_ranker")
        || suite.scenarios.iter().any(|s| s.execution.delay_hours > 0);
    let prepared = backtest::prepare(
        base,
        &suite.data_root,
        hourly,
        features_crypto::FundingWindow::Trailing,
    )?;
    std::thread::scope(|scope| {
        let handles: Vec<_> = suite
            .scenarios
            .iter()
            .zip(&configs)
            .map(|(scenario, configs)| {
                let suite = &suite;
                let prepared = &prepared;
                let fingerprint = &fingerprint;
                let files = &files;
                scope.spawn(move || -> Result<(), String> {
                    let result = (|| -> Result<(), String> {
                        let out = suite.out_dir.join(&scenario.id);
                        std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
                        for (fold, cfg) in suite.folds.iter().zip(configs) {
                            let target = out.join(format!("fold-{}.json", fold.id));
                            let checksum_path = target.with_extension("sha256");
                            let source_config = scenario
                                .config_dir
                                .as_ref()
                                .unwrap_or(&suite.source_dir)
                                .join(format!("config-{}.toml", fold.id));
                            let config_hash = &files[&source_config.to_string_lossy().into_owned()];
                            if target.exists() && checksum_path.exists() {
                                if std::fs::read_to_string(&checksum_path)
                                    .map_err(|e| e.to_string())?
                                    .trim()
                                    != hash_file(&target)?
                                {
                                    return Err(format!(
                                        "{} output checksum mismatch",
                                        target.display()
                                    ));
                                }
                                let previous: serde_json::Value = serde_json::from_slice(
                                    &std::fs::read(&target).map_err(|e| e.to_string())?,
                                )
                                .map_err(|e| e.to_string())?;
                                validate_cached(
                                    &previous,
                                    fold,
                                    scenario,
                                    fingerprint,
                                    config_hash,
                                )?;
                                let copied_config = out.join(format!("config-{}.toml", fold.id));
                                if hash_file(&copied_config)? != *config_hash {
                                    return Err("cached replay config changed".into());
                                }
                                eprintln!(
                                    "{} fold {}: verified inputs, retaining completed replay",
                                    scenario.id, fold.id
                                );
                                continue;
                            }
                            let result = backtest::replay_with_execution(
                                cfg,
                                fold.start,
                                fold.end,
                                &suite.data_root,
                                scenario.initial_cash,
                                Decimal::ONE,
                                prepared,
                                None,
                                &scenario.execution,
                            )?;
                            let mut json =
                                serde_json::to_value(&result).map_err(|e| e.to_string())?;
                            json["suite_fingerprint"] = serde_json::json!(fingerprint);
                            json["source_config_path"] = serde_json::json!(source_config);
                            json["source_config_sha256"] = serde_json::json!(config_hash);
                            json["scenario_id"] = serde_json::json!(scenario.id);
                            std::fs::copy(
                                &source_config,
                                out.join(format!("config-{}.toml", fold.id)),
                            )
                            .map_err(|e| e.to_string())?;
                            write_json(&target, &json)?;
                            std::fs::write(checksum_path, hash_file(&target)?)
                                .map_err(|e| e.to_string())?;
                            eprintln!(
                                "{} fold {}: {} days, Sharpe {:.3}, return {}",
                                scenario.id,
                                fold.id,
                                result.metrics.n,
                                result.metrics.sharpe,
                                result.metrics.total_return
                            );
                        }
                        Ok(())
                    })();
                    if let Err(error) = &result {
                        eprintln!("{} replay failed: {error}", scenario.id);
                    }
                    result
                })
            })
            .collect();
        for handle in handles {
            handle.join().map_err(|_| "replay worker panicked")??;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_alone_cannot_authorize_a_cached_result() {
        let fold = Fold {
            id: 1,
            start: "2026-01-01T00:00:00Z".parse().unwrap(),
            end: "2026-01-02T00:00:00Z".parse().unwrap(),
        };
        let scenario = Scenario {
            id: "impact".into(),
            config_dir: None,
            initial_cash: Decimal::from(100),
            execution: Settings::default(),
        };
        assert!(validate_cached(
            &serde_json::json!({"suite_fingerprint":"abc"}),
            &fold,
            &scenario,
            "abc",
            "config"
        )
        .is_err());
        let mut value = serde_json::json!({"suite_fingerprint":"abc", "schema_version":"rust-backtest-2", "source_config_sha256":"config", "scenario_id":"impact", "initial_cash":"100", "execution":scenario.execution,
            "steps":[{"as_of":"2026-01-01T00:00:00Z", "attribution":{}}, {"as_of":"2026-01-02T00:00:00Z", "attribution":{}}]});
        assert!(validate_cached(&value, &fold, &scenario, "abc", "config").is_ok());
        value["steps"][1]["as_of"] = serde_json::json!("2026-01-03T00:00:00Z");
        assert!(validate_cached(&value, &fold, &scenario, "abc", "config").is_err());
    }
}
