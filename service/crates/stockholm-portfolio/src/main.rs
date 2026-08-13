use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use equity_data::UniverseBucket as DataBucket;
use features_stockholm::{DirectionTrainingRow, InstrumentMeta, TrainingRow, UniverseBucket};
use sha2::{Digest, Sha256};
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

  collect-eodhd-fundamentals --data-root <dir> --universe <json>
                             --nasdaq-equity-notices <json>
                             [--pause-ms 100] [--limit 0]
      With EODHD_API_TOKEN, archive licensed quarterly statements using their
      filing dates for current Main Market and officially delisted securities.

  training-matrix --data-root <dir> --start YYYY-MM-DD --end YYYY-MM-DD
                  --out <jsonl> [--horizon-sessions 5] [--min-adv-sek 1000000]
                  [--skv-listing-history <json>]
                  [--feature-set baseline|baseline-global-risk|context|residual|residual-public-short|residual-pdmr|residual-pdmr-reports|residual-fundamentals|residual-quarterly-fundamentals|residual-pdmr-macro|residual-pdmr-microstructure|residual-pdmr-microstructure-borrow|residual-pdmr-microstructure-borrow-news|residual-pdmr-microstructure-borrow-news-global-risk|residual-pdmr-microstructure-borrow-news-report-text|residual-pdmr-microstructure-borrow-news-report-attachments]
                  [--fi-net-shorts <json>] [--fi-pdmr <json>]
                  [--nasdaq-reports <json>] [--esef-annual <json>]
                  [--eodhd-fundamentals <json>]
                  [--nasdaq-company-news <json>]
                  [--nasdaq-report-messages <json>]
                  [--nasdaq-report-attachments <json>]
                  [--riksbank-macro <json>]
                  [--nasdaq-market-history-root <dir>]
                  [--ib-fee-history-root <dir>]
                  [--cme-bars-root <dir>]
      Emit final Rust-owned features, missing flags, labels, and sample weights
      for Nasdaq Stockholm Large, Mid, and Small Cap only. When supplied,
      effective authority admission dates are enforced before cross-sectional
      ranks and labels are finalized. The global-risk feature set consumes
      ES/NQ/ZN/GC bars completed by the 17:30 Stockholm close and enters only
      at the following session's open.

  direction-training-matrix --index-dir <dir> --start YYYY-MM-DD --end YYYY-MM-DD
                            --out <jsonl> [--horizon-sessions 20]
                            [--cme-bars-root <dir>]
                            [--stockholm-close-cme-bars-root <dir>]
      Emit causal Rust-owned market-direction features and executable OMXSGI
      close-to-close absolute-return labels from official Nasdaq index
      histories.
      The Stockholm-close option adds ES/NQ/ZN/GC context known by 17:30 local
      time and is mutually exclusive with the prior-UTC-day ES option.

  filter-main-membership --matrix <jsonl> --universe <json>
                         --skv-listing-history <json> --out <jsonl>
      Conservatively remove rows before an issuer's effective-dated current
      Stockholm Main Market admission. This does not add delisted histories.

  diagnose-features --matrix <jsonl> --start YYYY-MM-DD --end YYYY-MM-DD
                    --out <json> [--cadence-sessions 20]
      Report date-local rank IC for every finalized Rust model input without
      changing features, labels, models, or portfolio decisions.

  fixed-momentum-backtest --matrix <jsonl> --start YYYY-MM-DD --end YYYY-MM-DD
                          --out <json> [--benchmark <json>]
                          [--cadence-sessions 20] [--max-positions 20]
                          [--position-weight 0.05] [--cost-multiple 1]
      Replay the predeclared adjusted-price 12-1 acceptance control as
      unconstrained directional, long-only, and long/short diagnostic arms.

  backtest --matrix <jsonl> --model <json> --start YYYY-MM-DD --end YYYY-MM-DD
           --out <json> [--benchmark <json>] [--cadence-sessions 5]
           [--max-positions 20 (directional) | 40 per sleeve (overlay)]
           [--rebalance-offset-sessions 0]
           [--retention-rank 20]
           [--max-sector-gross 0.25]
           [--ranking edge|edge_volatility]
           [--sizing equal|conviction|inverse_volatility|edge_volatility]
           [--max-gross 1 (directional) | 0.6 (overlay)] [--target-net <N>]
           [--direction-overlay]
           [--allocation-mode directional|overlay]
           [--core-weight 1] [--overlay-net-cap 0]
           [--core-tracking-cost-bps 10]
           [--market-forecast-matrix <jsonl> --market-forecast-model <json>
            --trained-direction-diagnostic]
           [--position-weight 0.05 (directional) | 0.015 (overlay)]
           [--min-position-weight 0]
           [--reference-edge 0.01] [--reference-volatility 0.02]
           [--aggregate-short-horizon-forecast]
           [--cost-multiple 1] [--bars-root <dir>]
           [--risk-free-annual 0.02]
      Replay one strictly-forward model fold with no long/short quota.
      --bars-root reads the same adjusted daily history the matrix was built
      from and marks NAV on every held session; without it a step reports its
      holding-period NAV alone. --risk-free-annual (a Riksbank policy-rate
      approximation until a SWESTR series is wired) is subtracted before
      Sharpe is computed, for both the portfolio and any benchmark.
      --direction-overlay sizes gross/net exposure off the fixed five-vote
      OMX trend state only; it is an optional drawdown guard, off by default.
      --allocation-mode overlay replaces the directional book with a fixed
      index core (--core-weight of the OMXSGI leg, charged only
      --core-tracking-cost-bps a year for an OMXS30 futures roll or ETF fee)
      plus a self-funding long/short overlay bounded by --max-gross and
      --overlay-net-cap. Its floor is the index minus that tracking cost, and
      the report attributes core and overlay separately, with the overlay's
      alpha t-stat taken against zero. It requires --benchmark and cannot be
      combined with --direction-overlay, --target-net or --max-sector-gross.
      Overlay mode also switches three defaults, unless the flag is passed
      explicitly (an explicit flag always wins, in either mode):
      --max-positions defaults to 40 PER SLEEVE (long and short are ranked
      and admitted against separate 40-name caps, so up to 80 names total,
      instead of directional's single 20-name cap shared by both directions);
      --position-weight defaults to 0.015 (vs 0.05); --max-gross defaults to
      0.6 (100% core + up to 30% long / 30% short overlay, vs 1.0). Rationale:
      the audit found the 20-name overlay book's phase-to-phase dispersion
      (-21%..+6%) was uncompensated concentration noise; more names at
      smaller size shrinks it roughly sqrt(2)-sqrt(3) while spending the same
      gross.
      Trained direction forecasts (--market-forecast-matrix/-model) are
      retired from every promotable configuration - every tested variant
      lost to controls on the available sample - and additionally require
      --trained-direction-diagnostic, a loud, explicit research/diagnostics
      opt-in.

  shadow-score --matrix <jsonl> --model <json> --out <jsonl>
              [--benchmark <json>]
              [--allocation-mode directional|overlay]
              [--core-weight 1] [--overlay-net-cap 0]
              [--core-tracking-cost-bps 10]
              [--max-gross 1 (directional) | 0.6 (overlay)]
              [--max-positions 20 (directional) | 40 per sleeve (overlay)]
              [--position-weight 0.05 (directional) | 0.015 (overlay)]
              [--min-position-weight 0] [--ranking edge|edge_volatility]
              [--sizing equal|conviction|inverse_volatility|edge_volatility]
              [--reference-edge 0.01] [--reference-volatility 0.02]
              [--cost-multiple 1]
      Task 16 shadow forward logging: score the single most recent decision
      date in --matrix with the frozen --model and the same overlay
      constructor --allocation-mode overlay uses in `backtest`, then append
      one JSON line to --out. No orders, no state mutation, deterministic --
      each call starts from flat, so there is no incumbent book to carry
      between sessions. --out is append-only: if its last recorded row's date
      is already >= the date being scored, the command refuses to write,
      rather than rewrite or duplicate a day's evidence. --allocation-mode
      overlay requires --benchmark, to price the index core on the scored
      date, and switches the same three defaults `backtest`'s overlay mode
      does (see `backtest` above). The matrix/model consistency gate is the
      same one `backtest` enforces: a model trained under an old feature-set
      version is refused against a matrix built under the binary's current
      one, and vice versa. Frozen models predate the current feature-set
      version, so shadow-score will refuse them against a freshly built
      matrix until Phase 1 retrains under the current version -- this is
      correct, not a bug; see docs/stockholm-portfolio-status.md.

  direction-backtest --matrix <jsonl> --model <json>
                     --start YYYY-MM-DD --end YYYY-MM-DD --out <json>
                     [--max-gross 1]
      Replay a trained direction fold and the fixed trend control on identical
      non-overlapping OMXSGI holding periods.

  summarize-direction --fold <json> [--fold <json> ...] --out <json>
      Recompute aggregate walk-forward direction metrics in Rust from frozen,
      non-overlapping fold steps.

  summarize-rebalance-phases --phase <json> [--phase <json> ...] --out <json>
                             [--risk-free-annual 0.02]
      Equal-weight every rebalance offset for one frozen model/fold. Exactly
      one report per offset from zero through cadence minus one is required.

  summarize-rebalance-phase-folds --fold <json> [--fold <json> ...] --out <json>
                                  [--risk-free-annual 0.02]
                                  [--target-sharpe-floor 1.0]
      Stitch non-overlapping equal-weight phase summaries into one auditable
      walk-forward result without treating overlapping phases as observations.
      `passed` requires `active_tstat >= 2.0` and
      `sharpe - 1.64*sharpe_se >= target_sharpe_floor`; the active t-stat reads
      null with a disclosed reason when the bot and benchmark series do not
      share one observation grid (daily bot vs holding-period benchmark, until
      Task 4 delivers a daily benchmark mark). --target-sharpe-floor is
      provisional pending Decision Point 1 in the remediation plan.

  add-benchmark --report <json> --benchmark <json> [--out <json>]
      Add exact-session benchmark attribution to an existing frozen Rust fold
      without rescoring or changing any portfolio decisions.
";

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct MatrixManifest {
    kind: String,
    feature_set_version: String,
    label_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label_coverage: Option<String>,
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
    quarterly_fundamental_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    quarterly_fundamental_asof_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    quarterly_fundamental_coverage: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    global_risk_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    global_risk_asof_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    global_risk_coverage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    membership_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    membership_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    membership_coverage: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    global_risk_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    global_risk_asof_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    global_risk_coverage: Option<String>,
}

fn get(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|value| value == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn repeated(args: &[String], name: &str) -> Vec<String> {
    args.iter()
        .enumerate()
        .filter_map(|(index, value)| (value == name).then(|| args.get(index + 1)).flatten())
        .cloned()
        .collect()
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

/// `true` when `--allocation-mode overlay` was passed. `backtest`'s overlay
/// mode widens the book (see `overlay_aware_number`) at the same total
/// gross, so several of its flags default differently from directional
/// mode's; this is checked before the `AllocationMode` enum itself is built
/// (that construction needs `max_gross`, one of the values this decides).
fn is_overlay_mode(args: &[String]) -> bool {
    matches!(get(args, "--allocation-mode").as_deref(), Some("overlay"))
}

/// Resolve a flag whose default depends on `--allocation-mode`. An explicit
/// flag always wins over either default, in either mode — this only chooses
/// which default `number` falls back to when the flag is absent.
fn overlay_aware_number<T: std::str::FromStr>(
    args: &[String],
    name: &str,
    overlay: bool,
    directional_default: T,
    overlay_default: T,
) -> Result<T, String>
where
    T::Err: std::fmt::Display,
{
    number(
        args,
        name,
        if overlay {
            overlay_default
        } else {
            directional_default
        },
    )
}

/// Whether a loaded model and the matrix manifest it is paired with agree
/// closely enough to replay together: identical declared feature-set
/// version, identical feature order, identical survivorship status.
///
/// `stockholm_portfolio::Model::load` no longer refuses a model merely for
/// declaring a `feature_set_version` this binary does not currently mint —
/// that version may predate a later feature-set correction (see its own
/// doc comment) and still be a legitimate frozen artifact. The safety
/// property this function enforces instead is CONSISTENCY between the two
/// artifacts, not currency with the binary: a model paired with a matrix
/// recorded under the identical (possibly old) version and feature list is
/// an internally consistent replay — the replay reads matrix rows from
/// disk and recomputes no features, so an old-on-old pair cannot silently
/// pick up the binary's current (different) feature semantics. A model and
/// matrix that disagree — either declares a different version, feature
/// order, or survivorship status than the other — are refused regardless
/// of whether either version is current, so a stale model can never be
/// quietly replayed against a freshly rebuilt matrix or vice versa.
fn model_agrees_with_matrix(
    model_features: &[String],
    model_feature_set_version: &str,
    model_survivorship_status: &str,
    manifest_features: &[String],
    manifest_feature_set_version: &str,
    manifest_survivorship_status: &str,
) -> bool {
    model_features == manifest_features
        && model_feature_set_version == manifest_feature_set_version
        && model_survivorship_status == manifest_survivorship_status
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

/// Prospective, publication-timestamped counterpart to `collect_fi_net_shorts`.
/// Run this on a schedule: it stamps each register row with the wall-clock
/// time it was first seen, building a causal history the position-date-keyed
/// register cannot give retroactively. No feature reads it yet.
fn collect_fi_net_short_observations(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let collection = equity_data::collect_fi_net_short_observations(&root)?;
    println!(
        "collected {} new FI net-short observations, {} total -> {}",
        collection.observations_added,
        collection.observations_total,
        collection.log_path.display(),
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
        "archived {}/{} Skatteverket company pages, {} listing rows, {} failures, {} rows with an unparsed admission date -> {}",
        collection.companies_archived,
        collection.companies_requested,
        collection.listing_rows,
        collection.failures,
        collection.admission_date_unparsed,
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

fn collect_eodhd_fundamentals(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let universe_path = PathBuf::from(need(args, "--universe")?);
    let universe = equity_data::load_instruments(&universe_path)?;
    let notices_path = PathBuf::from(need(args, "--nasdaq-equity-notices")?);
    let notices = equity_data::load_nasdaq_equity_notices(&notices_path)?;
    let token = std::env::var("EODHD_API_TOKEN")
        .map_err(|_| "EODHD_API_TOKEN is required for collect-eodhd-fundamentals".to_owned())?;
    let collection = equity_data::collect_eodhd_stockholm_fundamentals(
        &root,
        &universe_path,
        &universe,
        &notices_path,
        &notices,
        &token,
        number(args, "--pause-ms", 100_u64)?,
        number(args, "--limit", 0_usize)?,
    )?;
    println!(
        "matched {}/{} target ISINs against {} EODHD Stockholm symbols: {} histories, {} causal quarterly filings, {} failures -> {}",
        collection.matched_isins,
        collection.target_isins,
        collection.provider_symbols,
        collection.histories,
        collection.quarterly_filings,
        collection.failures,
        collection.dataset_path.display(),
    );
    Ok(())
}

fn load_stockholm_close_global_risk(
    root: &Path,
) -> Result<Vec<features_stockholm::GlobalRiskSeries>, String> {
    features_stockholm::STOCKHOLM_CLOSE_GLOBAL_RISK_SYMBOLS
        .iter()
        .map(|symbol| {
            let series = if *symbol == "NQ" {
                let nq = cme_data::load_daily_closes_at_stockholm_close(root, "NQ", 300)?;
                let mnq = cme_data::load_daily_closes_at_stockholm_close(root, "MNQ", 300)?;
                cme_data::stitch_daily_close_aliases("NQ", &[nq, mnq])?
            } else {
                cme_data::load_daily_closes_at_stockholm_close(root, symbol, 300)?
            };
            Ok(features_stockholm::GlobalRiskSeries {
                symbol: series.symbol,
                observations: series
                    .observations
                    .into_iter()
                    .map(|bar| features_stockholm::GlobalRiskBar {
                        date: bar.date,
                        close: bar.close,
                    })
                    .collect(),
            })
        })
        .collect()
}

fn matrix(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let (source, histories) = equity_data::load_stockholm(&root)?;
    let membership_history_path = get(args, "--skv-listing-history").map(PathBuf::from);
    let membership_history = membership_history_path
        .as_ref()
        .map(|path| equity_data::load_skv_listing_history(path))
        .transpose()?;
    let membership_admissions = membership_history
        .as_ref()
        .map(equity_data::skv_current_main_market_admission_dates)
        .unwrap_or_default();
    let main_instrument_count = histories
        .iter()
        .filter(|history| {
            matches!(
                history.instrument.bucket,
                DataBucket::LargeCap | DataBucket::MidCap | DataBucket::SmallCap
            )
        })
        .count();
    let eligible_from = histories
        .iter()
        .filter(|history| {
            matches!(
                history.instrument.bucket,
                DataBucket::LargeCap | DataBucket::MidCap | DataBucket::SmallCap
            )
        })
        .filter_map(|history| {
            membership_admissions
                .get(&equity_data::stockholm_security_issuer_key(
                    &history.instrument.name,
                ))
                .copied()
                .map(|date| (history.instrument.orderbook_id.clone(), date))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
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
            coverage.events_with_any_metric, coverage.deduplicated_events, coverage.by_feature,
        );
    }
    let esef_dataset = get(args, "--esef-annual")
        .map(|path| equity_data::load_esef_annual_filings(Path::new(&path)))
        .transpose()?;
    let mut matched_esef_filings = 0_usize;
    let mut matched_esef_instruments = std::collections::BTreeSet::new();
    let annual_fundamental_events = esef_dataset
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
    let quarterly_fundamental_dataset = get(args, "--eodhd-fundamentals")
        .map(|path| equity_data::load_eodhd_stockholm_fundamentals(Path::new(&path)))
        .transpose()?;
    let instruments_by_isin = histories
        .iter()
        .filter(|history| {
            matches!(
                history.instrument.bucket,
                DataBucket::LargeCap | DataBucket::MidCap | DataBucket::SmallCap
            )
        })
        .map(|history| {
            (
                history.instrument.isin.as_str(),
                history.instrument.orderbook_id.as_str(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut matched_quarterly_filings = 0_usize;
    let mut matched_quarterly_instruments = std::collections::BTreeSet::new();
    let quarterly_fundamental_events = quarterly_fundamental_dataset
        .as_ref()
        .map(|dataset| {
            dataset
                .histories
                .iter()
                .flat_map(|history| {
                    let Some(instrument_id) = instruments_by_isin.get(history.symbol.isin.as_str())
                    else {
                        return Vec::new();
                    };
                    matched_quarterly_instruments.insert((*instrument_id).to_owned());
                    matched_quarterly_filings += history.quarterly.len();
                    history
                        .quarterly
                        .iter()
                        .map(|filing| {
                            let values = &filing.values;
                            features_stockholm::AnnualFundamentalEvent {
                                instrument_id: (*instrument_id).to_owned(),
                                available_date: filing.available_date,
                                report_period_end: filing.report_period_end,
                                filing_key: filing.filing_key.clone(),
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
    if let Some(dataset) = &quarterly_fundamental_dataset {
        eprintln!(
            "EODHD quarterly-fundamental mapping: {matched_quarterly_filings} filings, {}/{} current Main Market securities ({} provider histories)",
            matched_quarterly_instruments.len(),
            instruments_by_isin.len(),
            dataset.histories.len(),
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
        "baseline-global-risk" => features_stockholm::FeatureSet::BaselineGlobalRisk,
        "context" => features_stockholm::FeatureSet::Context,
        "residual" => features_stockholm::FeatureSet::Residual,
        "residual-public-short" => features_stockholm::FeatureSet::ResidualPublicShort,
        "residual-pdmr" => features_stockholm::FeatureSet::ResidualPdmr,
        "residual-pdmr-reports" => features_stockholm::FeatureSet::ResidualPdmrReports,
        "residual-fundamentals" => features_stockholm::FeatureSet::ResidualFundamentals,
        "residual-quarterly-fundamentals" => {
            features_stockholm::FeatureSet::ResidualQuarterlyFundamentals
        }
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
        "residual-pdmr-microstructure-borrow-news-global-risk" => {
            features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNewsGlobalRisk
        }
        "residual-pdmr-microstructure-borrow-news-report-text" => {
            features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNewsReportText
        }
        "residual-pdmr-microstructure-borrow-news-report-attachments" => {
            features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNewsReportAttachments
        }
        other => return Err(format!("unknown Stockholm feature set {other:?}")),
    };
    let cme_bars_root = get(args, "--cme-bars-root").map(PathBuf::from);
    let global_risk_dataset = if feature_set == features_stockholm::FeatureSet::BaselineGlobalRisk {
        cme_bars_root
            .as_ref()
            .map(|path| cme_data::load_daily_closes(path, "ES", 300))
            .transpose()?
    } else {
        None
    };
    if feature_set == features_stockholm::FeatureSet::BaselineGlobalRisk
        && global_risk_dataset.is_none()
    {
        return Err("--cme-bars-root is required for baseline-global-risk features".into());
    }
    let global_risk = global_risk_dataset
        .as_ref()
        .map(|series| {
            series
                .observations
                .iter()
                .map(|bar| features_stockholm::GlobalRiskBar {
                    date: bar.date,
                    close: bar.close,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let stockholm_close_global_risk = if feature_set
        == features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNewsGlobalRisk
    {
        let root = cme_bars_root.as_ref().ok_or(
            "--cme-bars-root is required for residual-pdmr-microstructure-borrow-news-global-risk features",
        )?;
        load_stockholm_close_global_risk(root)?
    } else {
        Vec::new()
    };
    // residual-public-short no longer builds any public-short feature (see
    // features_stockholm::public_short_model_feature_names), so --fi-net-shorts
    // is accepted but never required here; the diagnostics-only feature set
    // that still uses it is not reachable from this flag at all.
    let public_short_dataset = get(args, "--fi-net-shorts")
        .map(|path| equity_data::load_fi_net_shorts(Path::new(&path)))
        .transpose()?;
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
            | features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNewsGlobalRisk
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
    if feature_set == features_stockholm::FeatureSet::ResidualQuarterlyFundamentals
        && quarterly_fundamental_dataset.is_none()
    {
        return Err(
            "--eodhd-fundamentals is required for residual-quarterly-fundamentals features".into(),
        );
    }
    let fundamental_events =
        if feature_set == features_stockholm::FeatureSet::ResidualQuarterlyFundamentals {
            &quarterly_fundamental_events
        } else {
            &annual_fundamental_events
        };
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
            | features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNewsGlobalRisk
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
            | features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNewsGlobalRisk
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
            | features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNewsGlobalRisk
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
    let matrix =
        features_stockholm::training_matrix_for_named_feature_set_with_all_sources_and_eligibility(
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
            fundamental_events,
            &macro_series,
            &microstructure,
            &borrow_fees,
            &company_news_events,
            &report_text_events,
            &global_risk,
            &stockholm_close_global_risk,
            &eligible_from,
        )?;
    let path = PathBuf::from(need(args, "--out")?);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let file = std::fs::File::create(&path).map_err(|error| error.to_string())?;
    let mut output = std::io::BufWriter::new(file);
    let labelled_rows = matrix
        .rows
        .iter()
        .filter(|row| row.target.is_some())
        .count();
    let manifest = MatrixManifest {
        kind: "stockholm_training_manifest".into(),
        feature_set_version: match feature_set {
            features_stockholm::FeatureSet::Baseline => {
                features_stockholm::BASELINE_FEATURE_SET_VERSION
            }
            features_stockholm::FeatureSet::BaselineGlobalRisk => {
                features_stockholm::BASELINE_GLOBAL_RISK_FEATURE_SET_VERSION
            }
            features_stockholm::FeatureSet::Context => features_stockholm::FEATURE_SET_VERSION,
            features_stockholm::FeatureSet::Residual => {
                features_stockholm::RESIDUAL_FEATURE_SET_VERSION
            }
            features_stockholm::FeatureSet::ResidualPublicShort => {
                features_stockholm::PUBLIC_SHORT_FEATURE_SET_VERSION
            }
            // Diagnostics-only; the `--feature-set` flag above never
            // produces this variant. Handled here only for exhaustiveness.
            features_stockholm::FeatureSet::DiagnosticsPublicShortLookahead => {
                features_stockholm::DIAGNOSTICS_PUBLIC_SHORT_LOOKAHEAD_FEATURE_SET_VERSION
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
            features_stockholm::FeatureSet::ResidualQuarterlyFundamentals => {
                features_stockholm::QUARTERLY_FUNDAMENTAL_FEATURE_SET_VERSION
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
            features_stockholm::FeatureSet::ResidualPdmrMicrostructureBorrowNewsGlobalRisk => {
                features_stockholm::PDMR_MICROSTRUCTURE_BORROW_NEWS_GLOBAL_RISK_FEATURE_SET_VERSION
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
        label_policy: Some(
            "Decision-date cross-section membership uses information available on the decision date alone. A member whose entry (t+1) or exit (t+1+H) session is missing keeps its features, cross-sectional ranks, sector medians and sample weight, and carries null targets. Each decision date's sample weight is one divided by every emitted row of that date, labelled or not, so weights describe the decision cross-section rather than the trainable subset. The label-space fields (market_target, relative_target, relative_rank_target, return_per_risk_target, relative_return_per_risk_target) are averages, centerings and ranks over the members whose outcome was observed, so they do move when a member's outcome ceases to exist; inventing a return for a security that stopped trading would be the alternative. That is a labelling limit, not a decision-time leak: no live decision reads a label."
                .into(),
        ),
        label_coverage: Some(format!(
            "{labelled_rows}/{} emitted rows carry an observed forward return; {} had no entry bar and could never have been entered; {} had an entry bar but no exit bar, so the security stopped trading inside the holding period and no terminal value exists for it",
            matrix.rows.len(),
            matrix
                .rows
                .iter()
                .filter(|row| row.entry_price.is_none())
                .count(),
            matrix
                .rows
                .iter()
                .filter(|row| row.entered_without_an_observed_exit())
                .count()
        )),
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
        quarterly_fundamental_source: quarterly_fundamental_dataset
            .as_ref()
            .map(|dataset| dataset.provider.clone()),
        quarterly_fundamental_asof_policy: quarterly_fundamental_dataset.as_ref().map(|_| {
            "licensed quarterly statements are ISIN-mapped in equity-data and become usable only after their provider filing_date; accounting period end never controls availability"
                .into()
        }),
        quarterly_fundamental_coverage: quarterly_fundamental_dataset.as_ref().map(|dataset| {
            format!(
                "{matched_quarterly_filings} causal quarterly filings mapped to {}/{} current Main Market securities from {} provider histories",
                matched_quarterly_instruments.len(),
                instruments_by_isin.len(),
                dataset.histories.len(),
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
        global_risk_source: global_risk_dataset
            .as_ref()
            .map(|series| {
                format!(
                    "archived CME {} {}-second Parquet bars in {}",
                    series.symbol,
                    series.interval_seconds,
                    series.source_root.display()
                )
            })
            .or_else(|| {
                (!stockholm_close_global_risk.is_empty()).then(|| {
                    format!(
                        "archived CME ES,NQ/MNQ,ZN,GC 300-second Parquet bars in {}",
                        cme_bars_root
                            .as_ref()
                            .expect("Stockholm-close series require a CME root")
                            .display()
                    )
                })
            }),
        global_risk_asof_policy: if global_risk_dataset.is_some() {
            Some(
                "Rust aggregates the last completed CME bar per UTC day and exposes only observations with a UTC date strictly before the Stockholm decision date"
                    .into(),
            )
        } else if !stockholm_close_global_risk.is_empty() {
            Some(
                "Rust uses the last CME bar completed by 17:30 Europe/Stockholm on the decision date; archived timestamps identify bar opens, timezone conversion follows historical CET/CEST, and equity entry is no earlier than the next session"
                    .into(),
            )
        } else {
            None
        },
        global_risk_coverage: global_risk_dataset
            .as_ref()
            .map(|series| {
                format!(
                    "{} source rows in {} partitions produce {} daily observations, {} through {}",
                    series.source_rows,
                    series.source_files,
                    series.observations.len(),
                    series
                        .observations
                        .first()
                        .map(|bar| bar.date.to_string())
                        .unwrap_or_else(|| "unknown".into()),
                    series
                        .observations
                        .last()
                        .map(|bar| bar.date.to_string())
                        .unwrap_or_else(|| "unknown".into())
                )
            })
            .or_else(|| {
                (!stockholm_close_global_risk.is_empty()).then(|| {
                    stockholm_close_global_risk
                        .iter()
                        .map(|series| {
                            format!(
                                "{}: {} observations, {} through {}",
                                series.symbol,
                                series.observations.len(),
                                series
                                    .observations
                                    .first()
                                    .map(|bar| bar.date.to_string())
                                    .unwrap_or_else(|| "unknown".into()),
                                series
                                    .observations
                                    .last()
                                    .map(|bar| bar.date.to_string())
                                    .unwrap_or_else(|| "unknown".into())
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("; ")
                })
            }),
        membership_source: membership_history_path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
        membership_policy: membership_history.as_ref().map(|_| {
            "Rows before an issuer's current continuous effective-dated Stockholm Main Market admission are removed in Rust before cross-sectional features, relative labels, and sample weights are finalized; unmatched issuers are retained, and no inactive history is synthesized."
                .into()
        }),
        membership_coverage: membership_history.as_ref().map(|_| {
            format!(
                "{}/{} current Large/Mid/Small Cap lines map by normalized issuer name to a Skatteverket effective Main Market admission",
                eligible_from.len(), main_instrument_count
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
        "wrote {} final Rust Stockholm rows ({} with an observed forward return) -> {}",
        matrix.rows.len(),
        labelled_rows,
        path.display()
    );
    Ok(())
}

fn filter_main_membership(args: &[String]) -> Result<(), String> {
    let matrix_path = PathBuf::from(need(args, "--matrix")?);
    let universe_path = PathBuf::from(need(args, "--universe")?);
    let history_path = PathBuf::from(need(args, "--skv-listing-history")?);
    let output_path = PathBuf::from(need(args, "--out")?);
    if matrix_path == output_path || output_path.exists() {
        return Err("membership-filter output must be a new path distinct from the input".into());
    }
    let instruments = equity_data::load_instruments(&universe_path)?;
    let history = equity_data::load_skv_listing_history(&history_path)?;
    let admissions = equity_data::skv_current_main_market_admission_dates(&history);
    let main_instruments = instruments
        .iter()
        .filter(|instrument| {
            matches!(
                instrument.bucket,
                DataBucket::LargeCap | DataBucket::MidCap | DataBucket::SmallCap
            )
        })
        .collect::<Vec<_>>();
    let eligible_from = main_instruments
        .iter()
        .filter_map(|instrument| {
            admissions
                .get(&equity_data::stockholm_security_issuer_key(
                    &instrument.name,
                ))
                .copied()
                .map(|date| (instrument.orderbook_id.clone(), date))
        })
        .collect::<std::collections::BTreeMap<_, _>>();

    let input = std::fs::File::open(&matrix_path)
        .map_err(|error| format!("{}: {error}", matrix_path.display()))?;
    let mut lines = std::io::BufReader::new(input).lines();
    let first = lines
        .next()
        .ok_or_else(|| "training matrix is empty".to_string())?
        .map_err(|error| error.to_string())?;
    let mut manifest: MatrixManifest =
        serde_json::from_str(&first).map_err(|error| error.to_string())?;
    if manifest.kind != "stockholm_training_manifest" {
        return Err("first row is not a Stockholm training manifest".into());
    }
    if manifest.membership_source.is_some() {
        return Err("training matrix already carries a membership filter".into());
    }
    manifest.membership_source = Some(history_path.to_string_lossy().into_owned());
    manifest.membership_policy = Some(
        "Rows before an issuer's current continuous effective-dated Stockholm Main Market admission are removed in Rust; unmatched issuers are retained, and no inactive history is synthesized. Existing feature and relative-label cross-sections are not recomputed."
            .into(),
    );
    manifest.membership_coverage = Some(format!(
        "{}/{} current Large/Mid/Small Cap lines map by normalized issuer name to a Skatteverket effective Main Market admission",
        eligible_from.len(),
        main_instruments.len()
    ));

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output_path)
        .map_err(|error| format!("{}: {error}", output_path.display()))?;
    let mut writer = std::io::BufWriter::new(output);
    serde_json::to_writer(&mut writer, &manifest).map_err(|error| error.to_string())?;
    writer.write_all(b"\n").map_err(|error| error.to_string())?;

    let mut current_date = None;
    let mut block = Vec::<TrainingRow>::new();
    let mut input_rows = 0_usize;
    let mut output_rows = 0_usize;
    let mut clipped_rows = 0_usize;
    let flush = |block: &mut Vec<TrainingRow>,
                 writer: &mut std::io::BufWriter<std::fs::File>,
                 output_rows: &mut usize|
     -> Result<(), String> {
        let weight = 1.0 / block.len() as f64;
        for mut row in block.drain(..) {
            row.sample_weight = weight;
            serde_json::to_writer(&mut *writer, &row).map_err(|error| error.to_string())?;
            writer.write_all(b"\n").map_err(|error| error.to_string())?;
            *output_rows += 1;
        }
        Ok(())
    };
    for line in lines {
        let line = line.map_err(|error| error.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        input_rows += 1;
        let row: TrainingRow = serde_json::from_str(&line).map_err(|error| error.to_string())?;
        if current_date.is_some_and(|date| row.date < date) {
            return Err("training matrix rows are not ordered by decision date".into());
        }
        if current_date.is_some_and(|date| row.date != date) && !block.is_empty() {
            flush(&mut block, &mut writer, &mut output_rows)?;
        }
        current_date = Some(row.date);
        if eligible_from
            .get(&row.instrument_id)
            .is_some_and(|admission| row.date < *admission)
        {
            clipped_rows += 1;
        } else {
            block.push(row);
        }
    }
    if !block.is_empty() {
        flush(&mut block, &mut writer, &mut output_rows)?;
    }
    writer.flush().map_err(|error| error.to_string())?;
    println!(
        "membership filter retained {output_rows}/{input_rows} rows and clipped {clipped_rows} pre-admission rows for {}/{} Main Market lines -> {}",
        eligible_from.len(),
        main_instruments.len(),
        output_path.display()
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
    let global_risk_root = get(args, "--cme-bars-root").map(PathBuf::from);
    let stockholm_close_global_risk_root =
        get(args, "--stockholm-close-cme-bars-root").map(PathBuf::from);
    if global_risk_root.is_some() && stockholm_close_global_risk_root.is_some() {
        return Err(
            "--cme-bars-root and --stockholm-close-cme-bars-root are mutually exclusive".into(),
        );
    }
    let global_risk_dataset = global_risk_root
        .as_ref()
        .map(|path| cme_data::load_daily_closes(path, "ES", 300))
        .transpose()?;
    let global_risk = global_risk_dataset
        .as_ref()
        .map(|series| {
            series
                .observations
                .iter()
                .map(|bar| features_stockholm::GlobalRiskBar {
                    date: bar.date,
                    close: bar.close,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let stockholm_close_global_risk = stockholm_close_global_risk_root
        .as_ref()
        .map(|root| load_stockholm_close_global_risk(root))
        .transpose()?
        .unwrap_or_default();
    let start = date(args, "--start")?;
    let end = date(args, "--end")?;
    let matrix = if stockholm_close_global_risk.is_empty() {
        features_stockholm::direction_training_matrix_with_global_risk(
            &inputs,
            start,
            end,
            horizon,
            &global_risk,
        )?
    } else {
        features_stockholm::direction_training_matrix_with_stockholm_close_global_risk(
            &inputs,
            start,
            end,
            horizon,
            &stockholm_close_global_risk,
        )?
    };
    let path = PathBuf::from(need(args, "--out")?);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let file = std::fs::File::create(&path).map_err(|error| error.to_string())?;
    let mut output = std::io::BufWriter::new(file);
    let manifest = DirectionMatrixManifest {
        kind: "stockholm_direction_training_manifest".into(),
        feature_set_version: if !stockholm_close_global_risk.is_empty() {
            features_stockholm::DIRECTION_STOCKHOLM_CLOSE_GLOBAL_RISK_FEATURE_SET_VERSION
        } else if global_risk_dataset.is_some() {
            features_stockholm::DIRECTION_GLOBAL_RISK_FEATURE_SET_VERSION
        } else {
            features_stockholm::DIRECTION_FEATURE_SET_VERSION
        }
        .into(),
        label_version: features_stockholm::direction_label_version(horizon)?,
        features: matrix.features,
        horizon_sessions: horizon,
        primary_index: "OMXSGI".into(),
        index_sources: sources,
        decision_policy:
            "all features use official index EOD values no later than the decision date; unavailable early histories have zero values plus explicit missing flags"
                .into(),
        label_policy:
            "OMXSGI official close of the first tradable session to the official close after the declared holding horizon; the archive's start-of-day value is the prior session's close plus a dividend adjustment and is never priced against"
                .into(),
        global_risk_source: global_risk_dataset
            .as_ref()
            .map(|series| {
                format!(
                    "archived CME {} {}-second Parquet bars in {}",
                    series.symbol,
                    series.interval_seconds,
                    series.source_root.display()
                )
            })
            .or_else(|| {
                stockholm_close_global_risk_root.as_ref().map(|root| {
                    format!(
                        "archived CME ES,NQ/MNQ,ZN,GC 300-second Parquet bars in {}",
                        root.display()
                    )
                })
            }),
        global_risk_asof_policy: if global_risk_dataset.is_some() {
            Some(
                "Rust aggregates the last completed CME bar per UTC day and exposes only observations with a UTC date strictly before the Stockholm decision date"
                    .into(),
            )
        } else if !stockholm_close_global_risk.is_empty() {
            Some(
                "Rust uses the last CME bar completed by 17:30 Europe/Stockholm on the decision date; archived timestamps identify bar opens, timezone conversion follows historical CET/CEST, and the OMXSGI label enters at the close of the next session"
                    .into(),
            )
        } else {
            None
        },
        global_risk_coverage: global_risk_dataset
            .as_ref()
            .map(|series| {
                format!(
                    "{} source rows in {} partitions produce {} daily observations, {} through {}",
                    series.source_rows,
                    series.source_files,
                    series.observations.len(),
                    series
                        .observations
                        .first()
                        .map(|bar| bar.date.to_string())
                        .unwrap_or_else(|| "unknown".into()),
                    series
                        .observations
                        .last()
                        .map(|bar| bar.date.to_string())
                        .unwrap_or_else(|| "unknown".into())
                )
            })
            .or_else(|| {
                (!stockholm_close_global_risk.is_empty()).then(|| {
                    stockholm_close_global_risk
                        .iter()
                        .map(|series| {
                            format!(
                                "{}: {} observations, {} through {}",
                                series.symbol,
                                series.observations.len(),
                                series
                                    .observations
                                    .first()
                                    .map(|bar| bar.date.to_string())
                                    .unwrap_or_else(|| "unknown".into()),
                                series
                                    .observations
                                    .last()
                                    .map(|bar| bar.date.to_string())
                                    .unwrap_or_else(|| "unknown".into())
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("; ")
                })
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
        "wrote {} final Rust direction rows with {} inputs -> {}",
        matrix.rows.len(),
        manifest.features.len(),
        path.display()
    );
    Ok(())
}

fn load_matrix(
    path: &Path,
    start: Date,
    end: Date,
) -> Result<(MatrixManifest, Vec<TrainingRow>), String> {
    let file = std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut lines = std::io::BufReader::new(file).lines();
    let first = lines
        .next()
        .ok_or("empty matrix")?
        .map_err(|error| error.to_string())?;
    let manifest = serde_json::from_str(&first).map_err(|error| error.to_string())?;
    let mut rows = Vec::new();
    for line in lines {
        let line = line.map_err(|error| error.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let row: TrainingRow = serde_json::from_str(&line).map_err(|error| error.to_string())?;
        if row.date >= start && row.date <= end {
            rows.push(row);
        }
    }
    Ok((manifest, rows))
}

fn diagnose_features(args: &[String]) -> Result<(), String> {
    let matrix_path = PathBuf::from(need(args, "--matrix")?);
    let start = date(args, "--start")?;
    let end = date(args, "--end")?;
    let cadence = number(args, "--cadence-sessions", 20_usize)?;
    if cadence == 0 {
        return Err("feature diagnostic cadence must be positive".into());
    }
    let (manifest, mut rows) = load_matrix(&matrix_path, start, end)?;
    let selected_dates = rows
        .iter()
        .map(|row| row.date)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .step_by(cadence)
        .collect::<std::collections::BTreeSet<_>>();
    rows.retain(|row| selected_dates.contains(&row.date));
    let diagnostics = features_stockholm::feature_target_diagnostics(&rows, &manifest.features)?;
    let report = serde_json::json!({
        "kind": "stockholm_feature_target_diagnostics",
        "matrix": matrix_path,
        "feature_set_version": manifest.feature_set_version,
        "label_version": manifest.label_version,
        "survivorship_status": manifest.survivorship_status,
        "start": start,
        "end": end,
        "cadence_sessions": cadence,
        "decision_dates": selected_dates.len(),
        "rows": rows.len(),
        "features": diagnostics,
        "disclosures": [
            "Each value is the mean of decision-date-local Spearman correlations between one finalized Rust input and the Rust forward relative-return ordering.",
            "This is retrospective drift diagnosis, not a fitted model or an independently held-out feature selection result."
        ]
    });
    let path = PathBuf::from(need(args, "--out")?);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut bytes = serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    std::fs::write(&path, bytes).map_err(|error| format!("{}: {error}", path.display()))?;
    println!(
        "wrote {} feature drift rows over {} matrix rows -> {}",
        manifest.features.len(),
        rows.len(),
        path.display()
    );
    Ok(())
}

fn run_fixed_momentum_backtest(args: &[String]) -> Result<(), String> {
    let matrix_path = PathBuf::from(need(args, "--matrix")?);
    let start = date(args, "--start")?;
    let end = date(args, "--end")?;
    let cadence = number(args, "--cadence-sessions", 20_usize)?;
    let max_positions = number(args, "--max-positions", 20_usize)?;
    let (manifest, rows) = load_matrix(&matrix_path, start, end)?;
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
    costs.market_friction_multiple = multiple;
    costs.round_trip_bps = costs.round_trip_commission_bps
        + multiple * (costs.round_trip_impact_bps + costs.fallback_spread_bps);
    // No borrow-fallback rescale here: short_borrow_annual_bps is an annual
    // rate and the library prorates it by cadence/252 itself from the
    // cadence_sessions it is already given below.
    let benchmark = get(args, "--benchmark")
        .map(|path| equity_data::load_benchmark(Path::new(&path)))
        .transpose()?;
    let result = stockholm_portfolio::fixed_momentum_backtest(
        &rows,
        &stockholm_portfolio::FixedMomentumConfig {
            start,
            end,
            cadence_sessions: cadence,
            max_positions,
            position_weight: number(args, "--position-weight", 0.05_f64)?,
            costs,
            benchmark,
            survivorship_status: manifest.survivorship_status,
        },
    )?;
    let path = PathBuf::from(need(args, "--out")?);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut bytes = serde_json::to_vec_pretty(&result).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    std::fs::write(&path, bytes).map_err(|error| format!("{}: {error}", path.display()))?;
    println!(
        "directional {:.2}%, Sharpe {:.2}; long-only {:.2}%, {:.2}; long/short {:.2}%, {:.2} -> {}",
        result.directional.metrics.total_return * 100.0,
        result.directional.metrics.sharpe,
        result.long_only.metrics.total_return * 100.0,
        result.long_only.metrics.sharpe,
        result.long_short_diagnostic.metrics.total_return * 100.0,
        result.long_short_diagnostic.metrics.sharpe,
        path.display()
    );
    Ok(())
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

/// Refuse a direction matrix or model built under a retired label convention.
///
/// The v1 label ran from the archive's start-of-day value, which is the prior
/// session's close, so it credited the overnight gap into the first held
/// session — a return no replay can execute. Mixing such a matrix into a replay
/// that now prices its index leg at closes would compare two different games,
/// so the mismatch is refused outright rather than silently reconciled.
fn current_direction_label(
    label_version: &str,
    horizon_sessions: usize,
    what: &str,
) -> Result<(), String> {
    let expected = features_stockholm::direction_label_version(horizon_sessions)?;
    if label_version != expected {
        return Err(format!(
            "direction {what} carries label {label_version}, but this build produces {expected}; regenerate the direction matrix and refit before replaying it"
        ));
    }
    Ok(())
}

fn run_direction_backtest(args: &[String]) -> Result<(), String> {
    let matrix_path = PathBuf::from(need(args, "--matrix")?);
    let (manifest, rows) = load_direction_matrix(&matrix_path)?;
    let model = stockholm_portfolio::DirectionModel::load(Path::new(&need(args, "--model")?))?;
    current_direction_label(&manifest.label_version, manifest.horizon_sessions, "matrix")?;
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
    let start = date(args, "--start")?;
    let end = date(args, "--end")?;
    let (manifest, rows) = load_matrix(&matrix_path, start, end)?;
    let model = stockholm_portfolio::Model::load(Path::new(&need(args, "--model")?))?;
    let cadence = number(args, "--cadence-sessions", 5_usize)?;
    if !model_agrees_with_matrix(
        &model.features,
        &model.feature_set_version,
        &model.survivorship_status,
        &manifest.features,
        &manifest.feature_set_version,
        &manifest.survivorship_status,
    ) {
        return Err("matrix/model/runtime contracts differ".into());
    }
    if manifest.horizon_sessions != cadence {
        return Err(format!(
            "matrix label holds {} sessions but replay cadence is {cadence}",
            manifest.horizon_sessions
        ));
    }
    let model_horizon = features_stockholm::label_horizon(&model.label_version)
        .ok_or_else(|| format!("unsupported model label version {:?}", model.label_version))?;
    let aggregate_short_horizon = args
        .iter()
        .any(|argument| argument == "--aggregate-short-horizon-forecast");
    let prediction_horizon_scale = if model.label_version == manifest.label_version {
        1.0
    } else if aggregate_short_horizon {
        if model_horizon >= cadence || cadence % model_horizon != 0 {
            return Err(format!(
                "model horizon {model_horizon} must be a proper divisor of holding horizon {cadence}"
            ));
        }
        cadence as f64 / model_horizon as f64
    } else {
        return Err(format!(
            "model label {:?} differs from matrix label {:?}; pass --aggregate-short-horizon-forecast only for the explicit shorter-horizon experiment",
            model.label_version, manifest.label_version
        ));
    };
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
    // No borrow-fallback rescale here: short_borrow_annual_bps is an annual
    // rate and the library prorates it by cadence/252 itself from the
    // cadence_sessions it is already given below.
    let benchmark = get(args, "--benchmark")
        .map(|path| equity_data::load_benchmark(Path::new(&path)))
        .transpose()?;
    let sizing = get(args, "--sizing")
        .unwrap_or_else(|| "equal".into())
        .parse()?;
    let ranking = get(args, "--ranking")
        .unwrap_or_else(|| "edge".into())
        .parse()?;
    // Overlay mode's defaults differ from directional's (see the
    // `--allocation-mode` block below for the rationale): widen the book at
    // the same total gross to shrink concentration noise. Peeking at the raw
    // flag here (before the enum is built, since building it needs
    // `max_gross`) only decides which default applies — an explicit flag,
    // in either mode, always wins over any default.
    let overlay_mode = is_overlay_mode(args);
    // Accept the old spelling while existing experiment commands migrate. It
    // is a maximum, never an instruction to spend the whole amount.
    let max_gross = if let Some(value) = get(args, "--max-gross") {
        value
            .parse::<f64>()
            .map_err(|error| format!("bad --max-gross: {error}"))?
    } else if let Some(value) = get(args, "--target-gross") {
        value
            .parse::<f64>()
            .map_err(|error| format!("bad --target-gross: {error}"))?
    } else if overlay_mode {
        // Overlay default: 100% core + up to 30% long / 30% short overlay.
        0.6
    } else {
        1.0
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
    // `directional` is the historical book: every krona of exposure comes from
    // candidates. `overlay` holds the index as a floor and lets the candidates
    // trade a self-funding long/short book on top of it.
    let allocation_mode = match get(args, "--allocation-mode").as_deref() {
        None | Some("directional") => stockholm_portfolio::AllocationMode::Directional,
        Some("overlay") => {
            if get(args, "--target-net").is_some() {
                return Err(
                    "--target-net sizes a directional book; the overlay's net is bounded by --overlay-net-cap"
                        .into(),
                );
            }
            stockholm_portfolio::AllocationMode::Overlay {
                budget: portfolio_construction::OverlayBudget {
                    core_weight: number(args, "--core-weight", 1.0_f64)?,
                    // The overlay spends the same gross budget flag the
                    // directional book does; in overlay mode it caps the
                    // overlay alone, on top of the core.
                    overlay_gross: max_gross,
                    // Zero by default: a self-funding overlay is not there to
                    // add market exposure the core already carries.
                    overlay_net_cap: number(args, "--overlay-net-cap", 0.0_f64)?,
                },
                core_tracking_cost_bps: number(args, "--core-tracking-cost-bps", 10.0_f64)?,
            }
        }
        Some(value) => {
            return Err(format!(
                "unsupported --allocation-mode {value:?}; expected directional or overlay"
            ));
        }
    };
    let direction_config = direction_overlay
        .then(|| portfolio_construction::DirectionConfig::baseline(max_gross))
        .transpose()?;
    let prediction_composition = match get(args, "--prediction-composition").as_deref() {
        None | Some("direct") => stockholm_portfolio::PredictionComposition::Direct,
        Some("cross-sectional-residual-plus-market") => {
            stockholm_portfolio::PredictionComposition::CrossSectionalResidualPlusMarket
        }
        Some(value) => {
            return Err(format!(
                "unsupported --prediction-composition {value:?}; expected direct or cross-sectional-residual-plus-market"
            ));
        }
    };
    if prediction_horizon_scale != 1.0
        && (model.reward != "absolute_return"
            || prediction_composition != stockholm_portfolio::PredictionComposition::Direct)
    {
        return Err(
            "short-horizon forecast aggregation currently requires a direct absolute-return model"
                .into(),
        );
    }
    let market_forecast_matrix = get(args, "--market-forecast-matrix");
    let market_forecast_model = get(args, "--market-forecast-model");
    // Trained direction is retired from every promotable configuration: it
    // failed every economic test on the ~250 independent 20-session market
    // outcomes available (22% directional accuracy, ~zero forecast
    // correlation, lost to both a fixed trend control and buy-and-hold
    // OMXSGI). --trained-direction-diagnostic exists only so an explicit
    // research/diagnostics replay can still ask for it; it must never be a
    // default a promotable config reaches by accident.
    let trained_direction_diagnostic = args
        .iter()
        .any(|argument| argument == "--trained-direction-diagnostic");
    let (market_return_forecasts, market_forecast_model_id) =
        match (market_forecast_matrix, market_forecast_model) {
            (None, None) => (None, None),
            (Some(matrix_path), Some(model_path)) => {
                if !direction_overlay {
                    return Err(
                        "market-return forecast composition requires --direction-overlay".into(),
                    );
                }
                if !trained_direction_diagnostic {
                    return Err(
                        "trained direction forecasts require --trained-direction-diagnostic: \
                         the model failed every economic test and is retired from promotable \
                         configurations; this flag exists only for explicit research/diagnostics \
                         replays"
                            .into(),
                    );
                }
                eprintln!(
                    "backtest: TRAINED DIRECTION forecast in use (--trained-direction-diagnostic) \
                     - retired from promotable configurations, every tested variant lost to a \
                     fixed trend control and to buy-and-hold OMXSGI. Diagnostics/research replay \
                     ONLY."
                );
                let (direction_manifest, direction_rows) =
                    load_direction_matrix(Path::new(&matrix_path))?;
                let direction_model =
                    stockholm_portfolio::DirectionModel::load(Path::new(&model_path))?;
                current_direction_label(
                    &direction_manifest.label_version,
                    direction_manifest.horizon_sessions,
                    "market forecast matrix",
                )?;
                if direction_manifest.features != direction_model.features
                    || direction_manifest.feature_set_version != direction_model.feature_set_version
                    || direction_manifest.label_version != direction_model.label_version
                    || direction_manifest.horizon_sessions != cadence
                {
                    return Err("market forecast matrix/model/runtime contracts differ".into());
                }
                if direction_model.trained_through != model.trained_through {
                    return Err(format!(
                        "stock and market models have different training cutoffs: {} versus {}",
                        model.trained_through, direction_model.trained_through
                    ));
                }
                let forecasts = direction_rows
                    .iter()
                    .filter(|row| row.date >= start && row.date <= end)
                    .map(|row| Ok((row.date, direction_model.predict(row)?.predicted_return)))
                    .collect::<Result<std::collections::BTreeMap<_, _>, String>>()?;
                if forecasts.is_empty() {
                    return Err("market forecast matrix has no rows in the replay window".into());
                }
                (Some(forecasts), Some(direction_model.model_id))
            }
            _ => return Err(
                "--market-forecast-matrix and --market-forecast-model must be supplied together"
                    .into(),
            ),
        };
    // Overlay's book is wider (40 names PER SLEEVE, i.e. up to 80 total) at a
    // smaller per-name weight than directional's combined 20-name cap: the
    // audit found the 20-name overlay's phase dispersion (-21%..+6%) was
    // uncompensated concentration noise, and more names at smaller size
    // shrinks it ~sqrt(2)-sqrt(3) while spending the same gross. An explicit
    // --max-positions/--position-weight always overrides this default,
    // whichever mode is chosen.
    let max_positions =
        overlay_aware_number(args, "--max-positions", overlay_mode, 20_usize, 40_usize)?;
    // Daily NAV marks need the adjusted closes between the executable entry and
    // exit opens; the matrix carries only those two prices per label.
    let mark_prices = match get(args, "--bars-root") {
        Some(root) => {
            let (_, histories) = equity_data::load_stockholm(Path::new(&root))?;
            let mut prices = stockholm_portfolio::MarkPrices::default();
            for history in &histories {
                prices.insert_history(
                    &history.instrument.orderbook_id,
                    history
                        .bars
                        .iter()
                        .map(|bar| (bar.date, bar.adjusted_close)),
                )?;
            }
            Some(prices)
        }
        None => None,
    };
    let result = stockholm_portfolio::backtest(
        &model,
        &rows,
        &stockholm_portfolio::BacktestConfig {
            start,
            end,
            cadence_sessions: cadence,
            rebalance_offset_sessions: number(args, "--rebalance-offset-sessions", 0_usize)?,
            model_horizon_sessions: model_horizon,
            prediction_horizon_scale,
            max_positions,
            retention_rank: number(args, "--retention-rank", max_positions)?,
            max_sector_gross: get(args, "--max-sector-gross")
                .map(|value| {
                    value
                        .parse::<f64>()
                        .map_err(|error| format!("bad --max-sector-gross: {error}"))
                })
                .transpose()?,
            ranking,
            sizing,
            allocation_budget,
            allocation_mode,
            position_weight: overlay_aware_number(
                args,
                "--position-weight",
                overlay_mode,
                0.05_f64,
                0.015_f64,
            )?,
            min_position_weight: number(args, "--min-position-weight", 0.0_f64)?,
            reference_edge: number(args, "--reference-edge", 0.01_f64)?,
            reference_volatility: number(args, "--reference-volatility", 0.02_f64)?,
            direction_config,
            prediction_composition,
            market_return_forecasts,
            market_forecast_model_id,
            costs,
            benchmark,
            mark_prices,
            risk_free_annual: number(args, "--risk-free-annual", 0.02_f64)?,
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
        "{} periods, return {:.2}%, Sharpe {:.2} ± {:.2} (rf {:.2}%), max DD {:.2}% -> {}",
        result.metrics.periods,
        result.metrics.total_return * 100.0,
        result.metrics.sharpe,
        result.metrics.sharpe_se,
        result.metrics.risk_free_annual * 100.0,
        result.metrics.max_drawdown * 100.0,
        path.display()
    );
    if let Some(benchmark) = &result.benchmark {
        println!(
            "{} return {:.2}%, Sharpe {:.2} ± {:.2}; portfolio minus benchmark {:.2}pp, beta {:.2}",
            benchmark.symbol,
            benchmark.total_return * 100.0,
            benchmark.sharpe,
            benchmark.sharpe_se,
            benchmark.portfolio_minus_benchmark_total_return * 100.0,
            benchmark.beta,
        );
    }
    if let Some(overlay) = &result.overlay_attribution {
        println!(
            "core {:.2}x contributed {:+.2}pp (tracking -{:.2}pp at {:.1} bp/yr), overlay contributed {:+.2}pp, overlay alpha t {}",
            overlay.core_weight,
            overlay.core_return * 100.0,
            overlay.core_tracking_cost * 100.0,
            overlay.core_tracking_cost_bps,
            overlay.overlay_return * 100.0,
            overlay
                .overlay_alpha_tstat
                .map(|value| format!("{value:.2}"))
                .unwrap_or_else(|| overlay
                    .overlay_alpha_tstat_status
                    .clone()
                    .unwrap_or_else(|| "unavailable".into())),
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

/// Read every row of a matrix, regardless of date, plus its manifest header.
/// Unlike `load_matrix` (used by `backtest`/`diagnose_features`, which take
/// an explicit `--start`/`--end` replay window), `shadow-score` scores
/// whichever date is most recent in the file, so it must see the whole file
/// to find that date.
fn read_matrix_all(path: &Path) -> Result<(MatrixManifest, Vec<TrainingRow>), String> {
    let file =
        std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut lines = std::io::BufReader::new(file).lines();
    let first = lines
        .next()
        .ok_or("empty matrix")?
        .map_err(|error| error.to_string())?;
    let manifest = serde_json::from_str(&first).map_err(|error| error.to_string())?;
    let mut rows = Vec::new();
    for line in lines {
        let line = line.map_err(|error| error.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        rows.push(serde_json::from_str(&line).map_err(|error| error.to_string())?);
    }
    Ok((manifest, rows))
}

/// Just enough of a shadow-log row to recover its `date`, deliberately not
/// `ShadowScoreRecord` itself and deliberately without `deny_unknown_fields`:
/// the append-only guard only ever needs the date, and coupling the tail
/// read to every field the record happens to carry today would let an
/// unrelated future field change wedge every later append.
#[derive(Debug, serde::Deserialize)]
struct ShadowLogTailDate {
    date: String,
}

/// A file's last line, found by seeking backward from EOF rather than
/// reading anything earlier in the file.
struct LastLine {
    /// Byte offset where this line's content starts.
    start: u64,
    /// Whether the file's last byte is the newline that terminates a
    /// completed append. `false` means the file ends mid-line -- exactly
    /// the shape a write that never finished (a crash, `kill -9`, a torn
    /// `O_APPEND` write past `PIPE_BUF`) leaves behind, so it is never
    /// treated as a real row below regardless of what it contains.
    well_terminated: bool,
    text: String,
}

/// Read only the last line of `path`: a handful of chunked backward seeks
/// bounded by that line's own length, never a scan of the whole file.
/// `Ok(None)` for a missing or empty file.
fn read_last_line(path: &Path) -> Result<Option<LastLine>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let mut file =
        std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let length = file
        .metadata()
        .map_err(|error| format!("{}: {error}", path.display()))?
        .len();
    if length == 0 {
        return Ok(None);
    }
    let mut last_byte = [0_u8; 1];
    file.seek(SeekFrom::Start(length - 1))
        .map_err(|error| format!("{}: {error}", path.display()))?;
    file.read_exact(&mut last_byte)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let well_terminated = last_byte[0] == b'\n';
    // The final newline itself, when present, is not part of the line's
    // content -- the search below looks for the PREVIOUS newline strictly
    // before it.
    let content_end = if well_terminated { length - 1 } else { length };

    const CHUNK: u64 = 8192;
    let mut cursor = content_end;
    let start = loop {
        let chunk_start = cursor.saturating_sub(CHUNK);
        let read_len = (cursor - chunk_start) as usize;
        let mut chunk = vec![0_u8; read_len];
        file.seek(SeekFrom::Start(chunk_start))
            .map_err(|error| format!("{}: {error}", path.display()))?;
        file.read_exact(&mut chunk)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if let Some(position) = chunk.iter().rposition(|byte| *byte == b'\n') {
            break chunk_start + position as u64 + 1;
        }
        if chunk_start == 0 {
            break 0;
        }
        cursor = chunk_start;
    };
    let mut content = vec![0_u8; (content_end - start) as usize];
    file.seek(SeekFrom::Start(start))
        .map_err(|error| format!("{}: {error}", path.display()))?;
    file.read_exact(&mut content)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(Some(LastLine {
        start,
        well_terminated,
        text: String::from_utf8_lossy(&content).into_owned(),
    }))
}

/// Best-effort filename hint for a quarantined line: the `date` its own
/// JSON claims, read as raw text without parsing (the line is, by
/// definition, one this function is never asked about unless it already
/// failed to parse), falling back to a content hash so two different torn
/// lines never collide on the same sidecar name.
fn tail_quarantine_hint(text: &str) -> String {
    let marker = "\"date\":\"";
    let candidate = text
        .find(marker)
        .and_then(|index| text.get(index + marker.len()..))
        .and_then(|rest| rest.get(..10))
        .filter(|value| value.as_bytes().get(4) == Some(&b'-') && value.as_bytes().get(7) == Some(&b'-'));
    match candidate {
        Some(value) => value.to_owned(),
        None => format!("{:x}", Sha256::digest(text.as_bytes()))[..12].to_owned(),
    }
}

/// A shadow log's last line is the residue of a write that never
/// completed, not evidence: move it to a sidecar file (with a header
/// explaining when and why), then repair the log by truncating exactly the
/// torn tail away. Every earlier line -- each already a completed,
/// newline-terminated append -- is untouched; this is the append-only log's
/// own self-repair, not an exception to it. Both writes are fsynced before
/// this returns, so the repair itself is as durable as an ordinary append.
fn quarantine_torn_tail(path: &Path, last: &LastLine, reason: &str) -> Result<PathBuf, String> {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "scores.jsonl".into());
    let sidecar = path.with_file_name(format!(
        "{file_name}.corrupt-{}",
        tail_quarantine_hint(&last.text)
    ));
    let header = format!(
        "# shadow-score quarantined a torn tail line from {} at {} UTC: {reason}\n",
        path.display(),
        humantime_now(),
    );
    let mut sidecar_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&sidecar)
        .map_err(|error| format!("{}: {error}", sidecar.display()))?;
    sidecar_file
        .write_all(header.as_bytes())
        .and_then(|()| sidecar_file.write_all(last.text.as_bytes()))
        .and_then(|()| sidecar_file.write_all(b"\n"))
        .map_err(|error| format!("{}: {error}", sidecar.display()))?;
    sidecar_file
        .sync_all()
        .map_err(|error| format!("{}: {error}", sidecar.display()))?;

    let repair_file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    repair_file
        .set_len(last.start)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    repair_file
        .sync_all()
        .map_err(|error| format!("{}: {error}", path.display()))?;

    eprintln!(
        "shadow-score: quarantined a torn tail line in {} -> {} ({reason})",
        path.display(),
        sidecar.display()
    );
    Ok(sidecar)
}

/// A timestamp for the quarantine header. Not parsed by anything -- purely
/// for a human reading the sidecar later -- so this deliberately avoids
/// pulling in a date-formatting dependency for one log line.
fn humantime_now() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format!("unix {seconds}")
}

/// The date of the last row already recorded in an append-only shadow log,
/// reading only that last line (never the whole file). If the last line is
/// torn -- missing the newline a completed append always writes, or present
/// but not parseable -- it is quarantined to a sidecar and the log is
/// repaired to its last complete line before this returns, so the
/// append-only guard is judged against real prior evidence, never a corrupt
/// fragment. Returns the resolved date (`None` for a missing, empty, or now
/// fully-quarantined log) plus any disclosure the caller should fold into
/// the record it is about to write.
fn last_shadow_log_date(path: &Path) -> Result<(Option<Date>, Vec<String>), String> {
    let Some(last) = read_last_line(path)? else {
        return Ok((None, Vec::new()));
    };
    let format = time::macros::format_description!("[year]-[month]-[day]");
    let parsed = last.well_terminated.then(|| {
        serde_json::from_str::<ShadowLogTailDate>(&last.text)
            .ok()
            .and_then(|value| Date::parse(&value.date, format).ok())
    });
    match parsed.flatten() {
        Some(date) => Ok((Some(date), Vec::new())),
        None => {
            let reason = if last.well_terminated {
                "last line is not parseable JSON with a date field"
            } else {
                "file does not end with the newline a completed append always writes"
            };
            let sidecar = quarantine_torn_tail(path, &last, reason)?;
            let disclosure = format!(
                "quarantined a torn tail line in {} to {} ({reason})",
                path.display(),
                sidecar.display()
            );
            // The log is now repaired to its last complete line (or empty).
            // One more resolution always suffices: truncation lands exactly
            // on a previous newline boundary, so the new tail is either
            // absent (file now empty) or itself already well-terminated.
            let (date, mut disclosures) = last_shadow_log_date(path)?;
            disclosures.insert(0, disclosure);
            Ok((date, disclosures))
        }
    }
}

/// Task 16 shadow forward logging: score the most recent decision date in
/// `--matrix` and append one line to the append-only `--out` log. See the
/// `shadow-score` usage block for the full contract.
fn run_shadow_score(args: &[String]) -> Result<(), String> {
    let matrix_path = PathBuf::from(need(args, "--matrix")?);
    let model = stockholm_portfolio::Model::load(Path::new(&need(args, "--model")?))?;
    let (manifest, rows) = read_matrix_all(&matrix_path)?;
    if !model_agrees_with_matrix(
        &model.features,
        &model.feature_set_version,
        &model.survivorship_status,
        &manifest.features,
        &manifest.feature_set_version,
        &manifest.survivorship_status,
    ) {
        return Err("matrix/model/runtime contracts differ".into());
    }
    let scored_date = rows
        .iter()
        .map(|row| row.date)
        .max()
        .ok_or("shadow-score matrix has no rows")?;

    let multiple = number(args, "--cost-multiple", 1.0_f64)?;
    if !multiple.is_finite() || multiple <= 0.0 {
        return Err("--cost-multiple must be finite and positive".into());
    }
    let mut costs = stockholm_portfolio::CostConfig::default();
    costs.market_friction_multiple = multiple;
    costs.round_trip_bps = costs.round_trip_commission_bps
        + multiple * (costs.round_trip_impact_bps + costs.fallback_spread_bps);

    let benchmark = get(args, "--benchmark")
        .map(|path| equity_data::load_benchmark(Path::new(&path)))
        .transpose()?;
    let benchmark_close = benchmark.as_ref().and_then(|history| {
        history
            .bars
            .iter()
            .find(|bar| bar.date == scored_date)
            .map(|bar| bar.end_value)
    });

    let sizing = get(args, "--sizing")
        .unwrap_or_else(|| "equal".into())
        .parse()?;
    let ranking = get(args, "--ranking")
        .unwrap_or_else(|| "edge".into())
        .parse()?;

    // Same overlay-mode wiring and defaults `run_backtest` uses (Task 14/15):
    // peeking at the raw flag only decides which default applies before the
    // `AllocationMode` enum is built; an explicit flag, in either mode,
    // always wins.
    let overlay_mode = is_overlay_mode(args);
    let max_gross = if let Some(value) = get(args, "--max-gross") {
        value
            .parse::<f64>()
            .map_err(|error| format!("bad --max-gross: {error}"))?
    } else if overlay_mode {
        0.6
    } else {
        1.0
    };
    let allocation_budget = portfolio_construction::Budget::gross_only(max_gross)?;
    let allocation_mode = match get(args, "--allocation-mode").as_deref() {
        None | Some("directional") => stockholm_portfolio::AllocationMode::Directional,
        Some("overlay") => stockholm_portfolio::AllocationMode::Overlay {
            budget: portfolio_construction::OverlayBudget {
                core_weight: number(args, "--core-weight", 1.0_f64)?,
                overlay_gross: max_gross,
                overlay_net_cap: number(args, "--overlay-net-cap", 0.0_f64)?,
            },
            core_tracking_cost_bps: number(args, "--core-tracking-cost-bps", 10.0_f64)?,
        },
        Some(value) => {
            return Err(format!(
                "unsupported --allocation-mode {value:?}; expected directional or overlay"
            ));
        }
    };
    let max_positions =
        overlay_aware_number(args, "--max-positions", overlay_mode, 20_usize, 40_usize)?;
    let position_weight = overlay_aware_number(
        args,
        "--position-weight",
        overlay_mode,
        0.05_f64,
        0.015_f64,
    )?;

    let mut record = stockholm_portfolio::shadow_score(
        &model,
        &rows,
        &stockholm_portfolio::ShadowScoreConfig {
            ranking,
            sizing,
            allocation_mode,
            allocation_budget,
            max_positions,
            position_weight,
            min_position_weight: number(args, "--min-position-weight", 0.0_f64)?,
            reference_edge: number(args, "--reference-edge", 0.01_f64)?,
            reference_volatility: number(args, "--reference-volatility", 0.02_f64)?,
            costs,
            benchmark_close,
        },
    )?;

    let path = PathBuf::from(need(args, "--out")?);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    // Append-only idempotency guard: a log that already holds a row on or
    // after the date being scored is left untouched. A rerun of the same
    // day, or an out-of-order matrix, must never rewrite or duplicate a
    // day's already-recorded evidence. `last_shadow_log_date` reads only the
    // log's last line; if that line turns out to be torn (an interrupted
    // write's residue, not a real prior row) it is quarantined and the log
    // repaired before the date below is judged, and the repair is disclosed
    // in the row this call is about to append.
    let (last_date, quarantine_disclosures) = last_shadow_log_date(&path)?;
    record.disclosures.extend(quarantine_disclosures);
    if let Some(last_date) = last_date {
        if record.date <= last_date {
            return Err(format!(
                "{} already holds a row for {last_date}, which is on or after the scored date \
                 {}; refusing to append out of order",
                path.display(),
                record.date
            ));
        }
    }
    let mut line = serde_json::to_string(&record).map_err(|error| error.to_string())?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    file.write_all(line.as_bytes())
        .map_err(|error| error.to_string())?;
    // Durability: "row logged" means on disk, not sitting in a page-cache
    // buffer an unattended cron job's crash can lose. `flush` is a formality
    // for an unbuffered `File`; `sync_all` is the fsync that matters.
    file.flush().map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    println!(
        "shadow-score {} ({} candidates, {} allocated) -> {}",
        record.date,
        record.candidates.len(),
        record.allocation.weights.len(),
        path.display()
    );
    Ok(())
}

fn summarize_rebalance_phases(args: &[String]) -> Result<(), String> {
    let paths = repeated(args, "--phase");
    if paths.is_empty() {
        return Err("at least one --phase report is required".into());
    }
    let reports = paths
        .iter()
        .map(|path| {
            let bytes = std::fs::read(path).map_err(|error| format!("{path}: {error}"))?;
            serde_json::from_slice(&bytes).map_err(|error| format!("{path}: {error}"))
        })
        .collect::<Result<Vec<stockholm_portfolio::BacktestResult>, String>>()?;
    let risk_free_annual = number(args, "--risk-free-annual", 0.02_f64)?;
    let summary = stockholm_portfolio::summarize_rebalance_phases(&reports, risk_free_annual)?;
    let path = PathBuf::from(need(args, "--out")?);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut bytes = serde_json::to_vec_pretty(&summary).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    std::fs::write(&path, bytes).map_err(|error| format!("{}: {error}", path.display()))?;
    println!(
        "{} equal-weight phases combined by {}, {} observations at {:.1}/yr: return {:.2}%, Sharpe {:.2} ± {:.2} (rf {:.2}%), max DD {:.2}% -> {}",
        summary.phase_count,
        summary.combination_method,
        summary.performance.periods,
        summary.performance.periods_per_year,
        summary.performance.total_return * 100.0,
        summary.performance.sharpe,
        summary.performance.sharpe_se,
        summary.performance.risk_free_annual * 100.0,
        summary.performance.max_drawdown * 100.0,
        path.display(),
    );
    match summary.active_tstat {
        Some(value) => println!("active t-stat vs benchmark: {value:.2}"),
        None => {
            if let Some(status) = &summary.active_tstat_status {
                println!("active t-stat vs benchmark: unavailable ({status})");
            }
        }
    }
    if summary.combination_method != stockholm_portfolio::CALENDAR_ALIGNED_DAILY_NAV {
        eprintln!(
            "warning: these phase reports carry no daily NAV marks, so overlapping holding windows were averaged by period index and the Sharpe above is overstated. Rerun the phases with --bars-root."
        );
    }
    if summary.benchmark_combination_method.as_deref()
        == Some(stockholm_portfolio::SINGLE_PHASE_INDEX_PATH)
    {
        eprintln!(
            "warning: these phase reports carry no daily benchmark marks, so the index leg stays on holding-period frequency, its Sharpe is not measured at the portfolio's frequency, and no active t-stat can be formed. Rerun the phases with --benchmark."
        );
    }
    Ok(())
}

fn summarize_rebalance_phase_folds(args: &[String]) -> Result<(), String> {
    let paths = repeated(args, "--fold");
    if paths.is_empty() {
        return Err("at least one --fold phase summary is required".into());
    }
    let folds = paths
        .iter()
        .map(|path| {
            let bytes = std::fs::read(path).map_err(|error| format!("{path}: {error}"))?;
            serde_json::from_slice(&bytes).map_err(|error| format!("{path}: {error}"))
        })
        .collect::<Result<Vec<stockholm_portfolio::RebalancePhaseSummary>, String>>()?;
    let risk_free_annual = number(args, "--risk-free-annual", 0.02_f64)?;
    let target_sharpe_floor = number(args, "--target-sharpe-floor", 1.0_f64)?;
    let summary = stockholm_portfolio::summarize_rebalance_phase_folds(
        &folds,
        risk_free_annual,
        target_sharpe_floor,
    )?;
    let path = PathBuf::from(need(args, "--out")?);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut bytes = serde_json::to_vec_pretty(&summary).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    std::fs::write(&path, bytes).map_err(|error| format!("{}: {error}", path.display()))?;
    println!(
        "{} folds, {} equal-weight phases combined by {}: return {:.2}%, Sharpe {:.2} ± {:.2} (rf {:.2}%, floor {:.2}), max DD {:.2}%, passed={} -> {}",
        summary.folds,
        summary.phase_count,
        summary.combination_method,
        summary.performance.total_return * 100.0,
        summary.performance.sharpe,
        summary.performance.sharpe_se,
        summary.performance.risk_free_annual * 100.0,
        summary.target_sharpe_floor,
        summary.performance.max_drawdown * 100.0,
        summary.passed,
        path.display(),
    );
    match summary.active_tstat {
        Some(value) => println!("active t-stat vs benchmark: {value:.2}"),
        None => {
            if let Some(status) = &summary.active_tstat_status {
                println!("active t-stat vs benchmark: unavailable ({status})");
            }
        }
    }
    if summary.combination_method != stockholm_portfolio::CALENDAR_ALIGNED_DAILY_NAV {
        eprintln!(
            "warning: these folds averaged their phases by period index; the Sharpe above is overstated. Rerun the phases with --bars-root."
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
        Some("collect-fi-net-short-observations") => collect_fi_net_short_observations(&args[1..]),
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
        Some("collect-eodhd-fundamentals") => collect_eodhd_fundamentals(&args[1..]),
        Some("training-matrix") => matrix(&args[1..]),
        Some("filter-main-membership") => filter_main_membership(&args[1..]),
        Some("diagnose-features") => diagnose_features(&args[1..]),
        Some("fixed-momentum-backtest") => run_fixed_momentum_backtest(&args[1..]),
        Some("direction-training-matrix") => direction_matrix(&args[1..]),
        Some("backtest") => run_backtest(&args[1..]),
        Some("shadow-score") => run_shadow_score(&args[1..]),
        Some("direction-backtest") => run_direction_backtest(&args[1..]),
        Some("summarize-direction") => summarize_direction(&args[1..]),
        Some("summarize-rebalance-phases") => summarize_rebalance_phases(&args[1..]),
        Some("summarize-rebalance-phase-folds") => summarize_rebalance_phase_folds(&args[1..]),
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

#[cfg(test)]
mod overlay_default_tests {
    use super::*;

    #[test]
    fn overlay_mode_default_applies_only_when_the_flag_is_absent() {
        let overlay_args = vec!["--allocation-mode".to_string(), "overlay".to_string()];
        let directional_args = vec!["--allocation-mode".to_string(), "directional".to_string()];
        let no_mode_args: Vec<String> = vec![];

        assert!(is_overlay_mode(&overlay_args));
        assert!(!is_overlay_mode(&directional_args));
        assert!(
            !is_overlay_mode(&no_mode_args),
            "directional is the default mode"
        );

        // Absent flag: each mode falls back to its own default.
        assert_eq!(
            overlay_aware_number(
                &overlay_args,
                "--max-positions",
                is_overlay_mode(&overlay_args),
                20_usize,
                40_usize
            )
            .unwrap(),
            40
        );
        assert_eq!(
            overlay_aware_number(
                &directional_args,
                "--max-positions",
                is_overlay_mode(&directional_args),
                20_usize,
                40_usize
            )
            .unwrap(),
            20
        );

        // An explicit flag always wins, in either mode.
        let overlay_explicit = vec![
            "--allocation-mode".to_string(),
            "overlay".to_string(),
            "--max-positions".to_string(),
            "7".to_string(),
        ];
        assert_eq!(
            overlay_aware_number(
                &overlay_explicit,
                "--max-positions",
                is_overlay_mode(&overlay_explicit),
                20_usize,
                40_usize
            )
            .unwrap(),
            7
        );
        let directional_explicit = vec![
            "--allocation-mode".to_string(),
            "directional".to_string(),
            "--max-positions".to_string(),
            "99".to_string(),
        ];
        assert_eq!(
            overlay_aware_number(
                &directional_explicit,
                "--max-positions",
                is_overlay_mode(&directional_explicit),
                20_usize,
                40_usize
            )
            .unwrap(),
            99
        );
    }

    #[test]
    fn overlay_position_weight_and_max_gross_defaults_match_the_brief() {
        let overlay_args = vec!["--allocation-mode".to_string(), "overlay".to_string()];
        let directional_args: Vec<String> = vec![];
        let overlay = is_overlay_mode(&overlay_args);
        let directional = is_overlay_mode(&directional_args);

        assert_eq!(
            overlay_aware_number(
                &overlay_args,
                "--position-weight",
                overlay,
                0.05_f64,
                0.015_f64
            )
            .unwrap(),
            0.015
        );
        assert_eq!(
            overlay_aware_number(
                &directional_args,
                "--position-weight",
                directional,
                0.05_f64,
                0.015_f64
            )
            .unwrap(),
            0.05
        );
        assert_eq!(
            overlay_aware_number(&overlay_args, "--max-gross", overlay, 1.0_f64, 0.6_f64).unwrap(),
            0.6
        );
        assert_eq!(
            overlay_aware_number(
                &directional_args,
                "--max-gross",
                directional,
                1.0_f64,
                0.6_f64
            )
            .unwrap(),
            1.0
        );

        // An explicit flag wins even when it's the same value as the other
        // mode's default, so the precedence is genuinely flag-first, not
        // just "does it differ from the default".
        let overlay_explicit_at_directional_default = vec![
            "--allocation-mode".to_string(),
            "overlay".to_string(),
            "--position-weight".to_string(),
            "0.05".to_string(),
        ];
        assert_eq!(
            overlay_aware_number(
                &overlay_explicit_at_directional_default,
                "--position-weight",
                is_overlay_mode(&overlay_explicit_at_directional_default),
                0.05_f64,
                0.015_f64
            )
            .unwrap(),
            0.05
        );
    }
}

#[cfg(test)]
mod matrix_model_agreement_tests {
    use super::*;

    fn names(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    /// Controller ruling: a model and matrix built under the identical old
    /// (pre-correction) version are internally consistent and must load —
    /// this is the case that unblocks Task 15 Step 2's re-baseline replay.
    #[test]
    fn old_model_and_matching_old_matrix_agree() {
        let old_version = "fs-rust-stockholm-1";
        let features = names(&["x_ret_1", "x_ret_5"]);
        assert!(model_agrees_with_matrix(
            &features,
            old_version,
            "SURVIVORSHIP_CONTAMINATED",
            &features,
            old_version,
            "SURVIVORSHIP_CONTAMINATED",
        ));
    }

    /// A stale model must not be silently replayed against a matrix rebuilt
    /// under the binary's current feature semantics.
    #[test]
    fn old_model_against_a_current_version_matrix_is_refused() {
        let features = names(&["x_ret_1", "x_ret_5"]);
        assert!(!model_agrees_with_matrix(
            &features,
            "fs-rust-stockholm-1",
            "SURVIVORSHIP_CONTAMINATED",
            &features,
            features_stockholm::BASELINE_FEATURE_SET_VERSION,
            "SURVIVORSHIP_CONTAMINATED",
        ));
    }

    /// A current model must not be silently replayed against a matrix still
    /// built under an old, pre-correction version.
    #[test]
    fn current_model_against_an_old_matrix_is_refused() {
        let features = names(&["x_ret_1", "x_ret_5"]);
        assert!(!model_agrees_with_matrix(
            &features,
            features_stockholm::BASELINE_FEATURE_SET_VERSION,
            "SURVIVORSHIP_CONTAMINATED",
            &features,
            "fs-rust-stockholm-1",
            "SURVIVORSHIP_CONTAMINATED",
        ));
    }

    /// Same declared version on both sides is not sufficient on its own —
    /// the feature order must agree too (a version string alone does not
    /// prove the matrix was built with today's ordering discipline).
    #[test]
    fn matching_version_with_different_feature_order_is_refused() {
        assert!(!model_agrees_with_matrix(
            &names(&["x_ret_1", "x_ret_5"]),
            "fs-rust-stockholm-1",
            "SURVIVORSHIP_CONTAMINATED",
            &names(&["x_ret_5", "x_ret_1"]),
            "fs-rust-stockholm-1",
            "SURVIVORSHIP_CONTAMINATED",
        ));
    }
}

/// Task 16: `shadow-score` CLI-level tests. These exercise `run_shadow_score`
/// end to end against tiny fixture files on disk — the append-only guard is
/// a filesystem contract (what is already on disk decides whether a write is
/// allowed), so it is tested through the CLI entry point rather than through
/// `stockholm_portfolio::shadow_score`, which is pure and never touches disk.
#[cfg(test)]
mod shadow_score_cli_tests {
    use super::*;

    /// A feature-set version this binary does not currently mint, so
    /// `Model::load`'s exact-feature-list check does not apply and the
    /// fixture only needs to agree with itself (old-on-old, exactly the
    /// consistency `model_agrees_with_matrix` requires).
    const FIXTURE_FEATURE_SET_VERSION: &str = "fs-shadow-score-test-1";

    fn unique_temp_dir() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let mut path = std::env::temp_dir();
        path.push(format!(
            "stockholm-shadow-score-test-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn write_fixture_model(path: &Path) {
        let model = serde_json::json!({
            "format_version": stockholm_portfolio::FORMAT_VERSION,
            "model_version": stockholm_portfolio::MODEL_VERSION,
            "feature_set_version": FIXTURE_FEATURE_SET_VERSION,
            "label_version": "forward-adjusted-open-5-v1",
            "trained_through": "2023-12-31",
            "trained_at": "fixture",
            "n_rows": 1,
            "n_dates": 1,
            "features": ["x_ret_1"],
            "survivorship_status": "SURVIVORSHIP_CONTAMINATED",
            "model_family": "lightgbm",
            "reward": "absolute_return",
            "objective": "l2",
            "ensemble_seeds": 1,
            "tree_info": [{
                "tree_index": 0,
                "num_leaves": 1,
                "num_cat": 0,
                "shrinkage": 1.0,
                "tree_structure": { "leaf_value": 0.05 },
            }],
        });
        std::fs::write(path, serde_json::to_vec_pretty(&model).unwrap()).unwrap();
    }

    /// One row per date in `dates`, on a distinct instrument so a
    /// multi-date fixture never accidentally collapses to one candidate.
    fn write_fixture_matrix(path: &Path, dates: &[&str]) {
        let manifest = serde_json::json!({
            "kind": "stockholm_training_manifest",
            "feature_set_version": FIXTURE_FEATURE_SET_VERSION,
            "label_version": "forward-adjusted-open-5-v1",
            "features": ["x_ret_1"],
            "horizon_sessions": 5,
            "min_adv20_sek": 1_000_000.0,
            "survivorship_status": "SURVIVORSHIP_CONTAMINATED",
            "universe_source": "fixture",
            "history_source": "fixture",
        });
        let mut file = std::fs::File::create(path).unwrap();
        writeln!(file, "{}", serde_json::to_string(&manifest).unwrap()).unwrap();
        for (index, date) in dates.iter().enumerate() {
            let row = serde_json::json!({
                "date": date,
                "instrument_id": format!("TX{index}"),
                "symbol": format!("SYM{index}"),
                "isin": "SE0000000000",
                "sector": "Industrials",
                "bucket": "large_cap",
                "target": null,
                "entry_price": null,
                "exit_price": null,
                "adv20_sek": 10_000_000.0,
                "vol60": 0.02,
                "sample_weight": 1.0,
                "features": { "x_ret_1": 0.1 },
            });
            writeln!(file, "{}", serde_json::to_string(&row).unwrap()).unwrap();
        }
    }

    fn read_lines(path: &Path) -> Vec<String> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// A single scoring call must emit exactly one parseable JSON line
    /// carrying the fields the shadow-forward-logging job promises.
    #[test]
    fn shadow_score_emits_one_parseable_line_with_the_promised_fields() {
        let dir = unique_temp_dir();
        let model_path = dir.join("model.json");
        let matrix_path = dir.join("matrix.jsonl");
        let out_path = dir.join("scores.jsonl");
        write_fixture_model(&model_path);
        write_fixture_matrix(&matrix_path, &["2024-01-02"]);

        run_shadow_score(&[
            "--matrix".into(),
            matrix_path.to_string_lossy().into_owned(),
            "--model".into(),
            model_path.to_string_lossy().into_owned(),
            "--out".into(),
            out_path.to_string_lossy().into_owned(),
        ])
        .expect("shadow-score should succeed against a consistent old-on-old fixture");

        let lines = read_lines(&out_path);
        assert_eq!(lines.len(), 1);
        let record: stockholm_portfolio::ShadowScoreRecord =
            serde_json::from_str(&lines[0]).expect("line must be parseable JSON");
        assert_eq!(record.date.to_string(), "2024-01-02");
        assert_eq!(record.feature_set_version, FIXTURE_FEATURE_SET_VERSION);
        assert_eq!(record.candidates.len(), 1);
        assert_eq!(record.candidates[0].id, "TX0");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Running the job on two later, increasing sessions appends two lines
    /// and never rewrites the first.
    #[test]
    fn shadow_score_run_twice_on_different_dates_appends_two_lines() {
        let dir = unique_temp_dir();
        let model_path = dir.join("model.json");
        let out_path = dir.join("scores.jsonl");
        write_fixture_model(&model_path);

        let day1 = dir.join("matrix-day1.jsonl");
        write_fixture_matrix(&day1, &["2024-01-02"]);
        run_shadow_score(&[
            "--matrix".into(),
            day1.to_string_lossy().into_owned(),
            "--model".into(),
            model_path.to_string_lossy().into_owned(),
            "--out".into(),
            out_path.to_string_lossy().into_owned(),
        ])
        .unwrap();

        let day2 = dir.join("matrix-day2.jsonl");
        write_fixture_matrix(&day2, &["2024-01-03"]);
        run_shadow_score(&[
            "--matrix".into(),
            day2.to_string_lossy().into_owned(),
            "--model".into(),
            model_path.to_string_lossy().into_owned(),
            "--out".into(),
            out_path.to_string_lossy().into_owned(),
        ])
        .unwrap();

        let lines = read_lines(&out_path);
        assert_eq!(lines.len(), 2, "two runs on two dates must append two lines");
        let first: stockholm_portfolio::ShadowScoreRecord =
            serde_json::from_str(&lines[0]).unwrap();
        let second: stockholm_portfolio::ShadowScoreRecord =
            serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(first.date.to_string(), "2024-01-02");
        assert_eq!(second.date.to_string(), "2024-01-03");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The append-only guard: a matrix whose most recent date does not
    /// strictly advance past the log's last row must be refused, and the
    /// log must be left exactly as it was.
    #[test]
    fn shadow_score_refuses_to_append_a_date_not_after_the_last_logged_row() {
        let dir = unique_temp_dir();
        let model_path = dir.join("model.json");
        let out_path = dir.join("scores.jsonl");
        write_fixture_model(&model_path);

        let day2 = dir.join("matrix-day2.jsonl");
        write_fixture_matrix(&day2, &["2024-01-03"]);
        run_shadow_score(&[
            "--matrix".into(),
            day2.to_string_lossy().into_owned(),
            "--model".into(),
            model_path.to_string_lossy().into_owned(),
            "--out".into(),
            out_path.to_string_lossy().into_owned(),
        ])
        .unwrap();
        let before = read_lines(&out_path);

        // Same date again: a rerun of an already-logged session.
        let rerun = run_shadow_score(&[
            "--matrix".into(),
            day2.to_string_lossy().into_owned(),
            "--model".into(),
            model_path.to_string_lossy().into_owned(),
            "--out".into(),
            out_path.to_string_lossy().into_owned(),
        ]);
        assert!(rerun.is_err(), "rerunning the same date must be refused");

        // An earlier date: an out-of-order matrix.
        let day1 = dir.join("matrix-day1.jsonl");
        write_fixture_matrix(&day1, &["2024-01-02"]);
        let out_of_order = run_shadow_score(&[
            "--matrix".into(),
            day1.to_string_lossy().into_owned(),
            "--model".into(),
            model_path.to_string_lossy().into_owned(),
            "--out".into(),
            out_path.to_string_lossy().into_owned(),
        ]);
        assert!(
            out_of_order.is_err(),
            "an earlier date than the log's last row must be refused"
        );

        let after = read_lines(&out_path);
        assert_eq!(before, after, "a refused append must leave the log untouched");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Reading the log's last line must never require successfully
    /// deserializing anything earlier: a huge, schema-incompatible earlier
    /// line (extra fields no `ShadowScoreRecord` has -- the old
    /// `deny_unknown_fields`-on-every-line implementation would have
    /// refused to even read past it) must not stop today's run, and its
    /// made-up future date must never be consulted by the guard.
    #[test]
    fn shadow_score_guard_reads_only_the_last_line_of_the_log() {
        let dir = unique_temp_dir();
        let model_path = dir.join("model.json");
        let out_path = dir.join("scores.jsonl");
        write_fixture_model(&model_path);

        // Line 1: huge, schema-incompatible, and claims a date far in the
        // future. If the guard read (or choked on) this line, either a
        // spurious refusal or a hard failure would show up below.
        let bogus_line = serde_json::json!({
            "date": "2099-01-01",
            "not_a_shadow_score_record_field": "x".repeat(4096),
            "another_unknown_field": (0..200).collect::<Vec<i64>>(),
        });
        // Line 2: the log's real last line, an earlier, ordinary date.
        let real_line = serde_json::json!({
            "date": "2024-01-01",
            "model_id": "fixture",
            "feature_set_version": FIXTURE_FEATURE_SET_VERSION,
            "survivorship_status": "SURVIVORSHIP_CONTAMINATED",
            "candidates": [],
            "allocation": { "weights": {} },
            "modeled_cost": 0.0,
            "disclosures": [],
        });
        {
            let mut file = std::fs::File::create(&out_path).unwrap();
            writeln!(file, "{}", serde_json::to_string(&bogus_line).unwrap()).unwrap();
            writeln!(file, "{}", serde_json::to_string(&real_line).unwrap()).unwrap();
        }

        let matrix_path = dir.join("matrix.jsonl");
        write_fixture_matrix(&matrix_path, &["2024-01-02"]);

        run_shadow_score(&[
            "--matrix".into(),
            matrix_path.to_string_lossy().into_owned(),
            "--model".into(),
            model_path.to_string_lossy().into_owned(),
            "--out".into(),
            out_path.to_string_lossy().into_owned(),
        ])
        .expect(
            "an odd, schema-incompatible earlier line must not stop the run, and its \
             made-up future date must never be consulted",
        );

        let lines = read_lines(&out_path);
        assert_eq!(
            lines.len(),
            3,
            "the two untouched prior lines plus the new row"
        );
        let appended: stockholm_portfolio::ShadowScoreRecord =
            serde_json::from_str(&lines[2]).unwrap();
        assert_eq!(
            appended.date.to_string(),
            "2024-01-02",
            "2024-01-02 clears the real last line's 2024-01-01; a run refused by the \
             bogus line's fictitious 2099-01-01 would prove the guard read more than \
             the last line"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A torn tail line -- no trailing newline, the residue of a write that
    /// never finished -- must be quarantined to a sidecar and the log
    /// repaired to its last complete line, not treated as evidence and not
    /// left wedging every future append.
    #[test]
    fn shadow_score_quarantines_a_torn_tail_line_and_repairs_the_log() {
        let dir = unique_temp_dir();
        let model_path = dir.join("model.json");
        let out_path = dir.join("scores.jsonl");
        write_fixture_model(&model_path);

        // A genuine, complete first row, produced by a real run so its shape
        // is exactly whatever ShadowScoreRecord actually serializes today.
        let day1 = dir.join("matrix-day1.jsonl");
        write_fixture_matrix(&day1, &["2024-01-01"]);
        run_shadow_score(&[
            "--matrix".into(),
            day1.to_string_lossy().into_owned(),
            "--model".into(),
            model_path.to_string_lossy().into_owned(),
            "--out".into(),
            out_path.to_string_lossy().into_owned(),
        ])
        .unwrap();
        let before_corruption = read_lines(&out_path);
        assert_eq!(before_corruption.len(), 1);

        // Simulate a write killed mid-append: bytes on disk, no trailing
        // newline, not a complete JSON row.
        {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&out_path)
                .unwrap();
            file.write_all(br#"{"date":"2024-01-05","model_id":"tor"#)
                .unwrap();
        }

        let day2 = dir.join("matrix-day2.jsonl");
        write_fixture_matrix(&day2, &["2024-01-10"]);
        run_shadow_score(&[
            "--matrix".into(),
            day2.to_string_lossy().into_owned(),
            "--model".into(),
            model_path.to_string_lossy().into_owned(),
            "--out".into(),
            out_path.to_string_lossy().into_owned(),
        ])
        .expect("a torn tail line must be quarantined and repaired, not wedge the append");

        let lines = read_lines(&out_path);
        assert_eq!(
            lines.len(),
            2,
            "the torn line gone, the first real row kept, and the new row appended"
        );
        assert_eq!(
            lines[0], before_corruption[0],
            "the intact first row must be byte-for-byte unchanged"
        );
        let appended: stockholm_portfolio::ShadowScoreRecord =
            serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(appended.date.to_string(), "2024-01-10");
        assert!(
            appended
                .disclosures
                .iter()
                .any(|line| line.contains("quarantined") && line.contains("torn tail line")),
            "the appended row must disclose the repair: {:?}",
            appended.disclosures
        );

        // A sidecar file was created next to the log, named after the torn
        // line's own claimed date, holding a header plus the torn content --
        // this is the persisted form of the same warning `quarantine_torn_tail`
        // also writes to stderr.
        let sidecar = dir.join("scores.jsonl.corrupt-2024-01-05");
        let sidecar_contents =
            std::fs::read_to_string(&sidecar).expect("a sidecar quarantine file must exist");
        assert!(sidecar_contents.contains("quarantined"));
        assert!(sidecar_contents.contains(r#"{"date":"2024-01-05","model_id":"tor"#));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
