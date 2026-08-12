use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use equity_data::UniverseBucket as DataBucket;
use features_stockholm::{DirectionTrainingRow, InstrumentMeta, TrainingRow, UniverseBucket};
use time::Date;

const USAGE: &str = "\
usage: stockholm-portfolio <command>

  collect --data-root <dir> --start YYYY-MM-DD --end YYYY-MM-DD
      Fetch the current Nasdaq Stockholm/First North universe and adjusted
      research history. The resulting dataset is survivorship-contaminated.

  collect-benchmark --data-root <dir> --symbol OMXSGI
                    --start YYYY-MM-DD --end YYYY-MM-DD
      Fetch official Nasdaq SOD/EOD index levels through the shared equity
      data provider. OMXSGI is the broad Stockholm gross-return benchmark.

  collect-fi-net-shorts --data-root <dir>
      Archive and normalize FI's official historical and current aggregate
      disclosed net-short positions through the shared equity data provider.

  collect-skv-equity-history --data-root <dir>
      Archive and normalize Skatteverket's official equity-history catalogue
      as the discovery stage for historical listing/delisting reconstruction.

  collect-skv-listing-events --data-root <dir> --catalogue <json>
                             [--pause-ms 500] [--limit 0]
      Resumably archive company pages and parse their listing-history tables.

  collect-fi-pdmr --data-root <dir> --start YYYY-MM-DD --end YYYY-MM-DD
                  [--pause-ms 500] [--interval-days 14]
      Archive bounded publication-date exports from FI's PDMR register and
      normalize transactions without moving availability to the trade date.

  collect-nasdaq-reports --data-root <dir> --start YYYY-MM-DD --end YYYY-MM-DD
                          [--pause-ms 100]
      Archive official Nasdaq Main Market Stockholm financial-report release
      events with publication timestamps and resumable raw JSONP pages.

  collect-nasdaq-report-messages --data-root <dir> --nasdaq-reports <json>
                                  [--pause-ms 250] [--concurrency 4] [--limit 0]
      Resumably archive official financial-report HTML bodies and attachment
      metadata through the shared equity provider; no bot owns web decoding.

  collect-nasdaq-report-attachments --data-root <dir> --report-messages <json>
                                     [--pause-ms 100] [--concurrency 4]
                                     [--max-attachment-mb 64] [--limit 0]
                                     [--cached-only]
      Download official report PDFs with bounded concurrency, then extract
      sequentially in memory-limited subprocesses. Existing files are reused.

  audit-nasdaq-report-attachments --report-messages <json>
                                  --report-attachments <json>
      Compare the fixed Rust report-metric parser's body-only coverage with
      coverage after adding the causally associated extracted PDF text.

  collect-nasdaq-company-news --data-root <dir> --start YYYY-MM-DD --end YYYY-MM-DD
                               [--pause-ms 100]
      Archive every official Nasdaq Main Market Stockholm company-news event
      and its original category/headline through the shared equity provider.

  collect-nasdaq-equity-notices --data-root <dir>
                                --start YYYY-MM-DD --end YYYY-MM-DD
                                [--pause-ms 50]
      Archive official Nasdaq equity-market notices and extract Stockholm
      listing/delisting identifiers and explicit effective trading dates.

  collect-nasdaq-market-history --data-root <dir>
                                --start YYYY-MM-DD --end YYYY-MM-DD
                                [--supplemental-universe <json>]
                                [--pause-ms 100] [--limit 0]
      Resumably archive official daily OHLC, closing bid/ask, turnover, and
      trade counts for current and explicitly supplied prior Stockholm Main
      Market shares. Research-only: old delistings disappear and history caps.

  collect-esef-annual --data-root <dir> --nasdaq-reports <json>
                      [--pause-ms 50]
      Archive Swedish ESEF annual filings through the shared equity data
      provider and retain causal, standard IFRS numeric facts.

  collect-riksbank-macro --data-root <dir> --start YYYY-MM-DD --end YYYY-MM-DD
      Archive the four predeclared official SEK FX/KIX/policy-rate series
      through the shared equity data provider.

  collect-eodhd-delisted --data-root <dir> --nasdaq-equity-notices <json>
                         --start YYYY-MM-DD --end YYYY-MM-DD
                         [--pause-ms 100] [--limit 0]
      With EODHD_API_TOKEN, archive licensed inactive Stockholm common-stock
      EOD histories only where ISIN matches an official Nasdaq delisting.

  training-matrix --data-root <dir> --start YYYY-MM-DD --end YYYY-MM-DD
                  --out <jsonl> [--horizon-sessions 5] [--min-adv-sek 1000000]
                  [--feature-set baseline|context|residual|residual-public-short|residual-pdmr|residual-pdmr-reports|residual-fundamentals|residual-pdmr-macro|residual-pdmr-microstructure|residual-pdmr-microstructure-borrow|residual-pdmr-microstructure-borrow-news|residual-pdmr-microstructure-borrow-news-report-text|residual-pdmr-microstructure-borrow-news-report-attachments]
                  [--fi-net-shorts <json>] [--fi-pdmr <json>]
                  [--nasdaq-reports <json>] [--esef-annual <json>]
                  [--nasdaq-company-news <json>]
                  [--nasdaq-report-messages <json>]
                  [--nasdaq-report-attachments <json>]
                  [--riksbank-macro <json>]
                  [--nasdaq-market-history-root <dir>]
                  [--ib-fee-history-root <dir>]
      Emit final Rust-owned features, missing flags, labels, and sample weights
      for Nasdaq Stockholm Large, Mid, and Small Cap only.

  direction-training-matrix --index-dir <dir> --start YYYY-MM-DD --end YYYY-MM-DD
                            --out <jsonl> [--horizon-sessions 20]
      Emit causal Rust-owned market-direction features and executable OMXSGI
      SOD-to-SOD absolute-return labels from official Nasdaq index histories.

  backtest --matrix <jsonl> --model <json> --start YYYY-MM-DD --end YYYY-MM-DD
           --out <json> [--benchmark <json>] [--cadence-sessions 5]
           [--max-positions 20]
           [--ranking edge|edge_volatility]
           [--sizing equal|conviction|inverse_volatility|edge_volatility]
           [--max-gross 1] [--target-net <N>] [--direction-overlay]
           [--position-weight 0.05] [--min-position-weight 0]
           [--reference-edge 0.01] [--reference-volatility 0.02]
           [--cost-multiple 1]
      Replay one strictly-forward model fold with no long/short quota.

  direction-backtest --matrix <jsonl> --model <json>
                     --start YYYY-MM-DD --end YYYY-MM-DD --out <json>
                     [--max-gross 1]
      Replay a trained direction fold and the fixed trend control on identical
      non-overlapping OMXSGI holding periods.

  summarize-direction --fold <json> [--fold <json> ...] --out <json>
      Recompute aggregate walk-forward direction metrics in Rust from frozen,
      non-overlapping fold steps.

  add-benchmark --report <json> --benchmark <json> [--out <json>]
      Add exact-session benchmark attribution to an existing frozen Rust fold
      without rescoring or changing any portfolio decisions.
";

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct MatrixManifest {
    kind: String,
    feature_set_version: String,
    label_version: String,
    features: Vec<String>,
    horizon_sessions: usize,
    min_adv20_sek: f64,
    survivorship_status: String,
    universe_source: String,
    history_source: String,
    #[serde(default)]
    universe_policy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    public_short_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    public_short_asof_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pdmr_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pdmr_asof_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    company_news_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    company_news_asof_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    company_news_mapping_coverage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    all_company_news_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    all_company_news_asof_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    all_company_news_mapping_coverage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    report_text_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    report_text_asof_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    report_text_mapping_coverage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    report_attachment_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    report_attachment_asof_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    report_attachment_coverage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    esef_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    esef_asof_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    esef_mapping_coverage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    macro_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    macro_asof_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    microstructure_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    microstructure_asof_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    microstructure_coverage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    borrow_fee_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    borrow_fee_asof_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    borrow_fee_coverage: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct DirectionMatrixManifest {
    kind: String,
    feature_set_version: String,
    label_version: String,
    features: Vec<String>,
    horizon_sessions: usize,
    primary_index: String,
    index_sources: std::collections::BTreeMap<String, String>,
    decision_policy: String,
    label_policy: String,
}

fn get(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|value| value == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn need(args: &[String], name: &str) -> Result<String, String> {
    get(args, name).ok_or_else(|| format!("{name} is required"))
}

fn date(args: &[String], name: &str) -> Result<Date, String> {
    let format = time::macros::format_description!("[year]-[month]-[day]");
    Date::parse(&need(args, name)?, format).map_err(|error| format!("bad {name}: {error}"))
}

fn number<T: std::str::FromStr>(args: &[String], name: &str, default: T) -> Result<T, String>
where
    T::Err: std::fmt::Display,
{
    get(args, name)
        .map(|value| {
            value
                .parse()
                .map_err(|error| format!("bad {name}: {error}"))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn collect(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let manifest = equity_data::PublicEquityData::new()?.collect_stockholm(
        &root,
        date(args, "--start")?,
        date(args, "--end")?,
    )?;
    println!(
        "collected {}/{} instruments, {} bars; {} failures -> {}",
        manifest.instruments_with_history,
        manifest.instruments_discovered,
        manifest.bars,
        manifest.instruments_failed,
        root.display()
    );
    Ok(())
}

fn collect_benchmark(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let symbol = get(args, "--symbol").unwrap_or_else(|| "OMXSGI".into());
    let history = equity_data::collect_nasdaq_benchmark(
        &root,
        &symbol,
        date(args, "--start")?,
        date(args, "--end")?,
    )?;
    println!(
        "collected {} official {} sessions -> {}",
        history.bars.len(),
        history.symbol,
        root.join("benchmarks")
            .join(format!("{}.json", history.symbol))
            .display()
    );
    Ok(())
}

fn collect_fi_net_shorts(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let collection = equity_data::collect_fi_net_shorts(&root)?;
    println!(
        "collected {} historical and {} aggregate FI net-short positions -> {}",
        collection.historical_positions,
        collection.aggregate_positions,
        collection.dataset_path.display(),
    );
    Ok(())
}

fn collect_skv_equity_history(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let collection = equity_data::collect_skv_equity_history_catalogue(&root)?;
    println!(
        "collected {} Skatteverket company links from {} source pages -> {}",
        collection.companies,
        collection.source_pages,
        collection.catalogue_path.display(),
    );
    Ok(())
}

fn collect_skv_listing_events(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let catalogue =
        equity_data::load_skv_equity_history_catalogue(Path::new(&need(args, "--catalogue")?))?;
    let collection = equity_data::collect_skv_listing_history(
        &root,
        &catalogue,
        number(args, "--pause-ms", 500_u64)?,
        number(args, "--limit", 0_usize)?,
    )?;
    println!(
        "archived {}/{} Skatteverket company pages, {} listing rows, {} failures -> {}",
        collection.companies_archived,
        collection.companies_requested,
        collection.listing_rows,
        collection.failures,
        collection.dataset_path.display(),
    );
    Ok(())
}

fn collect_fi_pdmr(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let collection = equity_data::collect_fi_pdmr(
        &root,
        date(args, "--start")?,
        date(args, "--end")?,
        number(args, "--pause-ms", 500_u64)?,
        number(args, "--interval-days", 14_usize)?,
    )?;
    println!(
        "collected {} FI PDMR rows across {} exports and {} ISINs -> {}",
        collection.transactions,
        collection.intervals,
        collection.unique_isins,
        collection.dataset_path.display(),
    );
    Ok(())
}

fn collect_nasdaq_reports(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let collection = equity_data::collect_nasdaq_financial_reports(
        &root,
        date(args, "--start")?,
        date(args, "--end")?,
        number(args, "--pause-ms", 100_u64)?,
    )?;
    println!(
        "collected {} Nasdaq financial-report announcements across {} companies and {} accepted pages -> {}",
        collection.announcements,
        collection.companies,
        collection.requests,
        collection.dataset_path.display(),
    );
    Ok(())
}

fn collect_nasdaq_company_news(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let collection = equity_data::collect_nasdaq_stockholm_company_news(
        &root,
        date(args, "--start")?,
        date(args, "--end")?,
        number(args, "--pause-ms", 100_u64)?,
    )?;
    println!(
        "collected {} Nasdaq company-news announcements across {} companies and {} accepted pages -> {}",
        collection.announcements,
        collection.companies,
        collection.requests,
        collection.dataset_path.display(),
    );
    Ok(())
}

fn collect_nasdaq_report_messages(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let metadata_path = PathBuf::from(need(args, "--nasdaq-reports")?);
    let metadata = equity_data::load_nasdaq_company_news(&metadata_path)?;
    let collection = equity_data::collect_nasdaq_financial_report_messages(
        &root,
        &metadata_path,
        &metadata,
        number(args, "--pause-ms", 250_u64)?,
        number(args, "--concurrency", 4_usize)?,
        number(args, "--limit", 0_usize)?,
    )?;
    println!(
        "collected {}/{} Nasdaq financial-report messages and {} attachment links ({} failures) -> {}",
        collection.messages,
        collection.requested,
        collection.attachments,
        collection.failures,
        collection.dataset_path.display(),
    );
    Ok(())
}

fn collect_nasdaq_report_attachments(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let metadata_path = PathBuf::from(need(args, "--report-messages")?);
    let metadata = equity_data::load_nasdaq_financial_report_messages(&metadata_path)?;
    let max_attachment_mb = number(args, "--max-attachment-mb", 64_u64)?;
    let max_attachment_bytes = max_attachment_mb
        .checked_mul(1024 * 1024)
        .ok_or_else(|| "--max-attachment-mb is too large".to_owned())?;
    let collection = equity_data::collect_nasdaq_financial_report_attachments(
        &root,
        &metadata_path,
        &metadata,
        number(args, "--pause-ms", 100_u64)?,
        number(args, "--concurrency", 4_usize)?,
        max_attachment_bytes,
        number(args, "--limit", 0_usize)?,
        args.iter().any(|arg| arg == "--cached-only"),
    )?;
    println!(
        "downloaded {}/{} requested Nasdaq report PDFs ({} available), extracted {} texts / {} chars from {} bytes, {} failures -> {}",
        collection.downloaded,
        collection.requested,
        collection.available,
        collection.extracted,
        collection.text_chars,
        collection.bytes,
        collection.failures,
        collection.dataset_path.display(),
    );
    Ok(())
}

fn extract_nasdaq_report_pdf_worker(args: &[String]) -> Result<(), String> {
    equity_data::extract_nasdaq_financial_report_pdf(
        Path::new(&need(args, "--input")?),
        Path::new(&need(args, "--output")?),
        number(args, "--max-bytes", 64_u64 * 1024 * 1024)?,
    )?;
    Ok(())
}

fn audit_nasdaq_report_attachments(args: &[String]) -> Result<(), String> {
    let messages_path = PathBuf::from(need(args, "--report-messages")?);
    let attachments_path = PathBuf::from(need(args, "--report-attachments")?);
    let messages = equity_data::load_nasdaq_financial_report_messages(&messages_path)?;
    let attachments = equity_data::load_nasdaq_financial_report_attachments(&attachments_path)?;
    let mut texts_by_disclosure = std::collections::BTreeMap::<u64, Vec<String>>::new();
    for document in &attachments.documents {
        let Some(path) = document.extracted_text_file.as_deref() else {
            continue;
        };
        let text = std::fs::read_to_string(path).map_err(|error| format!("{path}: {error}"))?;
        for disclosure_id in &document.disclosure_ids {
            texts_by_disclosure
                .entry(*disclosure_id)
                .or_default()
                .push(text.clone());
        }
    }
    let mut body_events = Vec::new();
    let mut augmented_events = Vec::new();
    let mut augmented_messages = 0_usize;
    for message in &messages.messages {
        let announcement = &message.announcement;
        let instrument_id = equity_data::nasdaq_news_issuer_key(&announcement.company);
        let baseline = features_stockholm::FinancialReportTextEvent {
            instrument_id,
            publication_date: announcement.publication_date,
            publication_key: announcement.published.clone(),
            language: announcement.language.clone(),
            body_text: message.body_text.clone(),
            extracted_metrics: None,
        };
        let mut augmented = baseline.clone();
        if let Some(texts) = texts_by_disclosure.get(&announcement.disclosure_id) {
            augmented_messages += 1;
            augmented.extracted_metrics =
                Some(features_stockholm::report_text_metrics_with_supplements(
                    &augmented.body_text,
                    texts.iter().map(String::as_str),
                ));
        }
        body_events.push(baseline);
        augmented_events.push(augmented);
    }
    let body = features_stockholm::report_text_metric_coverage(&body_events)?;
    let augmented = features_stockholm::report_text_metric_coverage(&augmented_events)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "attachment_documents": attachments.documents.len(),
            "attachment_texts": attachments.documents.iter().filter(|document| document.extracted_text_file.is_some()).count(),
            "messages_augmented": augmented_messages,
            "body_only": body,
            "body_plus_attachments": augmented,
        }))
        .map_err(|error| error.to_string())?
    );
    Ok(())
}

fn collect_nasdaq_equity_notices(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let collection = equity_data::collect_nasdaq_stockholm_equity_notices(
        &root,
        date(args, "--start")?,
        date(args, "--end")?,
        number(args, "--pause-ms", 50_u64)?,
    )?;
    println!(
        "collected {}/{} Stockholm equity notices from {} candidates ({} carry identifiers, {} failures, {} query pages) -> {}",
        collection.notices,
        collection.metadata_notices_seen,
        collection.candidate_messages,
        collection.identifiers,
        collection.failures,
        collection.requests,
        collection.dataset_path.display(),
    );
    Ok(())
}

fn collect_nasdaq_market_history(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let supplemental_universe = get(args, "--supplemental-universe")
        .map(|path| equity_data::load_instruments(Path::new(&path)))
        .transpose()?
        .unwrap_or_default();
    let collection = equity_data::collect_nasdaq_stockholm_market_history(
        &root,
        date(args, "--start")?,
        date(args, "--end")?,
        number(args, "--pause-ms", 100_u64)?,
        number(args, "--limit", 0_usize)?,
        &supplemental_universe,
    )?;
    println!(
        "collected {} official Nasdaq market histories and {} bars ({} failures) -> {}",
        collection.instruments,
        collection.bars,
        collection.failures,
        collection.manifest_path.display(),
    );
    Ok(())
}

fn collect_esef_annual(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let company_news =
        equity_data::load_nasdaq_company_news(Path::new(&need(args, "--nasdaq-reports")?))?;
    let collection = equity_data::collect_esef_annual_filings(
        &root,
        &company_news,
        number(args, "--pause-ms", 50_u64)?,
    )?;
    println!(
        "parsed {}/{} Swedish ESEF filings across {} entities and {} IFRS facts ({} failures) -> {}",
        collection.filings_parsed,
        collection.filings_seen,
        collection.entities,
        collection.facts,
        collection.failures,
        collection.dataset_path.display(),
    );
    Ok(())
}

fn collect_riksbank_macro(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let collection = equity_data::collect_riksbank_stockholm_macro(
        &root,
        date(args, "--start")?,
        date(args, "--end")?,
    )?;
    println!(
        "collected {} Riksbank series and {} observations -> {}",
        collection.series,
        collection.observations,
        collection.dataset_path.display(),
    );
    Ok(())
}

fn collect_eodhd_delisted(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let notices_path = PathBuf::from(need(args, "--nasdaq-equity-notices")?);
    let notices = equity_data::load_nasdaq_equity_notices(&notices_path)?;
    let token = std::env::var("EODHD_API_TOKEN")
        .map_err(|_| "EODHD_API_TOKEN is required for collect-eodhd-delisted".to_owned())?;
    let collection = equity_data::collect_eodhd_stockholm_delisted(
        &root,
        &notices_path,
        &notices,
        &token,
        date(args, "--start")?,
        date(args, "--end")?,
        number(args, "--pause-ms", 100_u64)?,
        number(args, "--limit", 0_usize)?,
    )?;
    println!(
        "matched {}/{} official delisting ISINs against {} EODHD inactive common stocks: {} histories, {} bars, {} failures -> {}",
        collection.matched_isins,
        collection.official_isins,
        collection.provider_symbols,
        collection.histories,
        collection.bars,
        collection.failures,
        collection.dataset_path.display(),
    );
    Ok(())
}

fn matrix(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let (source, histories) = equity_data::load_stockholm(&root)?;
    let company_news_dataset = get(args, "--nasdaq-reports")
        .map(|path| equity_data::load_nasdaq_company_news(Path::new(&path)))
        .transpose()?;
    let mut instruments_by_issuer = std::collections::BTreeMap::<String, Vec<String>>::new();
    for history in &histories {
        if matches!(
            history.instrument.bucket,
            DataBucket::LargeCap | DataBucket::MidCap | DataBucket::SmallCap
        ) {
            instruments_by_issuer
                .entry(equity_data::stockholm_security_issuer_key(
                    &history.instrument.name,
                ))
                .or_default()
                .push(history.instrument.orderbook_id.clone());
        }
    }
    let mut matched_disclosures = 0_usize;
    let mut matched_instruments = std::collections::BTreeSet::new();
    let report_events = company_news_dataset
        .as_ref()
        .map(|dataset| {
            dataset
                .announcements
                .iter()
                .flat_map(|announcement| {
                    let key = equity_data::nasdaq_news_issuer_key(&announcement.company);
                    let Some(instrument_ids) = instruments_by_issuer.get(&key) else {
                        return Vec::new();
                    };
                    matched_disclosures += 1;
                    instrument_ids
                        .iter()
                        .map(|instrument_id| {
                            matched_instruments.insert(instrument_id.clone());
                            features_stockholm::FinancialReportEvent {
                                instrument_id: instrument_id.clone(),
                                publication_date: announcement.publication_date,
                                publication_key: announcement.published.clone(),
                                after_market_close: announcement
                                    .published
                                    .get(11..19)
                                    .is_some_and(|time| time >= "17:25:00"),
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(dataset) = &company_news_dataset {
        eprintln!(
            "Nasdaq report mapping: {matched_disclosures}/{} disclosures, {}/{} current Main Market securities",
            dataset.announcements.len(),
            matched_instruments.len(),
            instruments_by_issuer.values().map(Vec::len).sum::<usize>()
        );
    }
    let all_company_news_dataset = get(args, "--nasdaq-company-news")
        .map(|path| equity_data::load_nasdaq_company_news(Path::new(&path)))
        .transpose()?;
    let mut matched_company_news = 0_usize;
    let mut matched_company_news_instruments = std::collections::BTreeSet::new();
    let company_news_events = all_company_news_dataset
        .as_ref()
        .map(|dataset| {
            dataset
                .announcements
                .iter()
                .flat_map(|announcement| {
                    let key = equity_data::nasdaq_news_issuer_key(&announcement.company);
                    let Some(instrument_ids) = instruments_by_issuer.get(&key) else {
                        return Vec::new();
                    };
                    matched_company_news += 1;
                    instrument_ids
                        .iter()
                        .map(|instrument_id| {
                            matched_company_news_instruments.insert(instrument_id.clone());
                            features_stockholm::CompanyNewsEvent {
                                instrument_id: instrument_id.clone(),
                                publication_date: announcement.publication_date,
                                publication_key: announcement.disclosure_id.to_string(),
                                after_market_close: announcement
                                    .published
                                    .get(11..19)
                                    .is_some_and(|time| time >= "17:25:00"),
                                kind: company_news_kind(&announcement.category),
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(dataset) = &all_company_news_dataset {
        eprintln!(
            "Nasdaq all-news mapping: {matched_company_news}/{} disclosures, {}/{} current Main Market securities",
            dataset.announcements.len(),
            matched_company_news_instruments.len(),
            instruments_by_issuer.values().map(Vec::len).sum::<usize>()
        );
    }
    let report_text_dataset = get(args, "--nasdaq-report-messages")
        .map(|path| equity_data::load_nasdaq_financial_report_messages(Path::new(&path)))
        .transpose()?;
    let report_attachment_dataset = get(args, "--nasdaq-report-attachments")
        .map(|path| equity_data::load_nasdaq_financial_report_attachments(Path::new(&path)))
        .transpose()?;
    let attachment_feature_requested = get(args, "--feature-set").as_deref()
        == Some("residual-pdmr-microstructure-borrow-news-report-attachments");
    if attachment_feature_requested {
        let dataset = report_attachment_dataset.as_ref().ok_or_else(|| {
            "--nasdaq-report-attachments is required for report-attachment features".to_owned()
        })?;
        if !dataset.network_downloads_enabled
            || dataset.requested_pdf_urls != dataset.available_pdf_urls
        {
            return Err(format!(
                "report-attachment archive is diagnostic/partial: network_enabled={}, requested {} of {} unique PDFs",
                dataset.network_downloads_enabled,
                dataset.requested_pdf_urls,
                dataset.available_pdf_urls,
            ));
        }
    }
    let mut attachment_paths_by_disclosure = std::collections::BTreeMap::<u64, Vec<String>>::new();
    if let Some(dataset) = &report_attachment_dataset {
        for document in &dataset.documents {
            let Some(path) = document.extracted_text_file.as_ref() else {
                continue;
            };
            for disclosure_id in &document.disclosure_ids {
                attachment_paths_by_disclosure
                    .entry(*disclosure_id)
                    .or_default()
                    .push(path.clone());
            }
        }
    }
    let mut attachment_metrics_by_disclosure =
        std::collections::BTreeMap::<u64, Vec<Option<f64>>>::new();
    if attachment_feature_requested {
        let messages = report_text_dataset.as_ref().ok_or_else(|| {
            "--nasdaq-report-messages is required for report-attachment features".to_owned()
        })?;
        for message in &messages.messages {
            let Some(paths) =
                attachment_paths_by_disclosure.get(&message.announcement.disclosure_id)
            else {
                continue;
            };
            let texts = paths
                .iter()
                .map(|path| {
                    std::fs::read_to_string(path).map_err(|error| format!("{path}: {error}"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            attachment_metrics_by_disclosure.insert(
                message.announcement.disclosure_id,
                features_stockholm::report_text_metrics_with_supplements(
                    &message.body_text,
                    texts.iter().map(String::as_str),
                ),
            );
        }
    }
    let mut matched_report_text = 0_usize;
    let mut matched_report_text_instruments = std::collections::BTreeSet::new();
    let report_text_events = report_text_dataset
        .as_ref()
        .map(|dataset| {
            dataset
                .messages
                .iter()
                .flat_map(|message| {
                    let announcement = &message.announcement;
                    let key = equity_data::nasdaq_news_issuer_key(&announcement.company);
                    let Some(instrument_ids) = instruments_by_issuer.get(&key) else {
                        return Vec::new();
                    };
                    matched_report_text += 1;
                    instrument_ids
                        .iter()
                        .map(|instrument_id| {
                            matched_report_text_instruments.insert(instrument_id.clone());
                            features_stockholm::FinancialReportTextEvent {
                                instrument_id: instrument_id.clone(),
                                publication_date: announcement.publication_date,
                                publication_key: announcement.published.clone(),
                                language: announcement.language.clone(),
                                body_text: message.body_text.clone(),
                                extracted_metrics: attachment_metrics_by_disclosure
                                    .get(&announcement.disclosure_id)
                                    .cloned(),
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(dataset) = &report_text_dataset {
        eprintln!(
            "Nasdaq report-text mapping: {matched_report_text}/{} messages, {}/{} current Main Market securities",
            dataset.messages.len(),
            matched_report_text_instruments.len(),
            instruments_by_issuer.values().map(Vec::len).sum::<usize>()
        );
    }
    let report_text_metric_coverage = (!report_text_events.is_empty())
        .then(|| features_stockholm::report_text_metric_coverage(&report_text_events))
        .transpose()?;
    if let Some(coverage) = &report_text_metric_coverage {
        eprintln!(
            "Rust report-text extraction: {}/{} deduplicated events have at least one declared metric ({:?})",
            coverage.events_with_any_metric,
            coverage.deduplicated_events,
            coverage.by_feature,
        );
    }
    let esef_dataset = get(args, "--esef-annual")
        .map(|path| equity_data::load_esef_annual_filings(Path::new(&path)))
        .transpose()?;
    let mut matched_esef_filings = 0_usize;
    let mut matched_esef_instruments = std::collections::BTreeSet::new();
    let fundamental_events = esef_dataset
        .as_ref()
        .map(|dataset| {
            dataset
                .filings
                .iter()
                .flat_map(|filing| {
                    let key = equity_data::nasdaq_news_issuer_key(&filing.entity_name);
                    let Some(instrument_ids) = instruments_by_issuer.get(&key) else {
                        return Vec::new();
                    };
                    matched_esef_filings += 1;
                    let values = equity_data::normalize_esef_annual_fundamentals(filing);
                    instrument_ids
                        .iter()
                        .map(|instrument_id| {
                            matched_esef_instruments.insert(instrument_id.clone());
                            features_stockholm::AnnualFundamentalEvent {
                                instrument_id: instrument_id.clone(),
                                available_date: filing.available_date,
                                report_period_end: filing.report_period_end,
                                filing_key: filing.filing_id.clone(),
                                reporting_currency: values.reporting_currency.clone(),
                                revenue: values.revenue,
                                prior_revenue: values.prior_revenue,
                                operating_profit: values.operating_profit,
                                net_income: values.net_income,
                                prior_net_income: values.prior_net_income,
                                assets: values.assets,
                                prior_assets: values.prior_assets,
                                equity: values.equity,
                                prior_equity: values.prior_equity,
                                cash: values.cash,
                                operating_cash_flow: values.operating_cash_flow,
                                current_assets: values.current_assets,
                                current_liabilities: values.current_liabilities,
                                basic_eps: values.basic_eps,
                                weighted_average_shares: values.weighted_average_shares,
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(dataset) = &esef_dataset {
        eprintln!(
            "ESEF mapping: {matched_esef_filings}/{} filings, {}/{} current Main Market securities",
            dataset.filings.len(),
            matched_esef_instruments.len(),
            instruments_by_issuer.values().map(Vec::len).sum::<usize>()
        );
    }
    let horizon = number(args, "--horizon-sessions", 5_usize)?;
    let min_adv = number(args, "--min-adv-sek", 1_000_000.0_f64)?;
    let mut bars = Vec::new();
    let mut instruments = Vec::new();
    for history in histories {
        let id = history.instrument.orderbook_id.clone();
        instruments.push(InstrumentMeta {
            instrument_id: id.clone(),
            symbol: history.instrument.symbol,
            isin: history.instrument.isin,
            sector: history.instrument.sector,
            bucket: bucket(history.instrument.bucket),
        });
        bars.extend(
            history
                .bars
                .into_iter()
                .map(|bar| features_stockholm::DailyBar {
                    date: bar.date,
                    instrument_id: id.clone(),
                    raw_open: bar.open,
                    raw_high: bar.high,
                    raw_low: bar.low,
                    raw_close: bar.close,
                    volume: bar.volume,
                    adjusted_close: bar.adjusted_close,
                }),
        );
    }
    let feature_set = get(args, "--feature-set").unwrap_or_else(|| "baseline".into());
    let feature_set = match feature_set.as_str() {
        "baseline" => features_stockholm::FeatureSet::Baseline,
        "context" => features_stockholm::FeatureSet::Context,
        "residual" => features_stockholm::FeatureSet::Residual,
        "residual-public-short" => features_stockholm::FeatureSet::ResidualPublicShort,
        "residual-pdmr" => features_stockholm::FeatureSet::ResidualPdmr,
        "residual-pdmr-reports" => features_stockholm::FeatureSet::ResidualPdmrReports,
        "residual-fundamentals" => features_stockholm::FeatureSet::ResidualFundamentals,
        "residual-pdmr-macro" => features_stockholm::FeatureSet::ResidualPdmrMacro,
        "residual-pdmr-microstructure" => {
            features_stockholm::FeatureSet::ResidualPdmrMicrostructure
        }
        "residual-pdmr-microstructure-borrow" => {
            features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrow
        }
        "residual-pdmr-microstructure-borrow-news" => {
            features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNews
        }
        "residual-pdmr-microstructure-borrow-news-report-text" => {
            features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNewsReportText
        }
        "residual-pdmr-microstructure-borrow-news-report-attachments" => {
            features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNewsReportAttachments
        }
        other => return Err(format!("unknown Stockholm feature set {other:?}")),
    };
    let public_short_dataset = get(args, "--fi-net-shorts")
        .map(|path| equity_data::load_fi_net_shorts(Path::new(&path)))
        .transpose()?;
    if feature_set == features_stockholm::FeatureSet::ResidualPublicShort
        && public_short_dataset.is_none()
    {
        return Err("--fi-net-shorts is required for residual-public-short features".into());
    }
    let public_short_events = public_short_dataset
        .as_ref()
        .map(|dataset| {
            dataset
                .historical
                .iter()
                .map(|event| features_stockholm::PublicShortPositionEvent {
                    holder: event.holder.clone(),
                    isin: event.isin.clone(),
                    position_date: event.position_date,
                    position_percent: event.position_percent,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let pdmr_dataset = get(args, "--fi-pdmr")
        .map(|path| equity_data::load_fi_pdmr(Path::new(&path)))
        .transpose()?;
    if matches!(
        feature_set,
        features_stockholm::FeatureSet::ResidualPdmr
            | features_stockholm::FeatureSet::ResidualPdmrMacro
            | features_stockholm::FeatureSet::ResidualPdmrMicrostructure
            | features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrow
            | features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNews
            | features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNewsReportText
            | features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNewsReportAttachments
    ) && pdmr_dataset.is_none()
    {
        return Err("--fi-pdmr is required for residual-pdmr features".into());
    }
    if feature_set == features_stockholm::FeatureSet::ResidualPdmrReports
        && (pdmr_dataset.is_none() || company_news_dataset.is_none())
    {
        return Err(
            "--fi-pdmr and --nasdaq-reports are required for residual-pdmr-reports features".into(),
        );
    }
    if feature_set == features_stockholm::FeatureSet::ResidualFundamentals && esef_dataset.is_none()
    {
        return Err("--esef-annual is required for residual-fundamentals features".into());
    }
    let pdmr_events = pdmr_dataset
        .as_ref()
        .map(|dataset| {
            dataset
                .transactions
                .iter()
                .filter_map(|event| {
                    Some(features_stockholm::PdmrTransactionEvent {
                        publication_date: event.publication_date,
                        transaction_date: event.transaction_date,
                        pdmr: event.pdmr.clone(),
                        isin: event.isin.clone()?,
                        initial_notification: event.initial_notification,
                        linked_to_share_option_programme: event.linked_to_share_option_programme,
                        nature: event.nature.clone(),
                        instrument_type: event.instrument_type.clone(),
                        volume: event.volume,
                        unit: event.unit.clone(),
                        price: event.price,
                        currency: event.currency.clone(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let macro_dataset = get(args, "--riksbank-macro")
        .map(|path| equity_data::load_riksbank_stockholm_macro(Path::new(&path)))
        .transpose()?;
    if feature_set == features_stockholm::FeatureSet::ResidualPdmrMacro && macro_dataset.is_none() {
        return Err("--riksbank-macro is required for residual-pdmr-macro features".into());
    }
    let macro_series = macro_dataset
        .as_ref()
        .map(|dataset| {
            dataset
                .series
                .iter()
                .map(|series| features_stockholm::MacroSeries {
                    series_id: series.series_id.clone(),
                    observations: series
                        .observations
                        .iter()
                        .map(|observation| features_stockholm::MacroObservation {
                            date: observation.date,
                            value: observation.value,
                        })
                        .collect(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let market_history_dataset = get(args, "--nasdaq-market-history-root")
        .map(|path| equity_data::load_nasdaq_market_history(Path::new(&path)))
        .transpose()?;
    if matches!(
        feature_set,
        features_stockholm::FeatureSet::ResidualPdmrMicrostructure
            | features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrow
            | features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNews
            | features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNewsReportText
            | features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNewsReportAttachments
    ) && market_history_dataset.is_none()
    {
        return Err(
            "--nasdaq-market-history-root is required for residual-pdmr-microstructure features"
                .into(),
        );
    }
    let microstructure = market_history_dataset
        .as_ref()
        .map(|(_, histories)| {
            histories
                .iter()
                .flat_map(|history| {
                    history
                        .bars
                        .iter()
                        .map(|bar| features_stockholm::MarketMicrostructureBar {
                            date: bar.date,
                            instrument_id: history.instrument.orderbook_id.clone(),
                            bid: bar.bid,
                            ask: bar.ask,
                            close: bar.close,
                            average: bar.average,
                            turnover_sek: bar.turnover_sek,
                            trades: bar.trades,
                        })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let borrow_fee_root = get(args, "--ib-fee-history-root").map(PathBuf::from);
    if matches!(
        feature_set,
        features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrow
            | features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNews
            | features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNewsReportText
            | features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNewsReportAttachments
    ) && borrow_fee_root.is_none()
    {
        return Err(
            "--ib-fee-history-root is required for residual-pdmr-microstructure-borrow features"
                .into(),
        );
    }
    if matches!(
        feature_set,
        features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNews
            | features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNewsReportText
            | features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNewsReportAttachments
    ) && all_company_news_dataset.is_none()
    {
        return Err(
            "--nasdaq-company-news is required for residual-pdmr-microstructure-borrow-news features"
                .into(),
        );
    }
    if matches!(
        feature_set,
        features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNewsReportText
            | features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNewsReportAttachments
    ) && report_text_dataset.is_none()
    {
        return Err("--nasdaq-report-messages is required for report-text features".into());
    }
    let borrow_records = borrow_fee_root
        .as_ref()
        .map(|root| ib::stocks::load_history_records(root, ib::stocks::DailySeries::FeeRate))
        .transpose()?
        .unwrap_or_default();
    let instruments_by_isin = instruments
        .iter()
        .map(|instrument| (instrument.isin.as_str(), instrument.instrument_id.as_str()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let borrow_fees = borrow_records
        .iter()
        .filter_map(|record| {
            let isin = record.stock.isin.as_deref()?;
            let instrument_id = instruments_by_isin.get(isin)?;
            Some(
                record
                    .bars
                    .iter()
                    .map(move |bar| features_stockholm::BorrowFeeBar {
                        date: bar.date,
                        instrument_id: (*instrument_id).to_owned(),
                        annual_rate: bar.close,
                    }),
            )
        })
        .flatten()
        .collect::<Vec<_>>();
    let matrix = features_stockholm::training_matrix_for_named_feature_set_with_all_sources(
        &bars,
        &instruments,
        date(args, "--start")?,
        date(args, "--end")?,
        horizon,
        min_adv,
        feature_set,
        &public_short_events,
        &pdmr_events,
        &report_events,
        &fundamental_events,
        &macro_series,
        &microstructure,
        &borrow_fees,
        &company_news_events,
        &report_text_events,
    )?;
    let path = PathBuf::from(need(args, "--out")?);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let file = std::fs::File::create(&path).map_err(|error| error.to_string())?;
    let mut output = std::io::BufWriter::new(file);
    let manifest = MatrixManifest {
        kind: "stockholm_training_manifest".into(),
        feature_set_version: match feature_set {
            features_stockholm::FeatureSet::Baseline => {
                features_stockholm::BASELINE_FEATURE_SET_VERSION
            }
            features_stockholm::FeatureSet::Context => features_stockholm::FEATURE_SET_VERSION,
            features_stockholm::FeatureSet::Residual => {
                features_stockholm::RESIDUAL_FEATURE_SET_VERSION
            }
            features_stockholm::FeatureSet::ResidualPublicShort => {
                features_stockholm::PUBLIC_SHORT_FEATURE_SET_VERSION
            }
            features_stockholm::FeatureSet::ResidualPdmr => {
                features_stockholm::PDMR_FEATURE_SET_VERSION
            }
            features_stockholm::FeatureSet::ResidualPdmrReports => {
                features_stockholm::REPORT_EVENT_FEATURE_SET_VERSION
            }
            features_stockholm::FeatureSet::ResidualFundamentals => {
                features_stockholm::FUNDAMENTAL_FEATURE_SET_VERSION
            }
            features_stockholm::FeatureSet::ResidualPdmrMacro => {
                features_stockholm::PDMR_MACRO_FEATURE_SET_VERSION
            }
            features_stockholm::FeatureSet::ResidualPdmrMicrostructure => {
                features_stockholm::PDMR_MICROSTRUCTURE_FEATURE_SET_VERSION
            }
            features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrow => {
                features_stockholm::PDMR_MICROSTRUCTURE_BORROW_FEATURE_SET_VERSION
            }
            features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNews => {
                features_stockholm::PDMR_MICROSTRUCTURE_BORROW_NEWS_FEATURE_SET_VERSION
            }
            features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNewsReportText => {
                features_stockholm::PDMR_MICROSTRUCTURE_BORROW_NEWS_REPORT_TEXT_FEATURE_SET_VERSION
            }
            features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNewsReportAttachments => {
                features_stockholm::PDMR_MICROSTRUCTURE_BORROW_NEWS_REPORT_ATTACHMENTS_FEATURE_SET_VERSION
            }
        }
        .into(),
        label_version: features_stockholm::label_version(horizon)?,
        features: matrix.features,
        horizon_sessions: horizon,
        min_adv20_sek: min_adv,
        survivorship_status: source.survivorship_status,
        universe_source: source.universe_source,
        history_source: source.history_source,
        universe_policy: "NASDAQ_STOCKHOLM_MAIN_LARGE_MID_SMALL".into(),
        public_short_source: public_short_dataset
            .as_ref()
            .map(|dataset| dataset.source_page.clone()),
        public_short_asof_policy: public_short_dataset.as_ref().map(|_| {
            "position-date events become usable only on later decision dates; threshold exits remain censored"
                .into()
        }),
        pdmr_source: pdmr_dataset
            .as_ref()
            .map(|dataset| dataset.source_page.clone()),
        pdmr_asof_policy: pdmr_dataset.as_ref().map(|_| {
            "initial share acquisition/disposal filings become usable only after publication date; transaction date and current status never move availability backward"
                .into()
        }),
        company_news_source: company_news_dataset
            .as_ref()
            .map(|dataset| dataset.source_page.clone()),
        company_news_asof_policy: company_news_dataset.as_ref().map(|_| {
            "official financial-report disclosures are issuer-name mapped, translations sharing one issuer/timestamp are deduplicated, and events become usable only on later decision dates"
                .into()
        }),
        company_news_mapping_coverage: company_news_dataset.as_ref().map(|dataset| {
            format!(
                "{matched_disclosures}/{} disclosures mapped to {}/{} current Main Market securities by normalized issuer name",
                dataset.announcements.len(),
                matched_instruments.len(),
                instruments_by_issuer.values().map(Vec::len).sum::<usize>()
            )
        }),
        all_company_news_source: all_company_news_dataset
            .as_ref()
            .map(|dataset| dataset.source_page.clone()),
        all_company_news_asof_policy: all_company_news_dataset.as_ref().map(|_| {
            "official Main Market issuer disclosures are category-mapped in Rust, deduplicated by disclosure ID, and become usable only on later decision dates"
                .into()
        }),
        all_company_news_mapping_coverage: all_company_news_dataset.as_ref().map(|dataset| {
            format!(
                "{matched_company_news}/{} disclosures mapped to {}/{} current Main Market securities by normalized issuer name",
                dataset.announcements.len(),
                matched_company_news_instruments.len(),
                instruments_by_issuer.values().map(Vec::len).sum::<usize>()
            )
        }),
        report_text_source: report_text_dataset
            .as_ref()
            .map(|dataset| dataset.source_page.clone()),
        report_text_asof_policy: report_text_dataset.as_ref().map(|_| {
            "official issuer-authored report bodies are parsed in Rust; translations sharing one issuer/publication timestamp are deduplicated and values become usable only on later decision dates"
                .into()
        }),
        report_text_mapping_coverage: report_text_dataset.as_ref().map(|dataset| {
            let extracted = report_text_metric_coverage
                .as_ref()
                .map(|coverage| {
                    format!(
                        "; {}/{} deduplicated mapped events expose at least one declared Rust metric ({:?})",
                        coverage.events_with_any_metric,
                        coverage.deduplicated_events,
                        coverage.by_feature
                    )
                })
                .unwrap_or_default();
            format!(
                "{matched_report_text}/{} report bodies mapped to {}/{} current Main Market securities by normalized issuer name{extracted}",
                dataset.messages.len(),
                matched_report_text_instruments.len(),
                instruments_by_issuer.values().map(Vec::len).sum::<usize>()
            )
        }),
        report_attachment_source: report_attachment_dataset
            .as_ref()
            .map(|dataset| dataset.source.clone()),
        report_attachment_asof_policy: report_attachment_dataset.as_ref().map(|_| {
            "official PDF text inherits its Nasdaq message publication timestamp; fixed Rust metrics fill only body-missing fields, and values become usable only on later decision dates"
                .into()
        }),
        report_attachment_coverage: report_attachment_dataset.as_ref().map(|dataset| {
            format!(
                "{}/{} unique PDF URLs have extracted text; {} documents carry explicit failures; network_enabled={}",
                dataset
                    .documents
                    .iter()
                    .filter(|document| document.extracted_text_file.is_some())
                    .count(),
                dataset.available_pdf_urls,
                dataset
                    .documents
                    .iter()
                    .filter(|document| document.failure.is_some())
                    .count(),
                dataset.network_downloads_enabled,
            )
        }),
        esef_source: esef_dataset.as_ref().map(|dataset| dataset.source.clone()),
        esef_asof_policy: esef_dataset.as_ref().map(|_| {
            "standard IFRS annual facts become usable only after the conservative filing available_date; period end never controls availability, filing versions remain point-in-time, and ratios join only decision-date-or-earlier prices"
                .into()
        }),
        esef_mapping_coverage: esef_dataset.as_ref().map(|dataset| {
            format!(
                "{matched_esef_filings}/{} filings mapped to {}/{} current Main Market securities by normalized issuer name",
                dataset.filings.len(),
                matched_esef_instruments.len(),
                instruments_by_issuer.values().map(Vec::len).sum::<usize>()
            )
        }),
        macro_source: macro_dataset.as_ref().map(|dataset| dataset.source.clone()),
        macro_asof_policy: macro_dataset.as_ref().map(|dataset| {
            format!(
                "Riksbank observations no later than each decision date are joined in Rust; same-day FX/KIX observations are admissible after their declared publication time, and this downloaded history is not revision-vintage ({})",
                dataset
                    .series
                    .iter()
                    .map(|series| format!("{} {}", series.series_id, series.publication_time))
                    .collect::<Vec<_>>()
                .join(", ")
            )
        }),
        microstructure_source: market_history_dataset
            .as_ref()
            .map(|(manifest, _)| manifest.history_source.clone()),
        microstructure_asof_policy: market_history_dataset.as_ref().map(|_| {
            "completed Nasdaq session observations no later than the decision date are joined in Rust and used only for next-session entry; missing quotes remain missing"
                .into()
        }),
        microstructure_coverage: market_history_dataset.as_ref().map(|(manifest, histories)| {
            format!(
                "{}/{} current/prior-snapshot Main Market instruments, {} daily rows, {} through {}; {}",
                histories.len(),
                manifest.instruments_discovered,
                manifest.bars,
                manifest.earliest_bar.as_deref().unwrap_or("unknown"),
                manifest.latest_bar.as_deref().unwrap_or("unknown"),
                manifest.survivorship_status
            )
        }),
        borrow_fee_source: borrow_fee_root.as_ref().map(|root| {
            format!(
                "IB TWS/Gateway historical FEE_RATE archives in {}",
                root.display()
            )
        }),
        borrow_fee_asof_policy: borrow_fee_root.as_ref().map(|_| {
            "completed decision-session FEE_RATE close is a decimal annual borrow-cost rate used for next-session entry; it is not historical locate availability"
                .into()
        }),
        borrow_fee_coverage: borrow_fee_root.as_ref().map(|_| {
            let mapped = borrow_records
                .iter()
                .filter(|record| {
                    record
                        .stock
                        .isin
                        .as_deref()
                        .is_some_and(|isin| instruments_by_isin.contains_key(isin))
                })
                .count();
            format!(
                "{mapped}/{} IB fee histories map to the current/prior-snapshot Main Market universe; {} daily observations",
                instruments.len(),
                borrow_fees.len()
            )
        }),
    };
    serde_json::to_writer(&mut output, &manifest).map_err(|error| error.to_string())?;
    output.write_all(b"\n").map_err(|error| error.to_string())?;
    for row in &matrix.rows {
        serde_json::to_writer(&mut output, row).map_err(|error| error.to_string())?;
        output.write_all(b"\n").map_err(|error| error.to_string())?;
    }
    output.flush().map_err(|error| error.to_string())?;
    println!(
        "wrote {} final Rust Stockholm rows -> {}",
        matrix.rows.len(),
        path.display()
    );
    Ok(())
}

fn direction_matrix(args: &[String]) -> Result<(), String> {
    let index_dir = PathBuf::from(need(args, "--index-dir")?);
    let horizon = number(args, "--horizon-sessions", 20_usize)?;
    let mut inputs = Vec::new();
    let mut sources = std::collections::BTreeMap::new();
    for symbol in features_stockholm::DIRECTION_INDEX_SYMBOLS {
        let path = index_dir.join(format!("{symbol}.json"));
        let history = equity_data::load_benchmark(&path)?;
        if history.symbol != *symbol {
            return Err(format!(
                "{} contains index {} instead of {symbol}",
                path.display(),
                history.symbol
            ));
        }
        sources.insert(history.symbol.clone(), history.source.clone());
        inputs.push(features_stockholm::MarketIndexSeries {
            symbol: history.symbol,
            bars: history
                .bars
                .into_iter()
                .map(|bar| features_stockholm::MarketIndexBar {
                    date: bar.date,
                    start_value: bar.start_value,
                    end_value: bar.end_value,
                })
                .collect(),
        });
    }
    let matrix = features_stockholm::direction_training_matrix(
        &inputs,
        date(args, "--start")?,
        date(args, "--end")?,
        horizon,
    )?;
    let path = PathBuf::from(need(args, "--out")?);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let file = std::fs::File::create(&path).map_err(|error| error.to_string())?;
    let mut output = std::io::BufWriter::new(file);
    let manifest = DirectionMatrixManifest {
        kind: "stockholm_direction_training_manifest".into(),
        feature_set_version: features_stockholm::DIRECTION_FEATURE_SET_VERSION.into(),
        label_version: features_stockholm::direction_label_version(horizon)?,
        features: matrix.features,
        horizon_sessions: horizon,
        primary_index: "OMXSGI".into(),
        index_sources: sources,
        decision_policy:
            "all features use official index EOD values no later than the decision date; unavailable early histories have zero values plus explicit missing flags"
                .into(),
        label_policy:
            "OMXSGI next-session official SOD value to official SOD value after the declared holding horizon"
                .into(),
    };
    serde_json::to_writer(&mut output, &manifest).map_err(|error| error.to_string())?;
    output.write_all(b"\n").map_err(|error| error.to_string())?;
    for row in &matrix.rows {
        serde_json::to_writer(&mut output, row).map_err(|error| error.to_string())?;
        output.write_all(b"\n").map_err(|error| error.to_string())?;
    }
    output.flush().map_err(|error| error.to_string())?;
    println!(
        "wrote {} final Rust direction rows with {} inputs -> {}",
        matrix.rows.len(),
        manifest.features.len(),
        path.display()
    );
    Ok(())
}

fn load_matrix(path: &Path) -> Result<(MatrixManifest, Vec<TrainingRow>), String> {
    let file = std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut lines = std::io::BufReader::new(file).lines();
    let first = lines
        .next()
        .ok_or("empty matrix")?
        .map_err(|error| error.to_string())?;
    let manifest = serde_json::from_str(&first).map_err(|error| error.to_string())?;
    let rows = lines
        .filter_map(|line| match line {
            Ok(value) if value.trim().is_empty() => None,
            other => Some(other),
        })
        .map(|line| {
            let line = line.map_err(|error| error.to_string())?;
            serde_json::from_str(&line).map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((manifest, rows))
}

fn load_direction_matrix(
    path: &Path,
) -> Result<(DirectionMatrixManifest, Vec<DirectionTrainingRow>), String> {
    let file = std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut lines = std::io::BufReader::new(file).lines();
    let first = lines
        .next()
        .ok_or("empty direction matrix")?
        .map_err(|error| error.to_string())?;
    let manifest: DirectionMatrixManifest =
        serde_json::from_str(&first).map_err(|error| error.to_string())?;
    if manifest.kind != "stockholm_direction_training_manifest" {
        return Err("first row is not a Stockholm direction manifest".into());
    }
    let rows = lines
        .filter_map(|line| match line {
            Ok(value) if value.trim().is_empty() => None,
            other => Some(other),
        })
        .map(|line| {
            let line = line.map_err(|error| error.to_string())?;
            serde_json::from_str(&line).map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((manifest, rows))
}

fn run_direction_backtest(args: &[String]) -> Result<(), String> {
    let matrix_path = PathBuf::from(need(args, "--matrix")?);
    let (manifest, rows) = load_direction_matrix(&matrix_path)?;
    let model = stockholm_portfolio::DirectionModel::load(Path::new(&need(args, "--model")?))?;
    if manifest.features != model.features
        || manifest.feature_set_version != model.feature_set_version
        || manifest.label_version != model.label_version
        || manifest.horizon_sessions
            != features_stockholm::direction_label_horizon(&model.label_version)
                .ok_or("direction model has an invalid label horizon")?
    {
        return Err("direction matrix/model/runtime contracts differ".into());
    }
    let max_gross = number(args, "--max-gross", 1.0_f64)?;
    let result = stockholm_portfolio::direction_backtest(
        &model,
        &rows,
        date(args, "--start")?,
        date(args, "--end")?,
        portfolio_construction::DirectionConfig::baseline(max_gross)?,
    )?;
    let path = PathBuf::from(need(args, "--out")?);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut bytes = serde_json::to_vec_pretty(&result).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    std::fs::write(&path, bytes).map_err(|error| format!("{}: {error}", path.display()))?;
    println!(
        "direction model return {:.2}%, Sharpe {:.2}; fixed control {:.2}%, {:.2}; OMXSGI {:.2}%, {:.2} -> {}",
        result.trained_model.performance.total_return * 100.0,
        result.trained_model.performance.sharpe,
        result.fixed_trend_control.performance.total_return * 100.0,
        result.fixed_trend_control.performance.sharpe,
        result.omxsgi_long_only.total_return * 100.0,
        result.omxsgi_long_only.sharpe,
        path.display()
    );
    Ok(())
}

fn summarize_direction(args: &[String]) -> Result<(), String> {
    let paths = args
        .windows(2)
        .filter(|window| window[0] == "--fold")
        .map(|window| PathBuf::from(&window[1]))
        .collect::<Vec<_>>();
    if paths.is_empty() {
        return Err("at least one --fold is required".into());
    }
    let reports = paths
        .iter()
        .map(|path| {
            let bytes = std::fs::read(path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
            serde_json::from_slice(&bytes)
                .map_err(|error| format!("cannot parse {}: {error}", path.display()))
        })
        .collect::<Result<Vec<stockholm_portfolio::DirectionBacktestResult>, String>>()?;
    let summary = stockholm_portfolio::summarize_direction_folds(&reports)?;
    let path = PathBuf::from(need(args, "--out")?);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut bytes = serde_json::to_vec_pretty(&summary).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    std::fs::write(&path, bytes).map_err(|error| format!("{}: {error}", path.display()))?;
    println!(
        "{} folds: direction model {:.2}%, Sharpe {:.2}; fixed control {:.2}%, {:.2}; OMXSGI {:.2}%, {:.2} -> {}",
        summary.folds,
        summary.trained_model.performance.total_return * 100.0,
        summary.trained_model.performance.sharpe,
        summary.fixed_trend_control.performance.total_return * 100.0,
        summary.fixed_trend_control.performance.sharpe,
        summary.omxsgi_long_only.total_return * 100.0,
        summary.omxsgi_long_only.sharpe,
        path.display()
    );
    Ok(())
}

fn run_backtest(args: &[String]) -> Result<(), String> {
    let matrix_path = PathBuf::from(need(args, "--matrix")?);
    let (manifest, rows) = load_matrix(&matrix_path)?;
    let model = stockholm_portfolio::Model::load(Path::new(&need(args, "--model")?))?;
    let cadence = number(args, "--cadence-sessions", 5_usize)?;
    if manifest.features != model.features
        || manifest.feature_set_version != model.feature_set_version
        || model.label_version != manifest.label_version
        || model.survivorship_status != manifest.survivorship_status
    {
        return Err("matrix/model/runtime contracts differ".into());
    }
    if manifest.horizon_sessions != cadence {
        return Err(format!(
            "matrix label holds {} sessions but replay cadence is {cadence}",
            manifest.horizon_sessions
        ));
    }
    let multiple = number(args, "--cost-multiple", 1.0_f64)?;
    if !multiple.is_finite() || multiple <= 0.0 {
        return Err("--cost-multiple must be finite and positive".into());
    }
    let mut costs = stockholm_portfolio::CostConfig::default();
    // Stress market frictions without pretending IB's percentage commission
    // changes when spreads and impact widen.
    costs.market_friction_multiple = multiple;
    costs.round_trip_bps = costs.round_trip_commission_bps
        + multiple * (costs.round_trip_impact_bps + costs.fallback_spread_bps);
    costs.short_borrow_bps *= cadence as f64 / 5.0;
    let benchmark = get(args, "--benchmark")
        .map(|path| equity_data::load_benchmark(Path::new(&path)))
        .transpose()?;
    let sizing = get(args, "--sizing")
        .unwrap_or_else(|| "equal".into())
        .parse()?;
    let ranking = get(args, "--ranking")
        .unwrap_or_else(|| "edge".into())
        .parse()?;
    // Accept the old spelling while existing experiment commands migrate. It
    // is a maximum, never an instruction to spend the whole amount.
    let max_gross = if let Some(value) = get(args, "--max-gross") {
        value
            .parse::<f64>()
            .map_err(|error| format!("bad --max-gross: {error}"))?
    } else {
        number(args, "--target-gross", 1.0_f64)?
    };
    let allocation_budget = match get(args, "--target-net") {
        Some(value) => portfolio_construction::Budget::from_gross_net(
            max_gross,
            value
                .parse::<f64>()
                .map_err(|error| format!("bad --target-net: {error}"))?,
        )?,
        None => portfolio_construction::Budget::gross_only(max_gross)?,
    };
    let direction_overlay = args
        .iter()
        .any(|argument| argument == "--direction-overlay");
    if direction_overlay && get(args, "--target-net").is_some() {
        return Err("--target-net and --direction-overlay are mutually exclusive".into());
    }
    let direction_config = direction_overlay
        .then(|| portfolio_construction::DirectionConfig::baseline(max_gross))
        .transpose()?;
    let result = stockholm_portfolio::backtest(
        &model,
        &rows,
        &stockholm_portfolio::BacktestConfig {
            start: date(args, "--start")?,
            end: date(args, "--end")?,
            cadence_sessions: cadence,
            max_positions: number(args, "--max-positions", 20_usize)?,
            ranking,
            sizing,
            allocation_budget,
            position_weight: number(args, "--position-weight", 0.05_f64)?,
            min_position_weight: number(args, "--min-position-weight", 0.0_f64)?,
            reference_edge: number(args, "--reference-edge", 0.01_f64)?,
            reference_volatility: number(args, "--reference-volatility", 0.02_f64)?,
            direction_config,
            costs,
            benchmark,
        },
    )?;
    let path = PathBuf::from(need(args, "--out")?);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut bytes = serde_json::to_vec_pretty(&result).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    std::fs::write(&path, bytes).map_err(|error| error.to_string())?;
    println!(
        "{} periods, return {:.2}%, Sharpe {:.2}, max DD {:.2}% -> {}",
        result.metrics.periods,
        result.metrics.total_return * 100.0,
        result.metrics.sharpe,
        result.metrics.max_drawdown * 100.0,
        path.display()
    );
    if let Some(benchmark) = &result.benchmark {
        println!(
            "{} return {:.2}%, Sharpe {:.2}; portfolio minus benchmark {:.2}pp, beta {:.2}",
            benchmark.symbol,
            benchmark.total_return * 100.0,
            benchmark.sharpe,
            benchmark.portfolio_minus_benchmark_total_return * 100.0,
            benchmark.beta,
        );
    }
    if let Some(direction) = &result.direction_metrics {
        println!(
            "direction layer gross return {:.2}%, Sharpe {:.2}, mean G {:.2}, mean N {:+.2} ({})",
            direction.total_return * 100.0,
            direction.sharpe,
            direction.mean_budget_gross,
            direction.mean_budget_net,
            direction.cost_status,
        );
    }
    Ok(())
}

fn add_benchmark(args: &[String]) -> Result<(), String> {
    let report_path = PathBuf::from(need(args, "--report")?);
    let bytes = std::fs::read(&report_path)
        .map_err(|error| format!("{}: {error}", report_path.display()))?;
    let report = serde_json::from_slice(&bytes)
        .map_err(|error| format!("{}: {error}", report_path.display()))?;
    let benchmark = equity_data::load_benchmark(Path::new(&need(args, "--benchmark")?))?;
    let result = stockholm_portfolio::add_benchmark(report, &benchmark)?;
    let out = PathBuf::from(
        get(args, "--out").unwrap_or_else(|| report_path.to_string_lossy().into_owned()),
    );
    let mut bytes = serde_json::to_vec_pretty(&result).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    std::fs::write(&out, bytes).map_err(|error| format!("{}: {error}", out.display()))?;
    let comparison = result
        .benchmark
        .as_ref()
        .ok_or("benchmark attribution unexpectedly missing")?;
    println!(
        "{}: portfolio {:.2}%, {} {:.2}%, excess {:.2}pp",
        out.display(),
        result.metrics.total_return * 100.0,
        comparison.symbol,
        comparison.total_return * 100.0,
        comparison.portfolio_minus_benchmark_total_return * 100.0,
    );
    Ok(())
}

fn company_news_kind(category: &str) -> features_stockholm::CompanyNewsKind {
    match category {
        "Inside information" => features_stockholm::CompanyNewsKind::InsideInformation,
        "Changes in company's own shares" => features_stockholm::CompanyNewsKind::OwnShares,
        "Changes board/management/auditors" => features_stockholm::CompanyNewsKind::Management,
        "Prospectus/Announcement of Prospectus" => features_stockholm::CompanyNewsKind::Prospectus,
        "Major shareholder announcements" => features_stockholm::CompanyNewsKind::MajorShareholder,
        "Tender offer" => features_stockholm::CompanyNewsKind::TenderOffer,
        _ => features_stockholm::CompanyNewsKind::Other,
    }
}

fn bucket(value: DataBucket) -> UniverseBucket {
    match value {
        DataBucket::LargeCap => UniverseBucket::LargeCap,
        DataBucket::MidCap => UniverseBucket::MidCap,
        DataBucket::SmallCap => UniverseBucket::SmallCap,
        DataBucket::FirstNorthPremier => UniverseBucket::FirstNorthPremier,
        DataBucket::FirstNorth => UniverseBucket::FirstNorth,
    }
}

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let result = match args.first().map(String::as_str) {
        Some("collect") => collect(&args[1..]),
        Some("collect-benchmark") => collect_benchmark(&args[1..]),
        Some("collect-fi-net-shorts") => collect_fi_net_shorts(&args[1..]),
        Some("collect-skv-equity-history") => collect_skv_equity_history(&args[1..]),
        Some("collect-skv-listing-events") => collect_skv_listing_events(&args[1..]),
        Some("collect-fi-pdmr") => collect_fi_pdmr(&args[1..]),
        Some("collect-nasdaq-reports") => collect_nasdaq_reports(&args[1..]),
        Some("collect-nasdaq-report-messages") => collect_nasdaq_report_messages(&args[1..]),
        Some("collect-nasdaq-report-attachments") => collect_nasdaq_report_attachments(&args[1..]),
        Some("__extract-nasdaq-report-pdf") => extract_nasdaq_report_pdf_worker(&args[1..]),
        Some("audit-nasdaq-report-attachments") => audit_nasdaq_report_attachments(&args[1..]),
        Some("collect-nasdaq-company-news") => collect_nasdaq_company_news(&args[1..]),
        Some("collect-nasdaq-equity-notices") => collect_nasdaq_equity_notices(&args[1..]),
        Some("collect-nasdaq-market-history") => collect_nasdaq_market_history(&args[1..]),
        Some("collect-esef-annual") => collect_esef_annual(&args[1..]),
        Some("collect-riksbank-macro") => collect_riksbank_macro(&args[1..]),
        Some("collect-eodhd-delisted") => collect_eodhd_delisted(&args[1..]),
        Some("training-matrix") => matrix(&args[1..]),
        Some("direction-training-matrix") => direction_matrix(&args[1..]),
        Some("backtest") => run_backtest(&args[1..]),
        Some("direction-backtest") => run_direction_backtest(&args[1..]),
        Some("summarize-direction") => summarize_direction(&args[1..]),
        Some("add-benchmark") => add_benchmark(&args[1..]),
        _ => Err(USAGE.into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
